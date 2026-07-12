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
    use super::ChildEvent;
    use crate::{ProcessId, Signal};

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
