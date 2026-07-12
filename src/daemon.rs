use std::{
    cmp::Ordering,
    error::Error,
    ffi::{CString, OsStr},
    fmt::{self, Display, Formatter},
    fs::OpenOptions,
    io,
    os::{
        fd::{AsRawFd, OwnedFd, RawFd},
        unix::ffi::OsStrExt,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    ChildEvent, ChildSignalState, ProcessGroupId, ProcessId, Signal,
    child_signal::Prepared as PreparedSignalState,
    descriptor::{Actions as DescriptorActions, Prepared as PreparedDescriptors},
    pipe_cloexec,
    raw::{last_errno, write_all},
    signal_process, signal_process_group, wait_event_nohang,
};

const DAEMON_MAGIC: u32 = 0x4441_454d;
const FAILURE_EXIT: libc::c_int = 1;
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);
const CLEANUP_POLL_INTERVAL: Duration = Duration::from_millis(5);

const RECORD_SESSION: u8 = 1;
const RECORD_DAEMON: u8 = 2;
const RECORD_READY: u8 = 3;
const RECORD_FAILURE: u8 = 4;

/// One reviewed stage in checked daemon startup.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[repr(u8)]
pub enum DaemonStage {
    FirstFork = 1,
    CreateSession = 2,
    SecondFork = 3,
    CurrentDirectory = 4,
    Descriptors = 5,
    Initialization = 6,
    Handshake = 7,
    SignalState = 8,
}

impl Display for DaemonStage {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::FirstFork => "first fork",
            Self::CreateSession => "session creation",
            Self::SecondFork => "second fork",
            Self::CurrentDirectory => "working-directory change",
            Self::Descriptors => "descriptor setup",
            Self::Initialization => "daemon initialization",
            Self::Handshake => "startup handshake",
            Self::SignalState => "signal-state reset",
        })
    }
}

/// Owned daemon work that could not be confirmed clean before returning.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct DaemonCleanup {
    intermediate: Option<ProcessId>,
    process_group: Option<ProcessGroupId>,
}

impl DaemonCleanup {
    /// Return the first-fork child if it still requires reaping.
    #[must_use]
    pub const fn intermediate(&self) -> Option<ProcessId> {
        self.intermediate
    }

    /// Return the owned daemon process group if it may still contain members.
    #[must_use]
    pub const fn process_group(&self) -> Option<ProcessGroupId> {
        self.process_group
    }

    /// Return whether all bounded cleanup obligations completed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.intermediate.is_none() && self.process_group.is_none()
    }
}

/// Failure to detach or initialize a checked daemon.
#[derive(Debug)]
pub enum DaemonError {
    OperatingSystem {
        stage: DaemonStage,
        error: io::Error,
        cleanup_pending: DaemonCleanup,
    },
    TimedOut {
        timeout: Duration,
        cleanup_pending: DaemonCleanup,
    },
    Abandoned {
        cleanup_pending: DaemonCleanup,
    },
    InvalidHandshake {
        cleanup_pending: DaemonCleanup,
    },
}

impl DaemonError {
    fn operating_system(stage: DaemonStage, error: io::Error) -> Self {
        Self::OperatingSystem {
            stage,
            error,
            cleanup_pending: DaemonCleanup::default(),
        }
    }

    fn timed_out(timeout: Duration) -> Self {
        Self::TimedOut {
            timeout,
            cleanup_pending: DaemonCleanup::default(),
        }
    }

    fn invalid_handshake() -> Self {
        Self::InvalidHandshake {
            cleanup_pending: DaemonCleanup::default(),
        }
    }

    fn with_cleanup(self, cleanup_pending: DaemonCleanup) -> Self {
        match self {
            Self::OperatingSystem { stage, error, .. } => Self::OperatingSystem {
                stage,
                error,
                cleanup_pending,
            },
            Self::TimedOut { timeout, .. } => Self::TimedOut {
                timeout,
                cleanup_pending,
            },
            Self::Abandoned { .. } => Self::Abandoned { cleanup_pending },
            Self::InvalidHandshake { .. } => Self::InvalidHandshake { cleanup_pending },
        }
    }

    /// Return daemon cleanup ownership that remains after the bounded attempt.
    #[must_use]
    pub const fn cleanup_pending(&self) -> DaemonCleanup {
        match self {
            Self::OperatingSystem {
                cleanup_pending, ..
            }
            | Self::TimedOut {
                cleanup_pending, ..
            }
            | Self::Abandoned { cleanup_pending }
            | Self::InvalidHandshake { cleanup_pending } => *cleanup_pending,
        }
    }
}

impl Display for DaemonError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::OperatingSystem { stage, error, .. } => {
                write!(formatter, "daemon {stage} failed: {error}")
            }
            Self::TimedOut { timeout, .. } => {
                write!(formatter, "daemon startup timed out after {timeout:?}")
            }
            Self::Abandoned { .. } => {
                formatter.write_str("daemon closed its startup channel before reporting ready")
            }
            Self::InvalidHandshake { .. } => {
                formatter.write_str("daemon returned an invalid startup handshake")
            }
        }
    }
}

impl Error for DaemonError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::OperatingSystem { error, .. } => Some(error),
            Self::TimedOut { .. } | Self::Abandoned { .. } | Self::InvalidHandshake { .. } => None,
        }
    }
}

/// The detached daemon observed by the original invoking process.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct DaemonProcess {
    process: ProcessId,
    process_group: ProcessGroupId,
}

impl DaemonProcess {
    /// Return the live detached daemon PID reported by the grandchild.
    #[must_use]
    pub const fn process(&self) -> ProcessId {
        self.process
    }

    /// Return the owned process group created by the session leader.
    #[must_use]
    pub const fn process_group(&self) -> ProcessGroupId {
        self.process_group
    }
}

/// Startup token held by the detached daemon until initialization finishes.
#[derive(Debug)]
pub struct DaemonNotifier {
    descriptor: OwnedFd,
    process: DaemonProcess,
}

impl DaemonNotifier {
    /// Return this daemon's process and group handles.
    #[must_use]
    pub const fn process(&self) -> DaemonProcess {
        self.process
    }

    /// Report successful initialization to the original invoker.
    ///
    /// # Errors
    ///
    /// Returns the startup-pipe write error, normally `EPIPE` when the invoker
    /// has already timed out or exited.
    pub fn notify_ready(self) -> io::Result<()> {
        write_record(
            self.descriptor.as_raw_fd(),
            WireRecord::new(RECORD_READY, DaemonStage::Initialization, 0),
        )
        .map_err(io::Error::from_raw_os_error)
    }

    /// Report initialization failure and terminate the detached daemon.
    ///
    /// The explicit name reflects that this method never returns and uses
    /// `_exit` so inherited application destructors are not run.
    pub fn fail_and_exit(self, error: &io::Error) -> ! {
        let errno = error.raw_os_error().unwrap_or(libc::EIO);
        let _ = write_record(
            self.descriptor.as_raw_fd(),
            WireRecord::new(RECORD_FAILURE, DaemonStage::Initialization, errno),
        );
        child_exit(FAILURE_EXIT)
    }
}

/// Which side returned from [`checked_daemon`].
#[derive(Debug)]
pub enum CheckedDaemon {
    /// The original invoker, after the detached process reported readiness.
    Parent(DaemonProcess),
    /// The detached grandchild, which must initialize and report readiness.
    Daemon(DaemonNotifier),
}

/// Fully materialized daemon detachment settings.
#[derive(Debug, Default)]
pub struct DaemonOptions {
    current_directory: Option<CString>,
    descriptors: DescriptorActions,
    signal_state: ChildSignalState,
}

impl DaemonOptions {
    /// Preserve the current directory and standard streams by default.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the directory entered after detachment.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when the path contains NUL.
    pub fn current_directory(&mut self, path: impl AsRef<OsStr>) -> io::Result<&mut Self> {
        self.current_directory = Some(CString::new(path.as_ref().as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "daemon working directory must not contain NUL",
            )
        })?);
        Ok(self)
    }

    /// Map an owned source descriptor onto a daemon descriptor number.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a negative, duplicate, or closed target.
    pub fn map_descriptor(&mut self, source: OwnedFd, target: RawFd) -> io::Result<&mut Self> {
        self.descriptors.map(source, target)?;
        Ok(self)
    }

    /// Preserve an owned descriptor at its current number.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when its number conflicts with another action.
    pub fn preserve_descriptor(&mut self, descriptor: OwnedFd) -> io::Result<&mut Self> {
        self.descriptors.inherit(descriptor)?;
        Ok(self)
    }

    /// Explicitly close a daemon descriptor, including standard streams.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a negative or mapped descriptor.
    pub fn close_descriptor(&mut self, descriptor: RawFd) -> io::Result<&mut Self> {
        self.descriptors.close(descriptor)?;
        Ok(self)
    }

    /// Configure whether the detached daemon resets or inherits signal state.
    pub fn signal_state(&mut self, state: ChildSignalState) -> &mut Self {
        self.signal_state = state;
        self
    }

    /// Redirect standard input, output, and error to independently opened
    /// `/dev/null` descriptors.
    ///
    /// # Errors
    ///
    /// Returns an open error or a descriptor-action conflict.
    pub fn redirect_standard_io_to_null(&mut self) -> io::Result<&mut Self> {
        for target in libc::STDIN_FILENO..=libc::STDERR_FILENO {
            let null = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null")?;
            self.descriptors.map(OwnedFd::from(null), target)?;
        }
        Ok(self)
    }
}

struct DaemonPreparation {
    current_directory: Option<CString>,
    descriptors: PreparedDescriptors,
    signal_state: Option<PreparedSignalState>,
    status_reader: OwnedFd,
    status_writer: OwnedFd,
}

/// Detach with a double fork and wait for an explicit bounded readiness result.
///
/// Call this before creating Tokio or any other thread. The original invoker
/// returns only after [`DaemonNotifier::notify_ready`] succeeds. The detached
/// grandchild receives the notifier and may initialize ordinary Rust state
/// because the fork occurred while the process was still single-threaded.
///
/// # Errors
///
/// Returns exact fork/session/directory/descriptor/initialization errors,
/// timeout, channel abandonment, or malformed handshake data. Failure paths
/// kill and reap within a bounded cleanup window and return any residual
/// ownership through [`DaemonError::cleanup_pending`].
pub fn checked_daemon(
    options: DaemonOptions,
    startup_timeout: Duration,
) -> Result<CheckedDaemon, DaemonError> {
    DaemonPreparation::new(options)?.detach(startup_timeout)
}

impl DaemonPreparation {
    fn new(options: DaemonOptions) -> Result<Self, DaemonError> {
        let pipe = pipe_cloexec()
            .map_err(|error| DaemonError::operating_system(DaemonStage::Handshake, error))?;
        let (status_reader, initial_status_writer) = pipe.into_parts();
        let (descriptors, status_writer) = options
            .descriptors
            .prepare(initial_status_writer)
            .map_err(|error| DaemonError::operating_system(DaemonStage::Descriptors, error))?;
        let signal_state = PreparedSignalState::new(options.signal_state)
            .map_err(|error| DaemonError::operating_system(DaemonStage::SignalState, error))?;
        Ok(Self {
            current_directory: options.current_directory,
            descriptors,
            signal_state,
            status_reader,
            status_writer,
        })
    }

    fn detach(self, startup_timeout: Duration) -> Result<CheckedDaemon, DaemonError> {
        // SAFETY: checked_daemon is documented as a pre-thread boundary; both
        // child branches below restrict work until the detached return point.
        let raw_intermediate = unsafe { libc::fork() };
        if raw_intermediate == -1 {
            return Err(DaemonError::operating_system(
                DaemonStage::FirstFork,
                io::Error::last_os_error(),
            ));
        }
        if raw_intermediate == 0 {
            return Ok(self.detach_child());
        }

        let intermediate = ProcessId::new(raw_intermediate).ok_or_else(|| {
            DaemonError::operating_system(
                DaemonStage::FirstFork,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "fork returned a nonpositive intermediate PID",
                ),
            )
        })?;
        drop(self.status_writer);
        let result = parent_wait_ready(&self.status_reader, intermediate, startup_timeout);
        match result {
            Ok(process) => Ok(CheckedDaemon::Parent(process)),
            Err((error, group)) => {
                let cleanup = cleanup_daemon(intermediate, group);
                Err(error.with_cleanup(cleanup))
            }
        }
    }

    fn detach_child(self) -> CheckedDaemon {
        drop(self.status_reader);

        let session = retry_setsid().unwrap_or_else(|errno| {
            report_and_exit(
                self.status_writer.as_raw_fd(),
                DaemonStage::CreateSession,
                errno,
            )
        });
        let group = ProcessGroupId::new(session).unwrap_or_else(|| {
            report_and_exit(
                self.status_writer.as_raw_fd(),
                DaemonStage::CreateSession,
                libc::EIO,
            )
        });
        if write_record(
            self.status_writer.as_raw_fd(),
            WireRecord::new(RECORD_SESSION, DaemonStage::CreateSession, group.get()),
        )
        .is_err()
        {
            child_exit(FAILURE_EXIT);
        }

        // SAFETY: the process remains single-threaded and the grandchild path
        // is fully materialized before this second fork.
        let raw_daemon = unsafe { libc::fork() };
        if raw_daemon == -1 {
            report_and_exit(
                self.status_writer.as_raw_fd(),
                DaemonStage::SecondFork,
                last_errno(),
            );
        }
        if raw_daemon > 0 {
            child_exit(0);
        }

        if let Some(signal_state) = &self.signal_state
            && let Err(errno) = signal_state.apply_in_child()
        {
            report_and_exit(
                self.status_writer.as_raw_fd(),
                DaemonStage::SignalState,
                errno,
            );
        }

        if let Some(directory) = &self.current_directory
            // SAFETY: directory is a live NUL-terminated CString.
            && unsafe { libc::chdir(directory.as_ptr()) } == -1
        {
            report_and_exit(
                self.status_writer.as_raw_fd(),
                DaemonStage::CurrentDirectory,
                last_errno(),
            );
        }
        self.descriptors
            .apply_before_return()
            .unwrap_or_else(|errno| {
                report_and_exit(
                    self.status_writer.as_raw_fd(),
                    DaemonStage::Descriptors,
                    errno,
                )
            });

        let process = ProcessId::new(current_pid()).unwrap_or_else(|| {
            report_and_exit(
                self.status_writer.as_raw_fd(),
                DaemonStage::Handshake,
                libc::EIO,
            )
        });
        if write_record(
            self.status_writer.as_raw_fd(),
            WireRecord::new(RECORD_DAEMON, DaemonStage::Handshake, process.get()),
        )
        .is_err()
        {
            child_exit(FAILURE_EXIT);
        }
        CheckedDaemon::Daemon(DaemonNotifier {
            descriptor: self.status_writer,
            process: DaemonProcess {
                process,
                process_group: group,
            },
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct WireRecord {
    magic: u32,
    value: i32,
    kind: u8,
    stage: u8,
    reserved: [u8; 2],
}

impl WireRecord {
    const fn new(kind: u8, stage: DaemonStage, value: i32) -> Self {
        Self {
            magic: DAEMON_MAGIC,
            value,
            kind,
            stage: stage as u8,
            reserved: [0; 2],
        }
    }
}

fn write_record(descriptor: RawFd, record: WireRecord) -> Result<(), libc::c_int> {
    // SAFETY: record remains live and immutable for the duration of write_all.
    let bytes = unsafe {
        std::slice::from_raw_parts((&raw const record).cast::<u8>(), size_of::<WireRecord>())
    };
    write_all(descriptor, bytes)
}

fn parent_wait_ready(
    reader: &OwnedFd,
    intermediate: ProcessId,
    timeout: Duration,
) -> Result<DaemonProcess, (DaemonError, Option<ProcessGroupId>)> {
    let started = Instant::now();
    let mut group = None;
    let mut daemon = None;
    loop {
        let record = match read_record(reader.as_raw_fd(), started, timeout) {
            Ok(Some(record)) => record,
            Ok(None) => {
                return Err((
                    DaemonError::Abandoned {
                        cleanup_pending: DaemonCleanup::default(),
                    },
                    group,
                ));
            }
            Err(error) => return Err((error, group)),
        };
        if record.magic != DAEMON_MAGIC {
            return Err((DaemonError::invalid_handshake(), group));
        }
        match record.kind {
            RECORD_SESSION
                if group.is_none()
                    && daemon.is_none()
                    && record.stage == DaemonStage::CreateSession as u8
                    && record.value == intermediate.get() =>
            {
                group = ProcessGroupId::new(record.value);
                if group.is_none() {
                    return Err((DaemonError::invalid_handshake(), None));
                }
            }
            RECORD_DAEMON
                if group.is_some()
                    && daemon.is_none()
                    && record.stage == DaemonStage::Handshake as u8
                    && record.value != intermediate.get() =>
            {
                daemon = ProcessId::new(record.value);
                if daemon.is_none() {
                    return Err((DaemonError::invalid_handshake(), group));
                }
            }
            RECORD_READY
                if group.is_some()
                    && daemon.is_some()
                    && record.stage == DaemonStage::Initialization as u8
                    && record.value == 0 =>
            {
                if let Err(error) = wait_for_intermediate(intermediate, started, timeout) {
                    return Err((error, group));
                }
                return Ok(DaemonProcess {
                    process: daemon.ok_or_else(|| (DaemonError::invalid_handshake(), group))?,
                    process_group: group
                        .ok_or_else(|| (DaemonError::invalid_handshake(), group))?,
                });
            }
            RECORD_FAILURE if record.value > 0 => {
                let Some(stage) = daemon_stage_from_wire(record.stage) else {
                    return Err((DaemonError::invalid_handshake(), group));
                };
                return Err((
                    DaemonError::operating_system(
                        stage,
                        io::Error::from_raw_os_error(record.value),
                    ),
                    group,
                ));
            }
            _ => return Err((DaemonError::invalid_handshake(), group)),
        }
    }
}

fn read_record(
    descriptor: RawFd,
    started: Instant,
    timeout: Duration,
) -> Result<Option<WireRecord>, DaemonError> {
    let mut bytes = [0_u8; size_of::<WireRecord>()];
    let mut offset = 0;
    while offset < bytes.len() {
        wait_readable(descriptor, started, timeout)?;
        let Some(remaining) = bytes.get_mut(offset..) else {
            return Err(DaemonError::invalid_handshake());
        };
        // SAFETY: remaining is writable and descriptor is the live pipe reader.
        let result =
            unsafe { libc::read(descriptor, remaining.as_mut_ptr().cast(), remaining.len()) };
        match result.cmp(&0) {
            Ordering::Greater => offset += result.unsigned_abs(),
            Ordering::Equal => {
                return if offset == 0 {
                    Ok(None)
                } else {
                    Err(DaemonError::invalid_handshake())
                };
            }
            Ordering::Less => {
                let errno = last_errno();
                if errno != libc::EINTR {
                    return Err(DaemonError::operating_system(
                        DaemonStage::Handshake,
                        io::Error::from_raw_os_error(errno),
                    ));
                }
            }
        }
    }
    // SAFETY: bytes has the exact WireRecord size; unaligned avoids alignment assumptions.
    Ok(Some(unsafe {
        std::ptr::read_unaligned(bytes.as_ptr().cast::<WireRecord>())
    }))
}

fn wait_readable(
    descriptor: RawFd,
    started: Instant,
    timeout: Duration,
) -> Result<(), DaemonError> {
    loop {
        let remaining = timeout.saturating_sub(started.elapsed());
        let mut poll_descriptor = libc::pollfd {
            fd: descriptor,
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        // SAFETY: poll_descriptor points to one initialized pollfd.
        let result = unsafe {
            libc::poll(
                &raw mut poll_descriptor,
                1,
                duration_to_poll_timeout(remaining),
            )
        };
        if result > 0 {
            return Ok(());
        }
        if result == 0 {
            return Err(DaemonError::timed_out(timeout));
        }
        let errno = last_errno();
        if errno != libc::EINTR {
            return Err(DaemonError::operating_system(
                DaemonStage::Handshake,
                io::Error::from_raw_os_error(errno),
            ));
        }
    }
}

fn wait_for_intermediate(
    intermediate: ProcessId,
    started: Instant,
    timeout: Duration,
) -> Result<(), DaemonError> {
    loop {
        match wait_event_nohang(intermediate) {
            Ok(Some(ChildEvent::Exited { code: 0, .. })) => return Ok(()),
            Ok(Some(event)) if event.is_terminal() => {
                return Err(DaemonError::invalid_handshake());
            }
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
                return Err(DaemonError::invalid_handshake());
            }
            Err(error) => {
                return Err(DaemonError::operating_system(DaemonStage::Handshake, error));
            }
            Ok(Some(_) | None) => {}
        }
        if started.elapsed() >= timeout {
            return Err(DaemonError::timed_out(timeout));
        }
        thread::sleep(CLEANUP_POLL_INTERVAL);
    }
}

fn cleanup_daemon(intermediate: ProcessId, group: Option<ProcessGroupId>) -> DaemonCleanup {
    if let Some(group) = group {
        let _ = signal_process_group(group, Signal::KILL);
    }
    let _ = signal_process(intermediate, Signal::KILL);

    let started = Instant::now();
    let intermediate_pending = loop {
        match wait_event_nohang(intermediate) {
            Ok(Some(event)) if event.is_terminal() => break None,
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => break None,
            Err(_) => break Some(intermediate),
            Ok(Some(_) | None) => {}
        }
        if started.elapsed() >= CLEANUP_TIMEOUT {
            break Some(intermediate);
        }
        thread::sleep(CLEANUP_POLL_INTERVAL);
    };

    let group_started = Instant::now();
    let group_pending = loop {
        let Some(owned) = group else {
            break None;
        };
        if !group_exists(owned) {
            break None;
        }
        if group_started.elapsed() >= CLEANUP_TIMEOUT {
            break Some(owned);
        }
        thread::sleep(CLEANUP_POLL_INTERVAL);
    };
    DaemonCleanup {
        intermediate: intermediate_pending,
        process_group: group_pending,
    }
}

fn group_exists(group: ProcessGroupId) -> bool {
    loop {
        // SAFETY: signal zero is used internally only to verify bounded cleanup
        // of a process group whose creation was confirmed by the handshake.
        let result = unsafe { libc::kill(-group.get(), 0) };
        if result == 0 {
            return true;
        }
        let errno = last_errno();
        if errno == libc::EINTR {
            continue;
        }
        return errno != libc::ESRCH;
    }
}

fn retry_setsid() -> Result<libc::pid_t, libc::c_int> {
    loop {
        // SAFETY: setsid has no pointer arguments and runs in the first child.
        let result = unsafe { libc::setsid() };
        if result >= 0 {
            return Ok(result);
        }
        let errno = last_errno();
        if errno != libc::EINTR {
            return Err(errno);
        }
    }
}

fn report_and_exit(descriptor: RawFd, stage: DaemonStage, errno: libc::c_int) -> ! {
    let _ = write_record(descriptor, WireRecord::new(RECORD_FAILURE, stage, errno));
    child_exit(FAILURE_EXIT)
}

fn current_pid() -> libc::pid_t {
    // SAFETY: getpid has no failure mode or pointer arguments.
    unsafe { libc::getpid() }
}

fn child_exit(code: libc::c_int) -> ! {
    // SAFETY: explicit child termination avoids inherited Rust destructors.
    unsafe { libc::_exit(code) }
}

fn duration_to_poll_timeout(duration: Duration) -> libc::c_int {
    if duration.is_zero() {
        return 0;
    }
    libc::c_int::try_from(duration.as_millis().max(1)).unwrap_or(libc::c_int::MAX)
}

const fn daemon_stage_from_wire(stage: u8) -> Option<DaemonStage> {
    match stage {
        2 => Some(DaemonStage::CreateSession),
        3 => Some(DaemonStage::SecondFork),
        4 => Some(DaemonStage::CurrentDirectory),
        5 => Some(DaemonStage::Descriptors),
        6 => Some(DaemonStage::Initialization),
        7 => Some(DaemonStage::Handshake),
        8 => Some(DaemonStage::SignalState),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{DaemonStage, WireRecord, daemon_stage_from_wire};

    #[test]
    fn wire_record_has_fixed_pipe_friendly_size() {
        assert_eq!(size_of::<WireRecord>(), 12);
    }

    #[test]
    fn wire_stage_rejects_parent_only_first_fork() {
        assert_eq!(daemon_stage_from_wire(2), Some(DaemonStage::CreateSession));
        assert_eq!(daemon_stage_from_wire(6), Some(DaemonStage::Initialization));
        assert_eq!(daemon_stage_from_wire(8), Some(DaemonStage::SignalState));
        assert_eq!(daemon_stage_from_wire(1), None);
    }
}
