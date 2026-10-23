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
/// numbers while preventing signal zero from being used as a process probe;
/// group membership is checked through [`process_group_exists`] instead.
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
/// with process delivery. Group 1 is refused: `kill(-1, ..)` is the POSIX
/// broadcast to every process the caller may signal, never a group selector.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] for group 1 and otherwise the
/// underlying `kill(2)` error.
pub fn signal_process_group(group: ProcessGroupId, signal: Signal) -> io::Result<()> {
    kill_retry(group_target(group)?, signal)
}

/// Report whether any process, including an unreaped zombie, belongs to `group`.
///
/// This is the POSIX null-signal check `kill(-pgid, 0)`: existence and
/// permission are verified and nothing is delivered. A member the caller may
/// not signal (`EPERM`) still counts as present. An exited member stays in its
/// group until its parent reaps it, so a group holding only zombies is reported
/// present until they are reaped; macOS skips zombie members and answers such a
/// group with `EPERM`, which is reported present as well.
///
/// The answer is a point-in-time observation. Once a group has emptied, the
/// kernel may hand its numeric ID to an unrelated group, so `true` never proves
/// that the original group still exists and must never be the reason to signal
/// it. Stop probing at the first `false`. The ID is only stable while the
/// caller pins it, for example with an unreaped zombie member, an unreaped
/// child whose PID equals the PGID, or an armed [`crate::ProcessGroupGuard`].
/// A live nonleader's membership snapshot is not a pin: it can leave immediately
/// after a `getpgid` observation.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] for group 1 (`kill(-1, ..)` is a
/// broadcast, not a group selector) and any `kill(2)` error other than
/// `ESRCH`, `EPERM`, or `EINTR`, which is retried.
pub fn process_group_exists(group: ProcessGroupId) -> io::Result<bool> {
    let target = group_target(group)?;
    loop {
        // SAFETY: `target` comes from `group_target`, the private negation of a
        // checked group ID other than 1, so it selects exactly that group and
        // never the broadcast target; signal zero delivers nothing.
        if unsafe { libc::kill(target, 0) } == 0 {
            return Ok(true);
        }
        if let Some(outcome) = classify_probe_errno(io::Error::last_os_error()) {
            return outcome;
        }
    }
}

/// Convert a checked group ID into its private negative `kill(2)` target.
///
/// Group 1 is refused because `kill(-1, ..)` addresses every process the
/// caller may signal rather than one group.
fn group_target(group: ProcessGroupId) -> io::Result<libc::pid_t> {
    if group.get() == 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "process group 1 cannot be targeted: kill(-1) is a broadcast",
        ));
    }
    Ok(-group.get())
}

/// Interpret one failed `kill(target, 0)` for [`process_group_exists`].
///
/// `None` asks the caller to retry an interrupted call. Every error other than
/// a definite absence keeps the group present or is returned, so a caller
/// waiting for a group to empty fails closed rather than finishing early.
fn classify_probe_errno(error: io::Error) -> Option<io::Result<bool>> {
    match error.raw_os_error() {
        Some(libc::EPERM) => Some(Ok(true)),
        Some(libc::ESRCH) => Some(Ok(false)),
        Some(libc::EINTR) => None,
        _ => Some(Err(error)),
    }
}

fn kill_retry(target: libc::pid_t, signal: Signal) -> io::Result<()> {
    loop {
        // SAFETY: `target` is derived from a checked positive process ID or from
        // `group_target`, which never yields the broadcast target `-1`; `signal`
        // cannot be zero.
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
    use std::{error::Error, io};

    use super::{Signal, classify_probe_errno, group_target};
    use crate::ProcessGroupId;

    #[test]
    fn signals_reject_probe_and_negative_values() {
        assert_eq!(Signal::new(0), None);
        assert_eq!(Signal::new(-1), None);
    }

    #[test]
    fn group_target_rejects_the_broadcast_group() -> Result<(), Box<dyn Error>> {
        let broadcast = ProcessGroupId::new(1).ok_or("group 1 must be representable")?;
        let refused = group_target(broadcast)
            .err()
            .ok_or("group 1 must never become a kill(2) target")?;
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);

        let group = ProcessGroupId::new(4242).ok_or("group 4242 must be representable")?;
        assert_eq!(group_target(group)?, -4242);
        Ok(())
    }

    #[test]
    fn probe_errno_classification_fails_closed() {
        let classify = |errno| classify_probe_errno(io::Error::from_raw_os_error(errno));
        assert!(matches!(classify(libc::EPERM), Some(Ok(true))));
        assert!(matches!(classify(libc::ESRCH), Some(Ok(false))));
        assert!(classify(libc::EINTR).is_none());
        assert!(matches!(
            classify(libc::EINVAL),
            Some(Err(error)) if error.raw_os_error() == Some(libc::EINVAL)
        ));
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
