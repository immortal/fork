use std::{
    error::Error,
    fmt::{self, Display, Formatter},
    io,
};

use crate::{ProcessGroupId, ProcessId};

/// A checked nonzero Unix signal number.
///
/// Associated constants cover the portable signals used by process
/// supervisors. [`Signal::new`] also permits platform-specific positive signal
/// numbers while preventing signal zero from being used as a process probe.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct Signal(libc::c_int);

impl Signal {
    /// Hangup or terminal disconnection.
    pub const HUP: Self = Self(libc::SIGHUP);
    /// Interactive interrupt.
    pub const INT: Self = Self(libc::SIGINT);
    /// Interactive quit.
    pub const QUIT: Self = Self(libc::SIGQUIT);
    /// Uncatchable termination.
    pub const KILL: Self = Self(libc::SIGKILL);
    /// Alarm timer expiration.
    pub const ALRM: Self = Self(libc::SIGALRM);
    /// Graceful termination request.
    pub const TERM: Self = Self(libc::SIGTERM);
    /// Uncatchable process stop.
    pub const STOP: Self = Self(libc::SIGSTOP);
    /// Resume a stopped process.
    pub const CONT: Self = Self(libc::SIGCONT);
    /// First user-defined signal.
    pub const USR1: Self = Self(libc::SIGUSR1);
    /// Second user-defined signal.
    pub const USR2: Self = Self(libc::SIGUSR2);
    /// Background terminal input.
    pub const TTIN: Self = Self(libc::SIGTTIN);
    /// Background terminal output.
    pub const TTOU: Self = Self(libc::SIGTTOU);
    /// Terminal window-size change.
    pub const WINCH: Self = Self(libc::SIGWINCH);

    /// Construct a signal from a positive platform signal number.
    #[must_use]
    pub const fn new(raw: libc::c_int) -> Option<Self> {
        if raw > 0 { Some(Self(raw)) } else { None }
    }

    /// Return the positive operating-system signal number.
    #[must_use]
    pub const fn get(self) -> libc::c_int {
        self.0
    }
}

impl Display for Signal {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

impl TryFrom<libc::c_int> for Signal {
    type Error = InvalidSignal;

    fn try_from(raw: libc::c_int) -> Result<Self, Self::Error> {
        Self::new(raw).ok_or(InvalidSignal(raw))
    }
}

impl From<Signal> for libc::c_int {
    fn from(signal: Signal) -> Self {
        signal.get()
    }
}

/// A raw signal number was zero or negative.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidSignal(libc::c_int);

impl InvalidSignal {
    /// Return the rejected raw value.
    #[must_use]
    pub const fn raw(self) -> libc::c_int {
        self.0
    }
}

impl Display for InvalidSignal {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "signal must be positive, got {}", self.0)
    }
}

impl Error for InvalidSignal {}

/// Deliver `signal` to exactly one process.
///
/// # Errors
///
/// Returns the underlying `kill(2)` error.
pub fn signal_process(process: ProcessId, signal: Signal) -> io::Result<()> {
    kill_retry(process.get(), signal)
}

/// Deliver `signal` to every member of one process group.
///
/// The public API accepts only a positive [`ProcessGroupId`]; conversion to the
/// negative `kill(2)` representation remains private and cannot be confused
/// with process delivery.
///
/// # Errors
///
/// Returns the underlying `kill(2)` error.
pub fn signal_process_group(group: ProcessGroupId, signal: Signal) -> io::Result<()> {
    kill_retry(-group.get(), signal)
}

fn kill_retry(target: libc::pid_t, signal: Signal) -> io::Result<()> {
    loop {
        // SAFETY: `target` is derived from a checked positive process ID or the
        // private negation of a checked group ID; `signal` cannot be zero.
        if unsafe { libc::kill(target, signal.get()) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Signal;

    #[test]
    fn signals_reject_probe_and_negative_values() {
        assert_eq!(Signal::new(0), None);
        assert_eq!(Signal::new(-1), None);
    }

    #[test]
    fn portable_constants_are_positive() {
        for signal in [
            Signal::HUP,
            Signal::INT,
            Signal::QUIT,
            Signal::KILL,
            Signal::ALRM,
            Signal::TERM,
            Signal::STOP,
            Signal::CONT,
            Signal::USR1,
            Signal::USR2,
            Signal::TTIN,
            Signal::TTOU,
            Signal::WINCH,
        ] {
            assert!(signal.get() > 0);
        }
    }
}
