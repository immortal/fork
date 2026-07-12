use std::{
    error::Error,
    fmt::{self, Display, Formatter},
    io,
};

/// A checked positive Unix process identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ProcessId(libc::pid_t);

impl ProcessId {
    /// Construct a process identifier only when `raw` is positive.
    #[must_use]
    pub const fn new(raw: libc::pid_t) -> Option<Self> {
        if raw > 0 { Some(Self(raw)) } else { None }
    }

    /// Return the positive operating-system value.
    #[must_use]
    pub const fn get(self) -> libc::pid_t {
        self.0
    }
}

impl Display for ProcessId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

impl TryFrom<libc::pid_t> for ProcessId {
    type Error = InvalidProcessId;

    fn try_from(raw: libc::pid_t) -> Result<Self, Self::Error> {
        Self::new(raw).ok_or(InvalidProcessId(raw))
    }
}

impl From<ProcessId> for libc::pid_t {
    fn from(pid: ProcessId) -> Self {
        pid.get()
    }
}

/// A raw value was not a valid positive process identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidProcessId(libc::pid_t);

impl InvalidProcessId {
    /// Return the rejected raw value.
    #[must_use]
    pub const fn raw(self) -> libc::pid_t {
        self.0
    }
}

impl Display for InvalidProcessId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "process ID must be positive, got {}", self.0)
    }
}

impl Error for InvalidProcessId {}

/// A checked positive Unix process-group identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ProcessGroupId(libc::pid_t);

impl ProcessGroupId {
    /// Construct a process-group identifier only when `raw` is positive.
    #[must_use]
    pub const fn new(raw: libc::pid_t) -> Option<Self> {
        if raw > 0 { Some(Self(raw)) } else { None }
    }

    /// Return the positive operating-system value.
    #[must_use]
    pub const fn get(self) -> libc::pid_t {
        self.0
    }
}

impl Display for ProcessGroupId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, formatter)
    }
}

impl TryFrom<libc::pid_t> for ProcessGroupId {
    type Error = InvalidProcessGroupId;

    fn try_from(raw: libc::pid_t) -> Result<Self, Self::Error> {
        Self::new(raw).ok_or(InvalidProcessGroupId(raw))
    }
}

impl From<ProcessGroupId> for libc::pid_t {
    fn from(pgid: ProcessGroupId) -> Self {
        pgid.get()
    }
}

impl From<ProcessId> for ProcessGroupId {
    fn from(pid: ProcessId) -> Self {
        Self(pid.get())
    }
}

/// A raw value was not a valid positive process-group identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidProcessGroupId(libc::pid_t);

impl InvalidProcessGroupId {
    /// Return the rejected raw value.
    #[must_use]
    pub const fn raw(self) -> libc::pid_t {
        self.0
    }
}

impl Display for InvalidProcessGroupId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "process-group ID must be positive, got {}",
            self.0
        )
    }
}

impl Error for InvalidProcessGroupId {}

/// Return the calling process's checked identifier.
#[must_use]
pub fn current_process_id() -> ProcessId {
    // SAFETY: getpid has no preconditions and always returns the caller's
    // positive PID on supported Unix platforms.
    let raw = unsafe { libc::getpid() };
    debug_assert!(raw > 0);
    ProcessId(raw)
}

/// Return the calling process's checked process-group identifier.
#[must_use]
pub fn current_process_group_id() -> ProcessGroupId {
    // SAFETY: getpgrp has no preconditions and always returns the caller's
    // positive process-group ID on supported Unix platforms.
    let raw = unsafe { libc::getpgrp() };
    debug_assert!(raw > 0);
    ProcessGroupId(raw)
}

/// Return the current process group of `process`.
///
/// # Errors
///
/// Returns the operating-system error when the process does not exist or may
/// not be inspected.
pub fn process_group(process: ProcessId) -> io::Result<ProcessGroupId> {
    loop {
        // SAFETY: `process` guarantees a positive PID; getpgid has no pointer
        // arguments and reports nonexistent or unauthorized processes as errors.
        let raw = unsafe { libc::getpgid(process.get()) };
        if raw >= 0 {
            return ProcessGroupId::try_from(raw)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Make the calling process the leader of a new process group.
///
/// # Errors
///
/// Returns the underlying `setpgid(2)` error.
pub fn create_current_process_group() -> io::Result<ProcessGroupId> {
    setpgid_retry(0, 0)?;
    Ok(ProcessGroupId::from(current_process_id()))
}

/// Make `process` the leader of a new process group with the same numeric ID.
///
/// Supervisors commonly call this in the parent while the child also calls
/// [`create_current_process_group`] to close the fork/exec race.
///
/// # Errors
///
/// Returns the underlying `setpgid(2)` error.
pub fn create_process_group(process: ProcessId) -> io::Result<ProcessGroupId> {
    setpgid_retry(process.get(), process.get())?;
    Ok(ProcessGroupId::from(process))
}

/// Move `process` into an existing process group.
///
/// # Errors
///
/// Returns the underlying `setpgid(2)` error.
pub fn join_process_group(process: ProcessId, group: ProcessGroupId) -> io::Result<()> {
    setpgid_retry(process.get(), group.get())
}

fn setpgid_retry(target_process: libc::pid_t, target_group: libc::pid_t) -> io::Result<()> {
    loop {
        // SAFETY: setpgid accepts the explicit POSIX zero sentinel used only by
        // create_current_process_group, or positive IDs checked by public types.
        if unsafe { libc::setpgid(target_process, target_group) } == 0 {
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
    use super::{ProcessGroupId, ProcessId};

    #[test]
    fn identifiers_reject_zero_and_negative_values() {
        assert_eq!(ProcessId::new(0), None);
        assert_eq!(ProcessId::new(-1), None);
        assert_eq!(ProcessGroupId::new(0), None);
        assert_eq!(ProcessGroupId::new(-1), None);
    }

    #[test]
    fn identifiers_preserve_positive_values() {
        let process = ProcessId::new(42);
        let group = ProcessGroupId::new(42);
        assert_eq!(process.map(ProcessId::get), Some(42));
        assert_eq!(group.map(ProcessGroupId::get), Some(42));
    }
}
