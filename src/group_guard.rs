//! Fail-closed process-group lifetime guard.
//!
//! A short-lived anchor reserves the group before a workload can join it. An
//! out-of-group helper then owns no application descriptors and waits on one
//! close-on-exec socket. Owner loss closes that socket and makes the helper kill
//! the complete group. Explicit disarm writes one byte before closure, proving
//! that cleanup policy—not incidental descriptor loss—ended the guard lifetime.
//!
//! The flow is `new` (reserve group and start both helpers), spawn the workload
//! into [`ProcessGroupGuard::process_group`], `activate` (retire the anchor), and
//! finally `disarm` for normal cleanup. Dropping an armed guard is fail-closed.
//! The owning broker must serialize helper cleanup with all-child event
//! collection; once a terminal event has been reaped, its numeric PID is never
//! safe to signal again.

use std::{
    io,
    os::fd::{AsRawFd, OwnedFd, RawFd},
    time::{Duration, Instant},
};

use crate::{
    ProcessFork, ProcessGroupId, ProcessId, Signal, create_process_group,
    descriptor::GuardPrepared, fork_process, signal_process, socket_pair_cloexec, wait_event,
    wait_event_nohang,
};

const DISARM_RECORD: u8 = 0xa5;
const DROP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);
const FAILURE_EXIT: libc::c_int = 70;
const INITIAL_POLL_INTERVAL: Duration = Duration::from_micros(50);
const MAX_POLL_INTERVAL: Duration = Duration::from_millis(5);
const YIELD_ATTEMPTS: u8 = 8;

/// An owned fail-closed capability for one reserved process group.
///
/// Dropping an armed value closes both owner sockets. The out-of-group guard
/// then sends `SIGKILL` to the group and both helper children are reaped within
/// the caller's process. Call [`Self::activate`] only after the workload has
/// successfully joined the reserved group, and [`Self::disarm`] only after the
/// group no longer requires broker-death containment.
///
/// This capability contains process-group membership, not arbitrary
/// descendants. A workload which creates a session or changes group has left
/// the portable boundary. The caller must also disarm immediately after
/// reaping the final owned member: Unix provides no persistent handle which
/// prevents an empty process-group ID from being reused.
#[must_use = "dropping an armed process-group guard triggers fail-closed cleanup"]
#[derive(Debug)]
pub struct ProcessGroupGuard {
    anchor: Option<GuardHelper>,
    group: ProcessGroupId,
    guard: Option<GuardHelper>,
    guard_process: ProcessId,
}

impl ProcessGroupGuard {
    /// Reserve a process group and establish an out-of-group owner-loss guard.
    ///
    /// Call this from a dedicated single-threaded process broker. The returned
    /// value exclusively owns two direct helper children until `activate`
    /// retires the startup anchor.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if descriptor preparation, either
    /// helper fork, or group creation fails. Every successfully created helper
    /// is closed, terminated when still live, and reaped before an error is
    /// returned.
    pub fn new() -> io::Result<Self> {
        let anchor = GuardHelper::spawn_anchor()?;
        let group = create_process_group(anchor.process)?;
        let guard = GuardHelper::spawn_for_group(group)?;
        let guard_process = guard.process;
        Ok(Self {
            anchor: Some(anchor),
            group,
            guard: Some(guard),
            guard_process,
        })
    }

    /// Return the reserved group which a prepared workload must join.
    #[must_use]
    pub const fn process_group(&self) -> ProcessGroupId {
        self.group
    }

    /// Return the persistent helper process for exact wait-event routing.
    ///
    /// This identifier is for classifying events in a broker-owned all-child
    /// wait loop. Helper cleanup and event collection must be serialized. A
    /// terminal helper event means containment failed; do not treat it as a
    /// normal workload exit.
    #[must_use]
    pub const fn guard_process(&self) -> ProcessId {
        self.guard_process
    }

    /// Retire and reap the startup anchor after a workload joins the group.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a zero timeout or repeated activation, or a
    /// write, wait, unexpected-status, or deadline error. Failure is
    /// fail-closed: the persistent guard remains armed and dropping this value
    /// kills the group.
    pub fn activate(&mut self, timeout: Duration) -> io::Result<()> {
        validate_timeout(timeout)?;
        let Some(mut anchor) = self.anchor.take() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process-group guard is already active",
            ));
        };
        anchor.disarm_and_reap(timeout)
    }

    /// Disarm and reap both helpers without signaling the workload group.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a zero timeout, or a write, wait,
    /// unexpected-status, or deadline error. Consuming failure remains
    /// fail-closed and performs bounded helper cleanup before returning.
    pub fn disarm(mut self, timeout: Duration) -> io::Result<()> {
        validate_timeout(timeout)?;
        if let Some(mut anchor) = self.anchor.take() {
            anchor.disarm_and_reap(timeout)?;
        }
        let Some(mut guard) = self.guard.take() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process-group guard is already disarmed",
            ));
        };
        guard.disarm_and_reap(timeout)
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        drop(self.guard.take());
        drop(self.anchor.take());
    }
}

#[derive(Debug)]
struct GuardHelper {
    owner: Option<OwnedFd>,
    process: ProcessId,
}

impl GuardHelper {
    fn spawn_anchor() -> io::Result<Self> {
        let channel = socket_pair_cloexec()?;
        let (reader, writer) = channel.into_parts();
        let descriptors = GuardPrepared::new(&reader)?;
        match fork_process()? {
            ProcessFork::Parent(process) => {
                drop(reader);
                Ok(Self {
                    owner: Some(writer),
                    process,
                })
            }
            ProcessFork::Child => {
                descriptors.apply_in_child();
                let group = child_create_process_group();
                guard_loop(reader.as_raw_fd(), group);
            }
        }
    }

    fn spawn_for_group(group: ProcessGroupId) -> io::Result<Self> {
        let channel = socket_pair_cloexec()?;
        let (reader, writer) = channel.into_parts();
        let descriptors = GuardPrepared::new(&reader)?;
        match fork_process()? {
            ProcessFork::Parent(process) => {
                drop(reader);
                Ok(Self {
                    owner: Some(writer),
                    process,
                })
            }
            ProcessFork::Child => {
                descriptors.apply_in_child();
                guard_loop(reader.as_raw_fd(), group);
            }
        }
    }

    fn disarm_and_reap(&mut self, timeout: Duration) -> io::Result<()> {
        let owner = self.owner.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "guard is already disarmed")
        })?;
        let write_result = write_disarm(owner.as_raw_fd());
        drop(owner);
        if let Err(error) = write_result {
            let _ = self.finish(timeout);
            return Err(error);
        }
        self.finish(timeout)
    }

    fn finish(&self, timeout: Duration) -> io::Result<()> {
        match wait_for_clean_exit(self.process, timeout) {
            Ok(()) => Ok(()),
            Err(HelperWaitFailure::StillLive(error)) => {
                self.terminate_and_reap();
                Err(error)
            }
            Err(HelperWaitFailure::ReapedOrUnknown(error)) => Err(error),
        }
    }

    fn terminate_and_reap(&self) {
        let _ = signal_process(self.process, Signal::KILL);
        let _ = wait_event(self.process);
    }
}

impl Drop for GuardHelper {
    fn drop(&mut self) {
        drop(self.owner.take());
        let _ = self.finish(DROP_CLEANUP_TIMEOUT);
    }
}

#[derive(Debug)]
enum HelperWaitFailure {
    /// The helper remains an owned child, so its PID cannot yet be reused.
    StillLive(io::Error),
    /// A terminal event was already reaped or child ownership is no longer provable.
    ReapedOrUnknown(io::Error),
}

fn validate_timeout(timeout: Duration) -> io::Result<()> {
    if timeout.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "guard cleanup timeout must be greater than zero",
        ));
    }
    Instant::now().checked_add(timeout).map_or_else(
        || {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "guard cleanup timeout exceeds the monotonic clock range",
            ))
        },
        |_| Ok(()),
    )
}

fn wait_for_clean_exit(process: ProcessId, timeout: Duration) -> Result<(), HelperWaitFailure> {
    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
        HelperWaitFailure::ReapedOrUnknown(io::Error::new(
            io::ErrorKind::InvalidInput,
            "guard cleanup timeout exceeds the monotonic clock range",
        ))
    })?;
    let mut poll_interval = INITIAL_POLL_INTERVAL;
    let mut yield_attempts = 0;
    loop {
        match wait_event_nohang(process) {
            Ok(Some(crate::ChildEvent::Continued { .. }) | None) => {}
            Ok(Some(event)) => {
                return match event {
                    crate::ChildEvent::Exited { code: 0, .. } => Ok(()),
                    other if other.is_terminal() => {
                        Err(HelperWaitFailure::ReapedOrUnknown(io::Error::other(
                            format!("process-group helper terminated unexpectedly: {other:?}"),
                        )))
                    }
                    other => Err(HelperWaitFailure::StillLive(io::Error::other(format!(
                        "process-group helper entered an unexpected state: {other:?}"
                    )))),
                };
            }
            Err(error) => return Err(HelperWaitFailure::ReapedOrUnknown(error)),
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(HelperWaitFailure::StillLive(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for process-group helper",
            )));
        }
        if yield_attempts < YIELD_ATTEMPTS {
            std::thread::yield_now();
            yield_attempts += 1;
        } else {
            std::thread::sleep(poll_interval.min(deadline.saturating_duration_since(now)));
            poll_interval = poll_interval.saturating_mul(2).min(MAX_POLL_INTERVAL);
        }
    }
}

fn write_disarm(descriptor: RawFd) -> io::Result<()> {
    let record = DISARM_RECORD;
    loop {
        // SAFETY: descriptor is the live owner endpoint and the byte remains
        // immutable for the duration of write.
        let written = unsafe {
            libc::send(
                descriptor,
                (&raw const record).cast::<libc::c_void>(),
                1,
                libc::MSG_NOSIGNAL,
            )
        };
        if written == 1 {
            return Ok(());
        }
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "process-group helper accepted no disarm data",
            ));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn child_create_process_group() -> ProcessGroupId {
    loop {
        // SAFETY: both zero arguments select the current child and a new group.
        if unsafe { libc::setpgid(0, 0) } == 0 {
            // SAFETY: getpid has no preconditions and returns a positive PID.
            let process = unsafe { libc::getpid() };
            if let Some(group) = ProcessGroupId::new(process) {
                return group;
            }
            child_exit(FAILURE_EXIT);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            child_exit(FAILURE_EXIT);
        }
    }
}

fn guard_loop(descriptor: RawFd, group: ProcessGroupId) -> ! {
    let mut record = 0_u8;
    loop {
        // SAFETY: descriptor is the only retained live fd and record is writable.
        let read = unsafe { libc::read(descriptor, (&raw mut record).cast::<libc::c_void>(), 1) };
        if read == 1 && record == DISARM_RECORD {
            child_exit(0);
        }
        if read == -1 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        child_kill_group(group);
    }
}

fn child_kill_group(group: ProcessGroupId) -> ! {
    loop {
        // SAFETY: group is checked positive and negation selects that group.
        if unsafe { libc::kill(-group.get(), libc::SIGKILL) } == 0 {
            child_exit(0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            child_exit(FAILURE_EXIT);
        }
    }
}

fn child_exit(code: libc::c_int) -> ! {
    // SAFETY: helper children must not run inherited destructors after fork.
    unsafe { libc::_exit(code) }
}
