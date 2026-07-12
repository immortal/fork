use std::{
    collections::BTreeMap,
    error::Error,
    ffi::{CString, OsStr, OsString},
    fmt::{self, Display, Formatter},
    fs::File,
    io::{self, Read},
    os::{
        fd::{AsRawFd, OwnedFd, RawFd},
        unix::ffi::OsStrExt,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    ChildSignalState, ProcessGroupId, ProcessId, Signal,
    child_signal::Prepared as PreparedSignalState,
    descriptor::{Actions as DescriptorActions, Prepared as PreparedDescriptors, close_raw},
    pipe_cloexec,
    raw::{last_errno, write_all},
    signal_process, wait_event_nohang,
};

const FAILURE_MAGIC: u32 = 0x494d_4d4f;
const CHILD_FAILURE_EXIT: libc::c_int = 127;
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);
const CLEANUP_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Process-group setup requested for a spawned program.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum ProcessGroup {
    /// Keep the broker's process group.
    #[default]
    Inherit,
    /// Make the child the leader of a new process group.
    New,
    /// Join an existing process group.
    Join(ProcessGroupId),
}

/// Supplementary-group behavior for a child identity transition.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum SupplementaryGroups {
    /// Retain the broker's supplementary groups explicitly.
    Preserve,
    /// Replace the list, or clear it when the vector is empty.
    Set(Vec<libc::gid_t>),
}

/// Numeric credentials materialized before `fork`.
///
/// Account-name and group-database resolution belongs in the caller. The child
/// applies supplementary groups first, then the primary GID, then the UID so
/// privilege cannot be lost before group setup is complete.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ProcessCredentials {
    user: libc::uid_t,
    group: libc::gid_t,
    supplementary_groups: SupplementaryGroups,
}

impl ProcessCredentials {
    /// Construct a numeric identity transition.
    #[must_use]
    pub const fn new(
        user: libc::uid_t,
        group: libc::gid_t,
        supplementary_groups: SupplementaryGroups,
    ) -> Self {
        Self {
            user,
            group,
            supplementary_groups,
        }
    }

    /// Return the target UID.
    #[must_use]
    pub const fn user(&self) -> libc::uid_t {
        self.user
    }

    /// Return the target primary GID.
    #[must_use]
    pub const fn group(&self) -> libc::gid_t {
        self.group
    }

    /// Return the requested supplementary-group behavior.
    #[must_use]
    pub const fn supplementary_groups(&self) -> &SupplementaryGroups {
        &self.supplementary_groups
    }
}

/// A reviewed child-side operation that can fail before `execve`.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[repr(u8)]
pub enum SpawnStage {
    Fork = 1,
    ProcessGroup = 2,
    CurrentDirectory = 3,
    DescriptorDuplication = 4,
    Execute = 5,
    StartupHandshake = 6,
    Identity = 7,
    SignalState = 8,
}

impl Display for SpawnStage {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Fork => "fork",
            Self::ProcessGroup => "process-group setup",
            Self::CurrentDirectory => "working-directory change",
            Self::DescriptorDuplication => "descriptor mapping",
            Self::Execute => "execve",
            Self::StartupHandshake => "startup handshake",
            Self::Identity => "identity transition",
            Self::SignalState => "signal-state reset",
        };
        formatter.write_str(name)
    }
}

/// Failure to fork, prepare, or execute a child process.
#[derive(Debug)]
pub enum SpawnError {
    /// An operating-system operation failed at a known startup stage.
    OperatingSystem {
        stage: SpawnStage,
        error: io::Error,
        cleanup_pending: Option<ProcessId>,
    },
    /// The child did not complete its exec handshake before the deadline.
    TimedOut {
        timeout: Duration,
        cleanup_pending: Option<ProcessId>,
    },
    /// The child returned a malformed startup record.
    InvalidHandshake { cleanup_pending: Option<ProcessId> },
}

impl SpawnError {
    fn operating_system(stage: SpawnStage, error: io::Error) -> Self {
        Self::OperatingSystem {
            stage,
            error,
            cleanup_pending: None,
        }
    }

    fn with_cleanup_pending(self, process: Option<ProcessId>) -> Self {
        match self {
            Self::OperatingSystem { stage, error, .. } => Self::OperatingSystem {
                stage,
                error,
                cleanup_pending: process,
            },
            Self::TimedOut { timeout, .. } => Self::TimedOut {
                timeout,
                cleanup_pending: process,
            },
            Self::InvalidHandshake { .. } => Self::InvalidHandshake {
                cleanup_pending: process,
            },
        }
    }

    /// Return a killed but not yet reaped child that the broker must retain.
    ///
    /// This is normally `None`. A value is returned only when bounded cleanup
    /// could not observe termination, for example while the child is stuck in
    /// uninterruptible kernel I/O. The PID remains a live ownership obligation
    /// and must be reaped through the normal child-event loop.
    #[must_use]
    pub const fn cleanup_pending(&self) -> Option<ProcessId> {
        match self {
            Self::OperatingSystem {
                cleanup_pending, ..
            }
            | Self::TimedOut {
                cleanup_pending, ..
            }
            | Self::InvalidHandshake { cleanup_pending } => *cleanup_pending,
        }
    }
}

impl Display for SpawnError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::OperatingSystem { stage, error, .. } => {
                write!(formatter, "child {stage} failed: {error}")
            }
            Self::TimedOut { timeout, .. } => {
                write!(formatter, "child startup timed out after {timeout:?}")
            }
            Self::InvalidHandshake { .. } => {
                formatter.write_str("child returned an invalid handshake")
            }
        }
    }
}

impl Error for SpawnError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::OperatingSystem { error, .. } => Some(error),
            Self::TimedOut { .. } | Self::InvalidHandshake { .. } => None,
        }
    }
}

/// A successfully executed direct child of the process broker.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SpawnedChild {
    process: ProcessId,
    group: Option<ProcessGroupId>,
}

impl SpawnedChild {
    /// Return the live child process handle.
    #[must_use]
    pub const fn process(&self) -> ProcessId {
        self.process
    }

    /// Return the explicitly configured process group, when any.
    #[must_use]
    pub const fn process_group(&self) -> Option<ProcessGroupId> {
        self.group
    }
}

/// A command whose child-visible data is materialized before `fork`.
///
/// The executable path is passed directly to `execve`; no shell or `PATH`
/// lookup is performed. Arguments, environment, working directory, descriptor
/// mappings, pointer arrays, and close bounds are all prepared in the broker
/// process. The child performs only reviewed system calls before `execve` or
/// `_exit`.
#[derive(Debug)]
pub struct PreparedCommand {
    program: CString,
    arguments: Vec<CString>,
    environment: BTreeMap<OsString, OsString>,
    current_directory: Option<CString>,
    process_group: ProcessGroup,
    credentials: Option<ProcessCredentials>,
    signal_state: ChildSignalState,
    descriptors: DescriptorActions,
}

impl PreparedCommand {
    /// Create a direct-exec command and snapshot the broker's environment.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when the path is empty or contains a NUL byte.
    pub fn new(program: impl AsRef<OsStr>) -> io::Result<Self> {
        let program = os_string_to_cstring(program.as_ref(), "program")?;
        if program.as_bytes().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "program must not be empty",
            ));
        }
        Ok(Self {
            program,
            arguments: Vec::new(),
            environment: std::env::vars_os().collect(),
            current_directory: None,
            process_group: ProcessGroup::Inherit,
            credentials: None,
            signal_state: ChildSignalState::Reset,
            descriptors: DescriptorActions::default(),
        })
    }

    /// Append one argument without invoking a shell.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when the argument contains a NUL byte.
    pub fn arg(&mut self, argument: impl AsRef<OsStr>) -> io::Result<&mut Self> {
        self.arguments
            .push(os_string_to_cstring(argument.as_ref(), "argument")?);
        Ok(self)
    }

    /// Remove every inherited environment entry.
    pub fn clear_environment(&mut self) -> &mut Self {
        self.environment.clear();
        self
    }

    /// Set one child environment entry.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when the key is empty, contains `=` or NUL, or
    /// when the value contains NUL.
    pub fn environment(
        &mut self,
        key: impl AsRef<OsStr>,
        value: impl AsRef<OsStr>,
    ) -> io::Result<&mut Self> {
        let key = key.as_ref();
        let key_bytes = key.as_bytes();
        if key_bytes.is_empty() || key_bytes.contains(&b'=') || key_bytes.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "environment key must be nonempty and contain neither '=' nor NUL",
            ));
        }
        if value.as_ref().as_bytes().contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "environment value must not contain NUL",
            ));
        }
        self.environment
            .insert(key.to_os_string(), value.as_ref().to_os_string());
        Ok(self)
    }

    /// Set the working directory used immediately before `execve`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when the path contains a NUL byte.
    pub fn current_directory(&mut self, path: impl AsRef<OsStr>) -> io::Result<&mut Self> {
        self.current_directory = Some(os_string_to_cstring(path.as_ref(), "working directory")?);
        Ok(self)
    }

    /// Configure child process-group membership.
    pub fn process_group(&mut self, group: ProcessGroup) -> &mut Self {
        self.process_group = group;
        self
    }

    /// Configure a numeric UID/GID transition performed immediately before exec.
    pub fn credentials(&mut self, credentials: ProcessCredentials) -> &mut Self {
        self.credentials = Some(credentials);
        self
    }

    /// Configure whether the child resets or inherits signal state.
    pub fn signal_state(&mut self, state: ChildSignalState) -> &mut Self {
        self.signal_state = state;
        self
    }

    /// Map an owned source descriptor onto a child descriptor number.
    ///
    /// The source is duplicated above every requested target before `fork`, so
    /// cyclic and overlapping mappings cannot overwrite another source. The
    /// target is the only descriptor inherited through a successful `exec`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a negative or duplicate target.
    pub fn map_descriptor(&mut self, source: OwnedFd, target: RawFd) -> io::Result<&mut Self> {
        self.descriptors.map(source, target)?;
        Ok(self)
    }

    /// Preserve an owned descriptor at its current number through `execve`.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when that descriptor is already mapped or closed.
    pub fn inherit_descriptor(&mut self, descriptor: OwnedFd) -> io::Result<&mut Self> {
        self.descriptors.inherit(descriptor)?;
        Ok(self)
    }

    /// Explicitly close a descriptor in the child before `execve`.
    ///
    /// This is primarily useful for standard input, output, or error because
    /// every descriptor above standard error is already closed unless mapped or
    /// explicitly inherited.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a negative or mapped descriptor.
    pub fn close_descriptor(&mut self, descriptor: RawFd) -> io::Result<&mut Self> {
        self.descriptors.close(descriptor)?;
        Ok(self)
    }

    /// Fork and execute the command with a bounded startup handshake.
    ///
    /// Success means `execve` completed and closed the handshake descriptor,
    /// not that the program remains running. Every failure path terminates and
    /// reaps the pre-exec child before returning.
    ///
    /// # Errors
    ///
    /// Returns a typed startup stage and OS error, a timeout, or a malformed
    /// handshake error.
    pub fn spawn(self, startup_timeout: Duration) -> Result<SpawnedChild, SpawnError> {
        SpawnPreparation::new(self)?.spawn(startup_timeout)
    }
}

struct SpawnPreparation {
    program: CString,
    _arguments: Vec<CString>,
    argument_pointers: Vec<*const libc::c_char>,
    _environment: Vec<CString>,
    environment_pointers: Vec<*const libc::c_char>,
    current_directory: Option<CString>,
    process_group: ProcessGroup,
    credentials: Option<ProcessCredentials>,
    signal_state: Option<PreparedSignalState>,
    descriptors: PreparedDescriptors,
    status_reader: OwnedFd,
    status_writer: OwnedFd,
}

impl SpawnPreparation {
    fn new(command: PreparedCommand) -> Result<Self, SpawnError> {
        let pipe = pipe_cloexec()
            .map_err(|error| SpawnError::operating_system(SpawnStage::StartupHandshake, error))?;
        let (status_reader, initial_status_writer) = pipe.into_parts();
        let (descriptors, status_writer) = command
            .descriptors
            .prepare(initial_status_writer)
            .map_err(|error| {
                SpawnError::operating_system(SpawnStage::DescriptorDuplication, error)
            })?;

        let environment = materialize_environment(command.environment)
            .map_err(|error| SpawnError::operating_system(SpawnStage::Execute, error))?;
        let mut arguments = Vec::with_capacity(command.arguments.len() + 1);
        arguments.push(command.program.clone());
        arguments.extend(command.arguments);
        let argument_pointers = cstring_pointers(&arguments);
        let environment_pointers = cstring_pointers(&environment);

        validate_credentials(command.credentials.as_ref())
            .map_err(|error| SpawnError::operating_system(SpawnStage::Identity, error))?;
        let signal_state = PreparedSignalState::new(command.signal_state)
            .map_err(|error| SpawnError::operating_system(SpawnStage::SignalState, error))?;

        Ok(Self {
            program: command.program,
            _arguments: arguments,
            argument_pointers,
            _environment: environment,
            environment_pointers,
            current_directory: command.current_directory,
            process_group: command.process_group,
            credentials: command.credentials,
            signal_state,
            descriptors,
            status_reader,
            status_writer,
        })
    }

    fn spawn(self, startup_timeout: Duration) -> Result<SpawnedChild, SpawnError> {
        // SAFETY: all child-visible data and pointer arrays are fully prepared;
        // the child branch calls only the reviewed syscall-only routine below.
        let raw_child = unsafe { libc::fork() };
        if raw_child == -1 {
            return Err(SpawnError::operating_system(
                SpawnStage::Fork,
                io::Error::last_os_error(),
            ));
        }
        if raw_child == 0 {
            self.run_child();
        }

        let process = ProcessId::new(raw_child).ok_or_else(|| {
            SpawnError::operating_system(
                SpawnStage::Fork,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "fork returned a nonpositive child PID",
                ),
            )
        })?;
        drop(self.status_writer);
        parent_set_process_group(process, self.process_group);

        let group = match self.process_group {
            ProcessGroup::Inherit => None,
            ProcessGroup::New => ProcessGroupId::new(process.get()),
            ProcessGroup::Join(group) => Some(group),
        };
        let result = read_startup_handshake(self.status_reader, startup_timeout);
        match result {
            Ok(()) => Ok(SpawnedChild { process, group }),
            Err(error) => {
                let cleanup_pending = terminate_and_reap_bounded(process);
                Err(error.with_cleanup_pending(cleanup_pending))
            }
        }
    }

    fn run_child(&self) -> ! {
        close_raw(self.status_reader.as_raw_fd());

        if let Some(signal_state) = &self.signal_state
            && let Err(errno) = signal_state.apply_in_child()
        {
            child_fail(
                self.status_writer.as_raw_fd(),
                SpawnStage::SignalState,
                errno,
            );
        }

        if let Err(errno) = child_set_process_group(self.process_group) {
            child_fail(
                self.status_writer.as_raw_fd(),
                SpawnStage::ProcessGroup,
                errno,
            );
        }
        if let Some(directory) = &self.current_directory {
            // SAFETY: the directory is a live, NUL-terminated CString.
            if unsafe { libc::chdir(directory.as_ptr()) } == -1 {
                child_fail(
                    self.status_writer.as_raw_fd(),
                    SpawnStage::CurrentDirectory,
                    last_errno(),
                );
            }
        }
        if let Err(errno) = self.descriptors.apply_in_child() {
            child_fail(
                self.status_writer.as_raw_fd(),
                SpawnStage::DescriptorDuplication,
                errno,
            );
        }

        if let Some(credentials) = &self.credentials
            && let Err(errno) = apply_credentials(credentials)
        {
            child_fail(self.status_writer.as_raw_fd(), SpawnStage::Identity, errno);
        }

        // The backing CString vectors remain fields of `self` through exec;
        // their pointer arrays were prepared before fork and end with null.
        // SAFETY: program, argv, and envp are valid C strings/pointer arrays.
        unsafe {
            libc::execve(
                self.program.as_ptr(),
                self.argument_pointers.as_ptr(),
                self.environment_pointers.as_ptr(),
            );
        }
        child_fail(
            self.status_writer.as_raw_fd(),
            SpawnStage::Execute,
            last_errno(),
        );
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FailureRecord {
    magic: u32,
    errno: i32,
    stage: u8,
    reserved: [u8; 3],
}

fn os_string_to_cstring(value: &OsStr, label: &str) -> io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} must not contain NUL"),
        )
    })
}

fn materialize_environment(environment: BTreeMap<OsString, OsString>) -> io::Result<Vec<CString>> {
    environment
        .into_iter()
        .map(|(key, value)| {
            let mut entry = Vec::with_capacity(key.as_bytes().len() + value.as_bytes().len() + 1);
            entry.extend_from_slice(key.as_bytes());
            entry.push(b'=');
            entry.extend_from_slice(value.as_bytes());
            CString::new(entry).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "environment contains NUL")
            })
        })
        .collect()
}

fn cstring_pointers(strings: &[CString]) -> Vec<*const libc::c_char> {
    let mut pointers = Vec::with_capacity(strings.len() + 1);
    pointers.extend(strings.iter().map(|value| value.as_ptr()));
    pointers.push(std::ptr::null());
    pointers
}

fn parent_set_process_group(process: ProcessId, group: ProcessGroup) {
    let target = match group {
        ProcessGroup::Inherit => return,
        ProcessGroup::New => process.get(),
        ProcessGroup::Join(group) => group.get(),
    };
    loop {
        // SAFETY: checked positive identifiers are passed to setpgid.
        if unsafe { libc::setpgid(process.get(), target) } == 0 {
            return;
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        // The child performs the same operation and reports authoritative
        // failure through the handshake. EACCES/ESRCH mean it won the race.
        return;
    }
}

fn read_startup_handshake(descriptor: OwnedFd, timeout: Duration) -> Result<(), SpawnError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    let mut poll_descriptor = libc::pollfd {
        fd: descriptor.as_raw_fd(),
        events: libc::POLLIN | libc::POLLHUP,
        revents: 0,
    };
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout_millis = duration_to_poll_timeout(remaining);
        // SAFETY: poll_descriptor points to one initialized pollfd.
        let result = unsafe { libc::poll(&raw mut poll_descriptor, 1, timeout_millis) };
        if result > 0 {
            break;
        }
        if result == 0 {
            return Err(SpawnError::TimedOut {
                timeout,
                cleanup_pending: None,
            });
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(SpawnError::operating_system(
                SpawnStage::StartupHandshake,
                error,
            ));
        }
    }

    let mut file = File::from(descriptor);
    let mut bytes = [0_u8; size_of::<FailureRecord>()];
    let mut read = 0;
    while read < bytes.len() {
        let remaining = bytes.get_mut(read..).ok_or(SpawnError::InvalidHandshake {
            cleanup_pending: None,
        })?;
        match file.read(remaining) {
            Ok(0) if read == 0 => return Ok(()),
            Ok(0) => {
                return Err(SpawnError::InvalidHandshake {
                    cleanup_pending: None,
                });
            }
            Ok(count) => read += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                return Err(SpawnError::operating_system(
                    SpawnStage::StartupHandshake,
                    error,
                ));
            }
        }
    }

    // SAFETY: FailureRecord is Copy and bytes has its exact size; read_unaligned
    // avoids imposing alignment on the byte array.
    let record = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<FailureRecord>()) };
    if record.magic != FAILURE_MAGIC {
        return Err(SpawnError::InvalidHandshake {
            cleanup_pending: None,
        });
    }
    let stage = spawn_stage_from_wire(record.stage).ok_or(SpawnError::InvalidHandshake {
        cleanup_pending: None,
    })?;
    Err(SpawnError::operating_system(
        stage,
        io::Error::from_raw_os_error(record.errno),
    ))
}

fn duration_to_poll_timeout(duration: Duration) -> libc::c_int {
    if duration.is_zero() {
        return 0;
    }
    let millis = duration.as_millis().max(1);
    libc::c_int::try_from(millis).unwrap_or(libc::c_int::MAX)
}

const fn spawn_stage_from_wire(stage: u8) -> Option<SpawnStage> {
    match stage {
        2 => Some(SpawnStage::ProcessGroup),
        3 => Some(SpawnStage::CurrentDirectory),
        4 => Some(SpawnStage::DescriptorDuplication),
        5 => Some(SpawnStage::Execute),
        7 => Some(SpawnStage::Identity),
        8 => Some(SpawnStage::SignalState),
        _ => None,
    }
}

fn validate_credentials(credentials: Option<&ProcessCredentials>) -> io::Result<()> {
    let Some(ProcessCredentials {
        supplementary_groups: SupplementaryGroups::Set(groups),
        ..
    }) = credentials
    else {
        return Ok(());
    };
    let maximum = loop {
        // SAFETY: sysconf reads the process group-count limit without pointers.
        let result = unsafe { libc::sysconf(libc::_SC_NGROUPS_MAX) };
        if result >= 0 {
            break usize::try_from(result).unwrap_or(usize::MAX);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    };
    if groups.len() > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "supplementary group count exceeds the operating-system limit",
        ));
    }
    Ok(())
}

fn terminate_and_reap_bounded(process: ProcessId) -> Option<ProcessId> {
    let _ = signal_process(process, Signal::KILL);
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    loop {
        match wait_event_nohang(process) {
            Ok(Some(event)) if event.is_terminal() => return None,
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => return None,
            Err(_) => return Some(process),
            Ok(Some(_) | None) => {}
        }
        if Instant::now() >= deadline {
            return Some(process);
        }
        thread::sleep(CLEANUP_POLL_INTERVAL);
    }
}

fn child_set_process_group(group: ProcessGroup) -> Result<(), libc::c_int> {
    let target = match group {
        ProcessGroup::Inherit => return Ok(()),
        ProcessGroup::New => 0,
        ProcessGroup::Join(group) => group.get(),
    };
    loop {
        // SAFETY: pid zero targets the current child; target is zero or checked positive.
        if unsafe { libc::setpgid(0, target) } == 0 {
            return Ok(());
        }
        let errno = last_errno();
        if errno != libc::EINTR {
            return Err(errno);
        }
    }
}

fn apply_credentials(credentials: &ProcessCredentials) -> Result<(), libc::c_int> {
    if let SupplementaryGroups::Set(groups) = &credentials.supplementary_groups {
        loop {
            if set_supplementary_groups(groups) == 0 {
                break;
            }
            let errno = last_errno();
            if errno != libc::EINTR {
                return Err(errno);
            }
        }
    }
    loop {
        // SAFETY: the numeric primary GID was fully materialized before fork.
        if unsafe { libc::setgid(credentials.group) } == 0 {
            break;
        }
        let errno = last_errno();
        if errno != libc::EINTR {
            return Err(errno);
        }
    }
    loop {
        // SAFETY: the numeric UID was fully materialized before fork.
        if unsafe { libc::setuid(credentials.user) } == 0 {
            return Ok(());
        }
        let errno = last_errno();
        if errno != libc::EINTR {
            return Err(errno);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn set_supplementary_groups(groups: &[libc::gid_t]) -> libc::c_int {
    // SAFETY: the group slice was bounded before fork and remains live.
    unsafe { libc::setgroups(groups.len(), groups.as_ptr()) }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn set_supplementary_groups(groups: &[libc::gid_t]) -> libc::c_int {
    let Ok(count) = libc::c_int::try_from(groups.len()) else {
        return -1;
    };
    // SAFETY: the group slice was bounded before fork and remains live.
    unsafe { libc::setgroups(count, groups.as_ptr()) }
}

fn child_fail(descriptor: RawFd, stage: SpawnStage, errno: libc::c_int) -> ! {
    let record = FailureRecord {
        magic: FAILURE_MAGIC,
        errno,
        stage: stage as u8,
        reserved: [0; 3],
    };
    // SAFETY: record remains live and immutable while the byte view is written.
    let bytes = unsafe {
        std::slice::from_raw_parts((&raw const record).cast::<u8>(), size_of::<FailureRecord>())
    };
    let _ = write_all(descriptor, bytes);
    // SAFETY: _exit terminates only the fork child without running destructors.
    unsafe { libc::_exit(CHILD_FAILURE_EXIT) }
}

#[cfg(test)]
mod tests {
    use super::{
        SpawnError, SpawnStage, duration_to_poll_timeout, read_startup_handshake,
        spawn_stage_from_wire,
    };
    use crate::pipe_cloexec;
    use std::{error::Error, thread, time::Duration};

    #[test]
    fn wire_stages_reject_parent_only_values() {
        assert_eq!(spawn_stage_from_wire(2), Some(SpawnStage::ProcessGroup));
        assert_eq!(spawn_stage_from_wire(5), Some(SpawnStage::Execute));
        assert_eq!(spawn_stage_from_wire(7), Some(SpawnStage::Identity));
        assert_eq!(spawn_stage_from_wire(8), Some(SpawnStage::SignalState));
        assert_eq!(spawn_stage_from_wire(1), None);
        assert_eq!(spawn_stage_from_wire(255), None);
    }

    #[test]
    fn poll_timeout_rounds_up_and_saturates() {
        assert_eq!(duration_to_poll_timeout(Duration::ZERO), 0);
        assert_eq!(duration_to_poll_timeout(Duration::from_nanos(1)), 1);
        assert_eq!(
            duration_to_poll_timeout(Duration::from_secs(u64::MAX)),
            libc::c_int::MAX
        );
    }

    #[test]
    fn startup_handshake_obeys_timeout() -> Result<(), Box<dyn Error>> {
        let pipe = pipe_cloexec()?;
        let (reader, writer) = pipe.into_parts();
        let holder = thread::spawn(move || {
            thread::sleep(Duration::from_millis(25));
            drop(writer);
        });
        let Err(error) = read_startup_handshake(reader, Duration::from_millis(1)) else {
            return Err(std::io::Error::other("open handshake did not time out").into());
        };
        assert!(matches!(error, SpawnError::TimedOut { .. }));
        holder
            .join()
            .map_err(|_| std::io::Error::other("handshake holder panicked"))?;
        Ok(())
    }
}
