//! Standalone subreaper lifecycle contract without the multithreaded test harness.

use std::{io, process::ExitCode};

use fork::is_subreaper;

const EXEC_PROBE_ARGUMENT: &str = "--subreaper-exec-probe";

fn main() -> ExitCode {
    let result = if std::env::args().any(|argument| argument == EXEC_PROBE_ARGUMENT) {
        exec_probe()
    } else {
        run_contracts()
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("subreaper contract failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Re-executed image asserting that an acquired role survives `exec`.
fn exec_probe() -> io::Result<()> {
    if is_subreaper()? {
        Ok(())
    } else {
        Err(io::Error::other(
            "subreaper role was not preserved across exec",
        ))
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn run_contracts() -> io::Result<()> {
    supported::run()
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn run_contracts() -> io::Result<()> {
    unsupported::run()
}

/// Lifecycle contracts for platforms that implement subreaping.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod supported {
    use std::{
        env,
        fs::File,
        io::{self, Read as _, Write as _},
        os::{fd::OwnedFd, unix::process::CommandExt as _},
        process::Command,
        time::{Duration, Instant},
    };

    use fork::{
        ChildEvent, ProcessFork, ProcessId, acquire_subreaper, current_process_id, fork_process,
        getppid, is_subreaper, pipe_cloexec, release_subreaper, wait_any_event, wait_event,
    };

    use super::EXEC_PROBE_ARGUMENT;

    const GRANDCHILD_EXIT: u8 = 42;

    pub(super) fn run() -> io::Result<()> {
        state_transitions()?;
        fork_does_not_inherit()?;
        exec_preserves_role()?;
        orphaned_grandchild_is_adopted()?;
        release_subreaper()
    }

    /// Acquire and release are idempotent and toggle the observed state.
    fn state_transitions() -> io::Result<()> {
        release_subreaper()?;
        require_state(false, "initial release")?;
        acquire_subreaper()?;
        acquire_subreaper()?;
        require_state(true, "repeated acquisition")?;
        release_subreaper()?;
        release_subreaper()?;
        require_state(false, "repeated release")
    }

    /// The role does not cross a `fork` boundary into the child.
    fn fork_does_not_inherit() -> io::Result<()> {
        acquire_subreaper()?;
        let pipe = pipe_cloexec()?;
        let (reader, writer) = pipe.into_parts();
        match fork_process()? {
            ProcessFork::Parent(child) => {
                drop(writer);
                let mut inherited = [0_u8; 1];
                File::from(reader).read_exact(&mut inherited)?;
                require_exited(wait_event(child)?, child, 0)?;
                if inherited == [0] {
                    Ok(())
                } else {
                    Err(io::Error::other("fork child inherited subreaper role"))
                }
            }
            ProcessFork::Child => {
                drop(reader);
                let inherited = is_subreaper().unwrap_or(true);
                write_byte_and_exit(writer, u8::from(inherited), 0);
            }
        }
    }

    /// The role survives `exec`, verified by the re-executed probe image.
    fn exec_preserves_role() -> io::Result<()> {
        match fork_process()? {
            ProcessFork::Parent(child) => require_exited(wait_event(child)?, child, 0),
            ProcessFork::Child => {
                if acquire_subreaper().is_err() {
                    child_exit(1);
                }
                let Ok(executable) = env::current_exe() else {
                    child_exit(2);
                };
                let error = Command::new(executable).arg(EXEC_PROBE_ARGUMENT).exec();
                eprintln!("failed to exec subreaper probe: {error}");
                child_exit(3);
            }
        }
    }

    /// A grandchild orphaned by its intermediate parent reparents to the
    /// subreaper and becomes reapable through `wait_any_*`.
    ///
    /// The role is acquired before the intermediate is forked because FreeBSD
    /// assigns a descendant's reaper at fork time.
    fn orphaned_grandchild_is_adopted() -> io::Result<()> {
        acquire_subreaper()?;
        let supervisor = current_process_id();
        let pipe = pipe_cloexec()?;
        let (reader, writer) = pipe.into_parts();
        match fork_process()? {
            ProcessFork::Parent(intermediate) => {
                drop(writer);
                let mut grandchild_bytes = [0_u8; size_of::<libc::pid_t>()];
                File::from(reader).read_exact(&mut grandchild_bytes)?;
                let grandchild = ProcessId::try_from(libc::pid_t::from_ne_bytes(grandchild_bytes))
                    .map_err(io::Error::other)?;
                require_exited(wait_event(intermediate)?, intermediate, 0)?;
                require_exited(wait_any_event()?, grandchild, GRANDCHILD_EXIT)
            }
            ProcessFork::Child => {
                drop(reader);
                match fork_process() {
                    Ok(ProcessFork::Parent(grandchild)) => {
                        write_pid_and_exit(writer, grandchild.get(), 0);
                    }
                    Ok(ProcessFork::Child) => {
                        drop(writer);
                        let deadline = Instant::now() + Duration::from_secs(3);
                        while getppid() != supervisor.get() {
                            if Instant::now() >= deadline {
                                child_exit(70);
                            }
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        child_exit(libc::c_int::from(GRANDCHILD_EXIT));
                    }
                    Err(_) => write_pid_and_exit(writer, 0, 71),
                }
            }
        }
    }

    fn require_state(expected: bool, context: &str) -> io::Result<()> {
        let actual = is_subreaper()?;
        if actual == expected {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "{context} left subreaper state {actual}; expected {expected}"
            )))
        }
    }

    fn require_exited(
        event: ChildEvent,
        expected_process: ProcessId,
        expected_code: u8,
    ) -> io::Result<()> {
        if event
            == (ChildEvent::Exited {
                pid: expected_process,
                code: expected_code,
            })
        {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "unexpected child event {event:?}; expected PID {expected_process} exit {expected_code}"
            )))
        }
    }

    fn write_byte_and_exit(descriptor: OwnedFd, value: u8, code: libc::c_int) -> ! {
        let result = File::from(descriptor).write_all(&[value]);
        child_exit(if result.is_ok() { code } else { 72 })
    }

    fn write_pid_and_exit(descriptor: OwnedFd, process: libc::pid_t, code: libc::c_int) -> ! {
        let result = File::from(descriptor).write_all(&process.to_ne_bytes());
        child_exit(if result.is_ok() { code } else { 73 })
    }

    fn child_exit(code: libc::c_int) -> ! {
        // SAFETY: _exit terminates a forked child without running inherited Rust
        // destructors or flushing duplicated buffers.
        unsafe { libc::_exit(code) }
    }
}

/// Contract for platforms where subreaping is not implemented.
#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
mod unsupported {
    use std::io;

    use fork::{acquire_subreaper, is_subreaper, release_subreaper};

    pub(super) fn run() -> io::Result<()> {
        assert_unsupported(acquire_subreaper())?;
        assert_unsupported(release_subreaper())?;
        match is_subreaper() {
            Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(()),
            Err(error) => Err(io::Error::other(format!(
                "subreaper query returned {error:?} instead of Unsupported"
            ))),
            Ok(value) => Err(io::Error::other(format!(
                "unsupported subreaper query unexpectedly returned {value}"
            ))),
        }
    }

    fn assert_unsupported(result: io::Result<()>) -> io::Result<()> {
        match result {
            Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(()),
            Err(error) => Err(io::Error::other(format!(
                "subreaper operation returned {error:?} instead of Unsupported"
            ))),
            Ok(()) => Err(io::Error::other(
                "unsupported subreaper operation unexpectedly succeeded",
            )),
        }
    }
}
