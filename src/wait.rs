use std::io;

use crate::{ProcessId, Signal};

const CHILD_EVENT_OPTIONS: libc::c_int = libc::WUNTRACED | libc::WCONTINUED;

/// One observed state change for a direct child process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildEvent {
    Exited { pid: ProcessId, code: u8 },
    Signalled { pid: ProcessId, signal: Signal },
    Stopped { pid: ProcessId, signal: Signal },
    Continued { pid: ProcessId },
}

impl ChildEvent {
    /// Return the child associated with this event.
    #[must_use]
    pub const fn pid(self) -> ProcessId {
        match self {
            Self::Exited { pid, .. }
            | Self::Signalled { pid, .. }
            | Self::Stopped { pid, .. }
            | Self::Continued { pid } => pid,
        }
    }

    /// Whether this event reaped the child permanently.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Exited { .. } | Self::Signalled { .. })
    }
}

/// Wait for a state change from one direct child.
///
/// # Errors
///
/// Returns the underlying `waitpid(2)` error or `InvalidData` if the operating
/// system returns an invalid status.
pub fn wait_event(process: ProcessId) -> io::Result<ChildEvent> {
    let (pid, status) = waitpid_reaped(process.get(), CHILD_EVENT_OPTIONS)?;
    decode_child_event(pid, status)
}

/// Poll one direct child for a state change without blocking.
///
/// # Errors
///
/// Returns the underlying `waitpid(2)` error or `InvalidData` if the operating
/// system returns an invalid status.
pub fn wait_event_nohang(process: ProcessId) -> io::Result<Option<ChildEvent>> {
    waitpid_reaped_nohang(process.get(), libc::WNOHANG | CHILD_EVENT_OPTIONS)?
        .map(|(pid, status)| decode_child_event(pid, status))
        .transpose()
}

/// Wait for a state change from any direct child.
///
/// This owns the same global child-reaping caveat as the raw `wait_any` API:
/// callers must own every child of the current process.
///
/// # Errors
///
/// Returns the underlying `waitpid(2)` error or `InvalidData` if the operating
/// system returns an invalid status.
pub fn wait_any_event() -> io::Result<ChildEvent> {
    let (pid, status) = waitpid_reaped(-1, CHILD_EVENT_OPTIONS)?;
    decode_child_event(pid, status)
}

/// Poll every direct child for one state change without blocking.
///
/// Call repeatedly until `Ok(None)` after a coalesced `SIGCHLD` to drain all
/// currently pending events.
///
/// # Errors
///
/// Returns the underlying `waitpid(2)` error or `InvalidData` if the operating
/// system returns an invalid status.
pub fn wait_any_event_nohang() -> io::Result<Option<ChildEvent>> {
    waitpid_reaped_nohang(-1, libc::WNOHANG | CHILD_EVENT_OPTIONS)?
        .map(|(pid, status)| decode_child_event(pid, status))
        .transpose()
}

pub(crate) fn waitpid_reaped(
    pid: libc::pid_t,
    options: libc::c_int,
) -> io::Result<(libc::pid_t, libc::c_int)> {
    let mut status: libc::c_int = 0;
    loop {
        // SAFETY: status points to initialized writable storage for the call;
        // pid/options are the documented waitpid selectors used by this module.
        let result = unsafe { libc::waitpid(pid, &raw mut status, options) };
        if result >= 0 {
            return Ok((result, status));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

pub(crate) fn waitpid_reaped_nohang(
    pid: libc::pid_t,
    options: libc::c_int,
) -> io::Result<Option<(libc::pid_t, libc::c_int)>> {
    let (reaped, status) = waitpid_reaped(pid, options)?;
    if reaped == 0 {
        Ok(None)
    } else {
        Ok(Some((reaped, status)))
    }
}

/// An exact-child observation must distinguish a retained pin from a consumed
/// terminal event. Callers must propagate Reaped rather than signal or wait again.
pub(crate) enum ExitObservation {
    Pinned(Option<ChildEvent>),
    Reaped(ChildEvent),
}

impl ExitObservation {
    /// A traced child may report a stop even without WUNTRACED. Only terminal
    /// collection releases the pin; consuming a stop/continue does not.
    fn from_consuming(event: Option<ChildEvent>) -> Self {
        match event {
            Some(event) if event.is_terminal() => Self::Reaped(event),
            _ => Self::Pinned(None),
        }
    }
}

/// Prefer a non-consuming terminal observation. If waitid fails, use
/// exact-child waitpid as in 0.10.0, explicitly returning any consumed event;
/// a successful but malformed observation still fails closed.
/// Idle observations prove ownership only while auto-reaping and other child
/// collectors remain excluded by the caller.
pub(crate) fn observe_exit_nohang(process: ProcessId) -> io::Result<ExitObservation> {
    match peek_exit_nohang(process) {
        Ok(event) => Ok(ExitObservation::Pinned(event)),
        Err(error) if waitid_unavailable(&error) => {
            waitpid_reaped_nohang(process.get(), libc::WNOHANG)?
                .map(|(pid, status)| decode_child_event(pid, status))
                .transpose()
                .map(ExitObservation::from_consuming)
        }
        Err(error) => Err(error),
    }
}

/// Whether a failed non-consuming observation must fall back to `waitpid`.
///
/// `waitid` is optional here: 0.10.0 used only exact-child `waitpid`. Sandboxes
/// deny syscalls with arbitrary errnos (`ENOSYS`, `EPERM`, `EACCES`, ...), and
/// a kernel without `WNOWAIT` answers `EINVAL`, so every system error falls
/// back. `ECHILD` gets the same answer from `waitpid`, and `EINTR` was already
/// retried. A waitid failure never proves ownership: the consuming wait decides.
/// Only decoding errors of a successful observation, which are not system
/// errors, are returned.
fn waitid_unavailable(error: &io::Error) -> bool {
    error.raw_os_error().is_some()
}

/// Observe without consuming; only `observe_exit_nohang` may handle unavailability.
fn peek_exit_nohang(process: ProcessId) -> io::Result<Option<ChildEvent>> {
    let id = libc::id_t::try_from(i128::from(process.get()))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    loop {
        // SAFETY: zeroed siginfo is valid storage; successful waitid initializes it.
        let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: the checked PID selects one child; WNOWAIT retains its zombie.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                id,
                &raw mut information,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        // SAFETY: successful waitid initialized these SIGCHLD union fields.
        let observed = unsafe { information.si_pid() };
        if observed == 0 {
            return Ok(None);
        }
        if observed != process.get() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "waitid returned another child",
            ));
        }
        // SAFETY: a nonzero matching PID identifies a populated child status.
        let status = unsafe { information.si_status() };
        return match information.si_code {
            libc::CLD_EXITED => Ok(Some(ChildEvent::Exited {
                pid: process,
                code: u8::try_from(status)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            })),
            libc::CLD_KILLED | libc::CLD_DUMPED => Ok(Some(ChildEvent::Signalled {
                pid: process,
                signal: decode_signal(status)?,
            })),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "waitid returned a nonterminal event",
            )),
        };
    }
}

fn decode_child_event(pid: libc::pid_t, status: libc::c_int) -> io::Result<ChildEvent> {
    let pid = ProcessId::try_from(pid)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if libc::WIFEXITED(status) {
        let code = u8::try_from(libc::WEXITSTATUS(status)).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "child exit code is out of range",
            )
        })?;
        return Ok(ChildEvent::Exited { pid, code });
    }
    if libc::WIFSIGNALED(status) {
        return Ok(ChildEvent::Signalled {
            pid,
            signal: decode_signal(libc::WTERMSIG(status))?,
        });
    }
    if libc::WIFSTOPPED(status) {
        return Ok(ChildEvent::Stopped {
            pid,
            signal: decode_signal(libc::WSTOPSIG(status))?,
        });
    }
    if libc::WIFCONTINUED(status) {
        return Ok(ChildEvent::Continued { pid });
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "waitpid returned an unknown child status",
    ))
}

fn decode_signal(raw: libc::c_int) -> io::Result<Signal> {
    Signal::try_from(raw).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::{ChildEvent, ExitObservation, waitid_unavailable};
    use crate::{ProcessId, Signal};

    /// Any errno a sandbox or old kernel can answer must keep 0.10.0's
    /// exact-child waitpid path; only non-system decoding errors propagate.
    #[test]
    fn waitid_fallback_covers_every_system_error() {
        for errno in [
            libc::ENOSYS,
            libc::EPERM,
            libc::EACCES,
            libc::EINVAL,
            libc::ECHILD,
        ] {
            assert!(waitid_unavailable(&std::io::Error::from_raw_os_error(
                errno
            )));
        }
        assert!(!waitid_unavailable(&std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "waitid returned another child",
        )));
    }

    #[test]
    fn consuming_observation_retires_only_terminal_events() -> std::io::Result<()> {
        let pid = ProcessId::try_from(42).map_err(std::io::Error::other)?;
        for event in [
            None,
            Some(ChildEvent::Stopped {
                pid,
                signal: Signal::STOP,
            }),
            Some(ChildEvent::Continued { pid }),
        ] {
            assert!(matches!(
                ExitObservation::from_consuming(event),
                ExitObservation::Pinned(None)
            ));
        }
        for event in [
            ChildEvent::Exited { pid, code: 73 },
            ChildEvent::Signalled {
                pid,
                signal: Signal::KILL,
            },
        ] {
            assert!(
                matches!(ExitObservation::from_consuming(Some(event)), ExitObservation::Reaped(observed) if observed == event)
            );
        }
        Ok(())
    }

    #[test]
    fn terminal_classification_is_explicit() {
        let process = ProcessId::new(42);
        assert!(process.is_some());
        let Some(pid) = process else {
            return;
        };
        assert!(ChildEvent::Exited { pid, code: 0 }.is_terminal());
        assert!(
            ChildEvent::Signalled {
                pid,
                signal: Signal::TERM
            }
            .is_terminal()
        );
        assert!(
            !ChildEvent::Stopped {
                pid,
                signal: Signal::STOP
            }
            .is_terminal()
        );
        assert!(!ChildEvent::Continued { pid }.is_terminal());
    }
}
