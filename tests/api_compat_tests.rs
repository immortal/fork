//! Behavioral compatibility guard for the pre-broker (0.8.0) public surface.
//!
//! `cargo semver-checks` locks the *signatures* of the legacy API, but not its
//! *observable behavior*. The process-broker work relocated the internal
//! `waitpid_reaped` helpers into a new module and routed `waitpid` /
//! `waitpid_nohang` through them; these tests pin the resulting behavior so a
//! future refactor cannot silently change it.
//!
//! Every process test uses a *targeted* `waitpid(child)` (never `waitpid(-1)`),
//! so the tests are safe to run in parallel and bound their waits with a
//! deadline. Children are killed and reaped through a `Drop` guard on both the
//! success and panic paths.

use std::{
    error::Error,
    io, thread,
    time::{Duration, Instant},
};

use fork::{
    Fork, ProcessFork, ProcessId, Signal, WEXITSTATUS, WIFEXITED, WIFSIGNALED, WTERMSIG, fork,
    getpgrp, getpid, getppid, signal_process, waitpid, waitpid_nohang,
};

const CHILD_EXIT_CODE: libc::c_int = 42;
const TEST_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(5);

struct ChildGuard {
    process: libc::pid_t,
    reaped: bool,
}

impl ChildGuard {
    const fn new(process: libc::pid_t) -> Self {
        Self {
            process,
            reaped: false,
        }
    }

    fn mark_reaped(&mut self) {
        self.reaped = true;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        if let Some(pid) = ProcessId::new(self.process) {
            let _ = signal_process(pid, Signal::KILL);
        }
        let _ = waitpid(self.process);
    }
}

fn child_exit(code: libc::c_int) -> ! {
    // SAFETY: _exit terminates the forked child immediately without running the
    // test harness's destructors or duplicating buffered output.
    unsafe { libc::_exit(code) }
}

fn child_wait_for_kill() -> ! {
    loop {
        // SAFETY: pause() has no preconditions; it suspends the child until a
        // signal arrives. The parent delivers SIGKILL to end it.
        unsafe {
            libc::pause();
        }
    }
}

#[test]
fn identity_accessors_return_positive_values() {
    assert!(getpid() > 0);
    assert!(getppid() > 0);
    assert!(getpgrp() > 0);
}

#[test]
fn fork_enum_accessors_are_stable() {
    // Pure: the observable shape of the pre-existing `Fork` enum, plus the
    // additive `child_process_id`, must not change.
    assert!(Fork::Child.is_child());
    assert!(!Fork::Child.is_parent());
    assert_eq!(Fork::Child.child_pid(), None);
    assert_eq!(Fork::Child.child_process_id(), None);

    let parent = Fork::Parent(4321);
    assert!(parent.is_parent());
    assert!(!parent.is_child());
    assert_eq!(parent.child_pid(), Some(4321));
    assert_eq!(parent.child_process_id(), ProcessId::new(4321));
}

#[test]
fn process_fork_enum_shape_is_stable() {
    assert_eq!(ProcessFork::Child, ProcessFork::Child);
    if let Some(pid) = ProcessId::new(4321) {
        let parent = ProcessFork::Parent(pid);
        assert_eq!(parent, ProcessFork::Parent(pid));
        assert_ne!(parent, ProcessFork::Child);
        if let ProcessFork::Parent(inner) = parent {
            assert_eq!(inner.get(), 4321);
        }
    }
}

#[test]
fn waitpid_reports_exact_exit_status() -> Result<(), Box<dyn Error>> {
    match fork()? {
        Fork::Parent(child) => {
            let mut guard = ChildGuard::new(child);
            let status = waitpid(child)?;
            assert!(WIFEXITED(status));
            assert_eq!(WEXITSTATUS(status), CHILD_EXIT_CODE);
            guard.mark_reaped();
            Ok(())
        }
        Fork::Child => child_exit(CHILD_EXIT_CODE),
    }
}

#[test]
fn waitpid_nohang_reports_none_while_running_then_terminal() -> Result<(), Box<dyn Error>> {
    match fork()? {
        Fork::Parent(child) => {
            let mut guard = ChildGuard::new(child);

            // The child blocks in pause(), so it never terminates on its own:
            // a non-blocking wait must observe no state change.
            assert_eq!(waitpid_nohang(child)?, None);

            let pid = ProcessId::new(child).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "fork returned a nonpositive pid",
                )
            })?;
            signal_process(pid, Signal::KILL)?;

            let deadline = Instant::now() + TEST_TIMEOUT;
            let status = loop {
                if let Some(status) = waitpid_nohang(child)? {
                    break status;
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "child did not terminate within the test deadline",
                    )
                    .into());
                }
                thread::sleep(POLL_INTERVAL);
            };
            assert!(WIFSIGNALED(status));
            assert_eq!(WTERMSIG(status), libc::SIGKILL);
            guard.mark_reaped();
            Ok(())
        }
        Fork::Child => child_wait_for_kill(),
    }
}
