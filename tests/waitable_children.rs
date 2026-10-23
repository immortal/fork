//! Explicit owner-policy hygiene, isolated from libtest's threads.
//!
//! Each disposition is observed without mutation, corrected only when needed,
//! then exercised through all three checked APIs. Children exit immediately;
//! the owner adopts detached children where supported and waits with deadlines.

use std::{
    error::Error,
    io, mem, ptr, thread,
    time::{Duration, Instant},
};

use fork::{CheckedDaemon, DaemonOptions, PreparedCommand, ProcessGroupGuard};

const TIMEOUT: Duration = Duration::from_secs(3);

/// A non-reaping handler whose identity, flags and mask must be preserved.
extern "C" fn handler(_: libc::c_int) {}

fn main() -> Result<(), Box<dyn Error>> {
    let adopted = match fork::acquire_subreaper() {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::Unsupported => false,
        Err(error) => return Err(error.into()),
    };
    for (disposition, flags, changed) in [
        (libc::SIG_DFL, 0, false),
        (libc::SIG_IGN, 0, true),
        (libc::SIG_DFL, libc::SA_NOCLDWAIT, true),
        (handler as *const () as usize, libc::SA_NOCLDWAIT, true),
        (handler as *const () as usize, libc::SA_RESTART, false),
    ] {
        let _restore = install(disposition, flags)?;
        let before = query()?;
        assert_eq!(fork::children_are_waitable()?, !changed);
        assert_same(&before, &query()?);
        if changed {
            daemon_rejected_before_correction()?;
        }
        assert_eq!(fork::ensure_waitable_children()?, changed);
        assert!(fork::children_are_waitable()?);
        let after = query()?;
        if changed {
            assert_eq!(after.sa_sigaction, libc::SIG_DFL);
            assert_eq!(
                after.sa_flags & (libc::SA_NOCLDWAIT | libc::SA_RESTART | libc::SA_SIGINFO),
                0
            );
            // SAFETY: the mask was filled by sigaction.
            assert_eq!(
                unsafe { libc::sigismember(&raw const after.sa_mask, libc::SIGUSR1) },
                0
            );
        } else {
            assert_same(&before, &after);
        }
        assert!(!fork::ensure_waitable_children()?);
        checked_apis_succeed(adopted)?;
    }
    if adopted {
        fork::release_subreaper()?;
    }
    Ok(())
}

/// Daemon startup requires collection, unlike guard/spawn's pipe-backed creation.
fn daemon_rejected_before_correction() -> Result<(), Box<dyn Error>> {
    match fork::checked_daemon(DaemonOptions::new(), TIMEOUT) {
        Err(fork::DaemonError::OperatingSystem {
            stage: fork::DaemonStage::SignalState,
            error,
            cleanup_pending,
        }) if error.kind() == io::ErrorKind::InvalidInput && cleanup_pending.is_empty() => Ok(()),
        Ok(CheckedDaemon::Daemon(notifier)) => {
            let _ = notifier.notify_ready();
            // SAFETY: never return into the parent fixture in a broken implementation.
            unsafe { libc::_exit(0) }
        }
        other => Err(format!("unexpected daemon result: {other:?}").into()),
    }
}

/// Create and finish every checked child with the corrected or preserved policy.
fn checked_apis_succeed(adopted: bool) -> Result<(), Box<dyn Error>> {
    ProcessGroupGuard::new()?.disarm(TIMEOUT)?;
    // /bin/true is absent on FreeBSD and macOS; /bin/sh exists on every target.
    let mut command = PreparedCommand::new("/bin/sh")?;
    command.arg("-c")?;
    command.arg("exit 0")?;
    let child = command.spawn(TIMEOUT)?;
    wait_child(child.process())?;
    match fork::checked_daemon(DaemonOptions::new(), TIMEOUT)? {
        CheckedDaemon::Daemon(notifier) => {
            let result = notifier.notify_ready();
            // SAFETY: terminate only this detached fixture without inherited destructors.
            unsafe { libc::_exit(i32::from(result.is_err())) }
        }
        CheckedDaemon::Parent(daemon) => {
            if adopted {
                wait_child(daemon.process())?;
            } else {
                let deadline = Instant::now() + TIMEOUT;
                while fork::process_group_exists(daemon.process_group())? {
                    if Instant::now() >= deadline {
                        return Err("daemon did not exit".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }
    Ok(())
}

/// Collect only this child's terminal event, with owned-PID failure cleanup.
fn wait_child(pid: fork::ProcessId) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(event) = fork::wait_event_nohang(pid)?
            && event.is_terminal()
        {
            assert!(matches!(event, fork::ChildEvent::Exited { code: 0, .. }));
            return Ok(());
        }
        if Instant::now() >= deadline {
            fork::signal_process(pid, fork::Signal::KILL)?;
            fork::wait_event(pid)?;
            return Err("child did not finish".into());
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// Compare semantic state; libc may add an internal restorer flag on installation.
fn assert_same(a: &libc::sigaction, b: &libc::sigaction) {
    assert_eq!(a.sa_sigaction, b.sa_sigaction);
    assert_eq!(a.sa_flags, b.sa_flags);
    // SAFETY: both masks were initialized by sigaction.
    unsafe {
        assert_eq!(
            libc::sigismember(&raw const a.sa_mask, libc::SIGUSR1),
            libc::sigismember(&raw const b.sa_mask, libc::SIGUSR1)
        );
    }
}

/// Restore process-global state on every return or assertion failure.
struct Restore(libc::sigaction);
impl Drop for Restore {
    fn drop(&mut self) {
        // SAFETY: this standalone executable owns the previously queried action.
        unsafe {
            libc::sigaction(libc::SIGCHLD, &raw const self.0, ptr::null_mut());
        }
    }
}

/// Install a test policy with a nonempty handler mask to detect accidental changes.
fn install(handler: libc::sighandler_t, flags: libc::c_int) -> io::Result<Restore> {
    let previous = query()?;
    // SAFETY: initialized before installation, with a valid handler or disposition.
    let mut action: libc::sigaction = unsafe { mem::zeroed() };
    action.sa_sigaction = handler;
    action.sa_flags = flags;
    // SAFETY: all pointers refer to live initialized signal structures.
    unsafe {
        libc::sigemptyset(&raw mut action.sa_mask);
        libc::sigaddset(&raw mut action.sa_mask, libc::SIGUSR1);
        if libc::sigaction(libc::SIGCHLD, &raw const action, ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(Restore(previous))
}

/// Read current state without changing it.
fn query() -> io::Result<libc::sigaction> {
    // SAFETY: sigaction initializes the output on success.
    let mut action: libc::sigaction = unsafe { mem::zeroed() };
    if unsafe { libc::sigaction(libc::SIGCHLD, ptr::null(), &raw mut action) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(action)
}
