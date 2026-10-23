//! Fail-closed process-group lifetime guard.
//!
//! An out-of-group helper owns the guard's lifetime. It creates a short-lived
//! anchor which leads the reserved group and exits at once, and it does not
//! reap that anchor while the guard is armed. The anchor is created without
//! running application fork callbacks (see `anchor`), and the helper resets
//! its signal dispositions first, so no foreign code runs in the helper and
//! nothing but the helper can reap the anchor. The unreaped zombie remains a
//! member of the group, so the group exists and its numeric ID cannot be
//! reused while the helper holds the anchor, even after every workload member
//! has exited or left the group. An externally killed helper can lose this pin
//! before the owner observes its death: armed-group signals therefore cross
//! the helper socket through `signal_group`, never an owner-side group kill.
//! The anchor is the helper's child, not the owner's, so the owner's all-child
//! waits never observe or reap it.
//!
//! The helper owns no application descriptors and waits on one close-on-exec
//! socket. Owner loss closes that socket and makes the helper kill the complete
//! group, which the anchor still pins, and then reap the anchor. Explicit
//! disarm writes one byte before closure, proving that cleanup policy—not
//! incidental descriptor loss—ended the guard lifetime; the helper then reaps
//! the anchor without signalling the workload, which releases the group's ID.
//!
//! The flow is `new` (start the helper, which reserves and pins the group),
//! spawn the workload into [`ProcessGroupGuard::process_group`], `activate`
//! (record that the workload joined), and finally `disarm` for normal cleanup.
//! Dropping an armed guard is fail-closed. The owning broker must serialize
//! helper cleanup with all-child event collection; once a terminal event has
//! been reaped, its numeric PID is never safe to signal again. Auto-reaping
//! owners may create guards and use the anchor pin and helper-mediated signals:
//! the helper resets its own SIGCHLD policy before creating the anchor. Owner-side
//! cleanup may resume/kill a helper only under waitable policy with an unreaped
//! child pin. Otherwise it closes the socket but refuses numeric PID delivery.

mod anchor;
mod delivery;

use std::{
    io,
    os::fd::{AsRawFd, OwnedFd, RawFd},
    sync::Mutex,
    time::{Duration, Instant},
};

use self::anchor::{AnchorCreation, PreparedAnchor};
use crate::{
    ProcessFork, ProcessGroupId, ProcessId, Signal, descriptor::GuardPrepared, fork_process,
    socket_pair_cloexec, wait_event, wait_event_nohang,
};

const DISARM_RECORD: u8 = 0xa5;
const DROP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);
const FAILURE_EXIT: libc::c_int = 70;
const GROUP_RECORD_BYTES: usize = 4;
const GROUP_RESERVE_TIMEOUT: Duration = Duration::from_secs(3);
const INITIAL_POLL_INTERVAL: Duration = Duration::from_micros(50);
const MAX_POLL_INTERVAL: Duration = Duration::from_millis(5);
const YIELD_ATTEMPTS: u8 = 8;

/// An owned fail-closed capability for one reserved process group.
///
/// While its helper lives, the guard pins its group: an unreaped anchor zombie
/// stays a member, so [`crate::process_group_exists`] reports it present until
/// [`Self::disarm`]. Use [`Self::signal_group`] while armed: direct group signals
/// are unsafe if an unobserved helper death has released the pin.
/// Dropping an armed value closes the owner socket. The out-of-group helper then sends `SIGKILL` to the
/// group and is reaped within the caller's process. Call [`Self::activate`]
/// only after the workload has successfully joined the reserved group, and
/// [`Self::disarm`] only after the group no longer requires broker-death
/// containment. Cleanup resumes a stopped helper and waits up to three seconds
/// before killing the helper; its final reap waits for kernel progress and is
/// not absolutely bounded. With owner `SIGCHLD` ignored or `SA_NOCLDWAIT` set,
/// creation and helper-mediated group delivery still work, but cleanup refuses
/// helper PID signals: it cannot resume or kill a stopped helper. The socket is
/// still closed, so a running helper can finish cleanup itself. A helper that
/// cannot run cannot guarantee group cleanup.
///
/// This capability contains process-group membership, not arbitrary
/// descendants. A workload which creates a session or changes group has left
/// the portable boundary. Disarming releases the pin: once disarmed, an empty
/// group's ID may be reused, so it must never be signalled again.
#[must_use = "dropping an armed process-group guard triggers fail-closed cleanup"]
#[derive(Debug)]
pub struct ProcessGroupGuard {
    active: bool,
    group: ProcessGroupId,
    guard: Option<GuardHelper>,
    guard_process: ProcessId,
}

impl ProcessGroupGuard {
    /// Reserve and pin a process group under an out-of-group owner-loss guard.
    ///
    /// Call this from a dedicated single-threaded process broker. The returned
    /// value exclusively owns one direct helper child, which in turn owns the
    /// group's anchor until the guard is disarmed or dropped. An auto-reaping
    /// owner is accepted, without changing its policy; the helper owns its own
    /// waitable anchor. For owner-side cleanup signals keep SIGCHLD non-ignored
    /// and `SA_NOCLDWAIT` unset, and serialize policy changes and collection.
    ///
    /// # Errors
    ///
    /// Returns an operating-system error if descriptor preparation, the helper
    /// fork, or group reservation fails. The helper reports the errno with
    /// which it failed to close inherited descriptors or to create or confirm
    /// the anchor, such as `EACCES` from a sandbox; on targets that spawn the
    /// anchor (macOS among them) a missing or exec-denied `/usr/bin/true`
    /// returns the spawn error. `TimedOut` means the helper did not report in
    /// time, and `UnexpectedEof` that it exited without reporting. A started
    /// helper's socket is closed; numeric-PID cleanup is attempted only while
    /// waitable ownership is proven. An auto-reaping owner cannot resume or kill
    /// a stopped helper and may need its own cleanup coordination.
    pub fn new() -> io::Result<Self> {
        let guard = GuardHelper::spawn()?;
        let group = guard.read_group(GROUP_RESERVE_TIMEOUT)?;
        let guard_process = guard.process;
        Ok(Self {
            active: false,
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

    /// Record that the workload has joined the group.
    ///
    /// The group is reserved and pinned from [`Self::new`] onward, so
    /// activation changes no process state; it keeps the activation order
    /// explicit for callers. `timeout` is validated for source compatibility
    /// with 0.10.0 and is otherwise unused.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a zero or out-of-range timeout, or for a
    /// repeated activation. The guard stays armed either way.
    pub fn activate(&mut self, timeout: Duration) -> io::Result<()> {
        validate_timeout(timeout)?;
        if self.active {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process-group guard is already active",
            ));
        }
        self.active = true;
        Ok(())
    }

    /// Signal the group through the helper while it holds the unreaped anchor.
    ///
    /// Never signal the numeric group directly while armed: an unobserved
    /// helper death may have released its pin. Calls are serialized, including
    /// concurrent shared borrows. A timeout does not cancel a queued signal;
    /// it may still be delivered later. `Ok` means the helper delivered this
    /// exact request. Unique request IDs prevent stale replies, including for
    /// the same signal, from acknowledging a later request.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for zero/oversized timeouts, a missing owner, or
    /// exhaustion of the guard's non-reused u32 request IDs,
    /// `TimedOut` at the deadline, and `BrokenPipe`/`UnexpectedEof` if the helper
    /// is gone (the group must not then be signalled directly). Otherwise returns
    /// the helper's kill errno, including macOS `EPERM` for an anchor-only group.
    pub fn signal_group(&self, signal: Signal, timeout: Duration) -> io::Result<()> {
        validate_timeout(timeout)?;
        let guard = self
            .guard
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "guard helper was taken"))?;
        let owner = guard
            .owner
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "guard owner was taken"))?;
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "guard signal timeout is too large",
            )
        })?;
        delivery::signal_group(owner.as_raw_fd(), &guard.exchange, signal, deadline)
    }

    /// Disarm the helper and release the group's pin without signaling it.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a zero/oversized timeout or an auto-reaping
    /// owner policy, or a write, wait,
    /// unexpected-status, or deadline error. Consuming failure remains
    /// fail-closed. Cleanup resumes a stopped helper until the deadline, then
    /// kills it; the final reap waits for the kernel and has no absolute bound.
    /// If the owner violated the waitable-child policy, the socket is still
    /// closed but cleanup never signals a potentially reused helper PID. A
    /// stopped helper then cannot be resumed by this guard.
    pub fn disarm(mut self, timeout: Duration) -> io::Result<()> {
        validate_timeout(timeout)?;
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
    }
}

#[derive(Debug)]
struct GuardHelper {
    owner: Option<OwnedFd>,
    process: ProcessId,
    exchange: Mutex<delivery::Exchange>,
}

impl GuardHelper {
    fn spawn() -> io::Result<Self> {
        let channel = socket_pair_cloexec()?;
        let (reader, writer) = channel.into_parts();
        set_nonblocking(&writer)?;
        let descriptors = GuardPrepared::new(&reader)?;
        let anchor = PreparedAnchor::prepare()?;
        match fork_process()? {
            ProcessFork::Parent(process) => {
                drop(reader);
                Ok(Self {
                    owner: Some(writer),
                    process,
                    exchange: Mutex::new(delivery::Exchange::default()),
                })
            }
            ProcessFork::Child => {
                let descriptor = reader.as_raw_fd();
                match descriptors
                    .apply_in_child()
                    .and_then(|()| child_spawn_anchor(&anchor))
                {
                    Ok(group) => {
                        child_send_record(descriptor, group.get());
                        guard_loop(descriptor, group);
                    }
                    Err(errno) => {
                        child_send_record(descriptor, failure_record(errno));
                        child_exit(FAILURE_EXIT);
                    }
                }
            }
        }
    }

    /// Read the group the helper reserved, within `timeout`.
    fn read_group(&self, timeout: Duration) -> io::Result<ProcessGroupId> {
        let owner = self.owner.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "guard is already disarmed")
        })?;
        let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "group reservation timeout exceeds the monotonic clock range",
            )
        })?;
        let mut record = [0_u8; GROUP_RECORD_BYTES];
        let mut filled = 0;
        while filled < GROUP_RECORD_BYTES {
            wait_ready(owner.as_raw_fd(), libc::POLLIN, deadline)?;
            let Some(remaining) = record.get_mut(filled..) else {
                break;
            };
            // SAFETY: owner is the live helper socket and `remaining` is
            // writable storage of exactly the length passed.
            let read = unsafe {
                libc::read(
                    owner.as_raw_fd(),
                    remaining.as_mut_ptr().cast::<libc::c_void>(),
                    remaining.len(),
                )
            };
            match usize::try_from(read) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "process-group helper exited before reserving the group",
                    ));
                }
                Ok(count) => filled = filled.saturating_add(count),
                Err(_) => {
                    let error = io::Error::last_os_error();
                    if !matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) {
                        return Err(error);
                    }
                }
            }
        }
        decode_group_record(libc::pid_t::from_ne_bytes(record))
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
        crate::child_signal::require_waitable_children()?;
        match wait_for_clean_exit(self.process, timeout) {
            Ok(()) => Ok(()),
            Err(HelperWaitFailure::StillLive(error)) => {
                self.terminate_and_reap()?;
                Err(error)
            }
            Err(HelperWaitFailure::ReapedOrUnknown(error)) => Err(error),
        }
    }

    fn terminate_and_reap(&self) -> io::Result<()> {
        if crate::child_signal::signal_owned_child(self.process, Signal::KILL)?.is_none() {
            wait_event(self.process)?;
        }
        Ok(())
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

/// Make the owner endpoint nonblocking before the helper exists.
///
/// Exchanges and group-record reads wait with `poll`; disarm's single-byte send
/// fails immediately on a full queue so it can close the endpoint and clean up.
/// Nonblocking mode turns a full/empty socket into `WouldBlock`, not a hang. macOS
/// ignores `MSG_DONTWAIT` on sends, so the flag alone cannot provide this. The
/// helper's endpoint is a separate open file description and stays blocking.
///
/// # Errors
///
/// Returns the `fcntl` failure; the guard is then never created.
fn set_nonblocking(descriptor: &OwnedFd) -> io::Result<()> {
    // SAFETY: descriptor is a live owned socket; F_GETFL only reads its flags.
    let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: this adds O_NONBLOCK to the owner endpoint's own file description.
    if unsafe {
        libc::fcntl(
            descriptor.as_raw_fd(),
            libc::F_SETFL,
            flags | libc::O_NONBLOCK,
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The helper's startup record: a positive reserved group, or the negated errno
/// with which closing descriptors or creating and confirming the anchor failed.
fn decode_group_record(record: libc::pid_t) -> io::Result<ProcessGroupId> {
    if record < 0 {
        let errno = record.checked_neg().unwrap_or(libc::EIO);
        return Err(io::Error::from_raw_os_error(errno));
    }
    ProcessGroupId::try_from(record)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Encode a helper startup failure; an errno is never zero or negative, but a
/// malformed one must still read as a failure.
fn failure_record(errno: libc::c_int) -> libc::pid_t {
    errno
        .checked_neg()
        .filter(|record| *record < 0)
        .unwrap_or(-libc::EIO)
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

/// Wait for the requested socket readiness or the shared deadline.
fn wait_ready(descriptor: RawFd, events: libc::c_short, deadline: Instant) -> io::Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for the process-group helper",
            ));
        }
        let milliseconds =
            libc::c_int::try_from(remaining.as_millis().max(1)).unwrap_or(libc::c_int::MAX);
        let mut poll = libc::pollfd {
            fd: descriptor,
            events,
            revents: 0,
        };
        // SAFETY: poll points to one initialized pollfd for a live descriptor.
        let ready = unsafe { libc::poll(&raw mut poll, 1, milliseconds) };
        if ready > 0 {
            return Ok(());
        }
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
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
    let mut initial = true;
    loop {
        match wait_event_nohang(process) {
            Ok(
                event @ (Some(
                    crate::ChildEvent::Continued { .. } | crate::ChildEvent::Stopped { .. },
                )
                | None),
            ) => {
                // The ownership probe must precede SIGCONT: a terminal reap or
                // ECHILD releases the PID. Resume initially even when the broker
                // already consumed STOP, and again on every newly observed stop.
                if initial || matches!(event, Some(crate::ChildEvent::Stopped { .. })) {
                    // A running helper can finish via the socket even when a
                    // resume is unavailable. Keep waiting as before; every later
                    // delivery (including deadline KILL) rechecks its own pin.
                    if let Ok(Some(event)) =
                        crate::child_signal::signal_owned_child(process, Signal::CONT)
                    {
                        return helper_exit_result(event);
                    }
                }
            }
            Ok(Some(event)) => {
                return helper_exit_result(event);
            }
            Err(error) => return Err(HelperWaitFailure::ReapedOrUnknown(error)),
        }
        initial = false;
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

fn helper_exit_result(event: crate::ChildEvent) -> Result<(), HelperWaitFailure> {
    match event {
        crate::ChildEvent::Exited { code: 0, .. } => Ok(()),
        other => Err(HelperWaitFailure::ReapedOrUnknown(io::Error::other(
            format!("process-group helper terminated unexpectedly: {other:?}"),
        ))),
    }
}

fn write_disarm(descriptor: RawFd) -> io::Result<()> {
    let record = DISARM_RECORD;
    loop {
        // SAFETY: descriptor is the live owner endpoint and the byte remains
        // immutable for the duration of write. A full queue must not block
        // cleanup from closing the owner and resuming a stopped helper; the
        // endpoint is nonblocking, since macOS ignores MSG_DONTWAIT on sends.
        let written = unsafe {
            libc::send(
                descriptor,
                (&raw const record).cast::<libc::c_void>(),
                1,
                libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
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

/// In the helper: create the anchor, which leads the group and exits at once.
///
/// Signals are reset first, and the anchor is created without fork callbacks,
/// so from here on only this helper can reap it. The helper then confirms that
/// a group with the anchor's ID exists. The anchor stays unreaped, so the
/// group's ID is its PID and cannot be reused until the helper reaps it. On
/// failure the errno is returned for the owner, after reaping any anchor.
fn child_spawn_anchor(anchor: &PreparedAnchor) -> Result<ProcessGroupId, libc::c_int> {
    child_reset_signals();
    let group = ProcessGroupId::new(anchor.child_create()?).ok_or(libc::EIO)?;
    loop {
        // SAFETY: the null signal delivers nothing; the negated positive ID
        // selects exactly the group named after the unreaped anchor.
        if unsafe { libc::kill(-group.get(), 0) } == 0 {
            return Ok(group);
        }
        let errno = crate::raw::last_errno();
        match classify_group_errno(Some(errno)) {
            GroupErrno::Present => return Ok(group),
            GroupErrno::Interrupted => {}
            GroupErrno::Failed => {
                child_reap_anchor(group);
                return Err(errno);
            }
        }
    }
}

/// How the helper reads a failed `kill(2)` of its own group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GroupErrno {
    /// The group exists, but no member could be signalled (`EPERM`). macOS
    /// skips zombie members and answers this once the unreaped anchor is the
    /// only member left, so the group is still reserved.
    Present,
    /// The call was interrupted (`EINTR`) and is retried.
    Interrupted,
    /// Any other error, including `ESRCH`: the group is not reserved.
    Failed,
}

fn classify_group_errno(errno: Option<libc::c_int>) -> GroupErrno {
    match errno {
        Some(libc::EPERM) => GroupErrno::Present,
        Some(libc::EINTR) => GroupErrno::Interrupted,
        _ => GroupErrno::Failed,
    }
}

/// Give the helper exec-like dispositions before it has any children.
///
/// Any application handler, not just SIGCHLD, could reap the anchor. Reset all
/// handlers, retaining ignored signals except SIGCHLD (the zombie pin) and
/// SIGCONT (cleanup's resume). Block callbacks while changing dispositions,
/// then restore the inherited mask. The anchor inherits this sanitized state;
/// it only calls setpgid and _exit, so it needs no second reset. The owner is
/// untouched. All operations here are async-signal-safe.
fn child_reset_signals() {
    // SAFETY: an all-zero sigaction is a valid value: no flags, an empty mask
    // after sigemptyset, and the default handler set below.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = libc::SIG_DFL;
    // SAFETY: sigemptyset initializes writable mask storage in this child.
    if unsafe { libc::sigemptyset(&raw mut action.sa_mask) } != 0 {
        child_exit(FAILURE_EXIT);
    }
    // SAFETY: both sets are initialized by the signal calls below.
    let mut blocked: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: sigprocmask writes the original mask before it is restored.
    let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: live writable signal sets; only this child changes its mask.
    if unsafe { libc::sigfillset(&raw mut blocked) } != 0
        || unsafe { libc::sigprocmask(libc::SIG_BLOCK, &raw const blocked, &raw mut previous) } != 0
    {
        child_exit(FAILURE_EXIT);
    }
    // A signal set has at least one bit per supported signal on our Unix
    // targets. Query the entire representable range, including realtime and
    // platform-specific signals; EINVAL skips holes and libc-reserved signals.
    for signal in 1..=size_of::<libc::sigset_t>() * 8 {
        let Ok(signal) = libc::c_int::try_from(signal) else {
            child_exit(FAILURE_EXIT);
        };
        if signal == libc::SIGKILL || signal == libc::SIGSTOP {
            continue;
        }
        // SAFETY: sigaction initializes this output on success.
        let mut inherited: libc::sigaction = unsafe { std::mem::zeroed() };
        loop {
            // SAFETY: reads one disposition into live storage.
            if unsafe { libc::sigaction(signal, std::ptr::null(), &raw mut inherited) } == 0 {
                break;
            }
            match crate::raw::last_errno() {
                libc::EINTR => {}
                libc::EINVAL => break,
                _ => child_exit(FAILURE_EXIT),
            }
        }
        if signal != libc::SIGCHLD
            && signal != libc::SIGCONT
            && (inherited.sa_sigaction == libc::SIG_DFL || inherited.sa_sigaction == libc::SIG_IGN)
        {
            continue;
        }
        loop {
            // SAFETY: initialized action; changes only this helper's disposition.
            if unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) } == 0 {
                break;
            }
            if crate::raw::last_errno() != libc::EINTR {
                child_exit(FAILURE_EXIT);
            }
        }
    }
    // SAFETY: restore the mask saved above, now with no application callbacks.
    if unsafe { libc::sigprocmask(libc::SIG_SETMASK, &raw const previous, std::ptr::null_mut()) }
        != 0
    {
        child_exit(FAILURE_EXIT);
    }
}

/// In the helper: send the startup record, the reserved group or a failure.
///
/// A failed report means the owner is gone; after a group, the guard loop then
/// observes the closed socket and kills the group, so no separate failure path
/// is needed.
fn child_send_record(descriptor: RawFd, value: libc::pid_t) {
    let record = value.to_ne_bytes();
    let mut sent = 0;
    while let Some(remaining) = record.get(sent..) {
        if remaining.is_empty() {
            return;
        }
        // SAFETY: descriptor is the helper's live socket and `remaining` is
        // readable storage of exactly the length passed.
        let written = unsafe {
            libc::send(
                descriptor,
                remaining.as_ptr().cast::<libc::c_void>(),
                remaining.len(),
                libc::MSG_NOSIGNAL,
            )
        };
        match usize::try_from(written) {
            Ok(count) if count > 0 => sent = sent.saturating_add(count),
            _ if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
            _ => return,
        }
    }
}

fn guard_loop(descriptor: RawFd, group: ProcessGroupId) -> ! {
    let mut record = 0_u8;
    loop {
        // SAFETY: descriptor is the only retained live fd and record is writable.
        let read = unsafe { libc::read(descriptor, (&raw mut record).cast::<libc::c_void>(), 1) };
        if read == 1 && record == DISARM_RECORD {
            child_reap_anchor(group);
            child_exit(0);
        }
        if read == 1 && record == delivery::SIGNAL_RECORD {
            delivery::child_signal(descriptor, group);
            continue;
        }
        if read == -1 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        child_kill_group(group);
    }
}

/// In the helper: kill the group the anchor still pins, then release it.
///
/// `EPERM` means no member could be signalled; on macOS that is the normal
/// answer once every workload member is gone and only the anchor remains, so
/// it is not a failed cleanup.
fn child_kill_group(group: ProcessGroupId) -> ! {
    loop {
        // SAFETY: group is checked positive and negation selects that group,
        // which the unreaped anchor keeps reserved.
        if unsafe { libc::kill(-group.get(), libc::SIGKILL) } == 0 {
            child_reap_anchor(group);
            child_exit(0);
        }
        match classify_group_errno(io::Error::last_os_error().raw_os_error()) {
            GroupErrno::Present => {
                child_reap_anchor(group);
                child_exit(0);
            }
            GroupErrno::Interrupted => {}
            GroupErrno::Failed => {
                child_reap_anchor(group);
                child_exit(FAILURE_EXIT);
            }
        }
    }
}

/// In the helper: reap the anchor, whose PID is the group's ID.
///
/// Kill the specific owned child first: it may have stopped before `_exit`.
/// Its positive PID cannot be reused until this helper reaps it, and nothing
/// else can: no foreign code runs in the helper after `child_reset_signals`,
/// and the anchor was created without fork callbacks. No workload member is
/// signalled here. The final wait still depends on kernel progress.
fn child_reap_anchor(group: ProcessGroupId) {
    // SAFETY: this positive PID is the helper's own unreaped anchor child.
    let _ = unsafe { libc::kill(group.get(), libc::SIGKILL) };
    let mut status = 0;
    loop {
        // SAFETY: status is writable; the PID is this helper's own child.
        let reaped = unsafe { libc::waitpid(group.get(), &raw mut status, 0) };
        if reaped == group.get() {
            return;
        }
        if reaped == -1 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return;
    }
}

fn child_exit(code: libc::c_int) -> ! {
    // SAFETY: helper children must not run inherited destructors after fork.
    unsafe { libc::_exit(code) }
}

#[cfg(test)]
mod tests {
    use super::{
        GroupErrno, ProcessGroupGuard, child_send_record, classify_group_errno,
        decode_group_record, failure_record,
    };

    /// A stopped helper can fill the request queue. Disarm must return an
    /// error rather than block before the owner-loss cleanup can resume it.
    #[test]
    fn disarm_full_socket_does_not_block() -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let (owner, _helper) = crate::socket_pair_cloexec()?.into_parts();
        super::set_nonblocking(&owner)?;
        let record = 0_u8;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            // SAFETY: owner is live; record is one initialized byte. This
            // fills only a test-owned socket, using nonblocking writes.
            let sent = unsafe {
                libc::send(
                    owner.as_raw_fd(),
                    (&raw const record).cast(),
                    1,
                    libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
                )
            };
            if sent < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().kind(),
                    std::io::ErrorKind::WouldBlock
                );
                break;
            }
            if std::time::Instant::now() >= deadline {
                return Err(std::io::Error::other("socket did not fill"));
            }
        }
        let result = super::write_disarm(owner.as_raw_fd());
        assert!(matches!(result, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
        Ok(())
    }

    #[test]
    fn guard_preserves_auto_traits() {
        fn require<T: Send + Sync + Unpin + std::panic::UnwindSafe + std::panic::RefUnwindSafe>() {}
        require::<ProcessGroupGuard>();
    }

    #[test]
    fn signal_rejects_a_taken_owner() -> std::io::Result<()> {
        let mut guard = ProcessGroupGuard::new()?;
        if let Some(helper) = guard.guard.as_mut() {
            drop(helper.owner.take());
        }
        let result = guard.signal_group(crate::Signal::KILL, std::time::Duration::from_secs(1));
        assert!(matches!(result, Err(error) if error.kind() == std::io::ErrorKind::InvalidInput));
        Ok(())
    }

    /// The startup record carries a positive group or a negated errno, so the
    /// owner reports why the helper could not reserve a group.
    #[test]
    fn group_record_reports_helper_errno() {
        assert!(matches!(decode_group_record(4242), Ok(group) if group.get() == 4242));
        let denied = decode_group_record(failure_record(libc::EACCES));
        assert!(matches!(denied, Err(error) if error.raw_os_error() == Some(libc::EACCES)));
        for malformed in [0, -1, libc::c_int::MIN] {
            assert_eq!(failure_record(malformed), -libc::EIO);
        }
        assert_eq!(failure_record(0), -libc::EIO);
        let overflow = decode_group_record(libc::pid_t::MIN);
        assert!(matches!(overflow, Err(error) if error.raw_os_error() == Some(libc::EIO)));
        assert!(
            matches!(decode_group_record(0), Err(error) if error.kind() == std::io::ErrorKind::InvalidData)
        );
    }

    /// The helper's failure record crosses the socket intact.
    #[test]
    fn helper_failure_record_crosses_the_socket() -> std::io::Result<()> {
        use std::{io::Read, os::fd::AsRawFd};
        let (owner, helper) = crate::socket_pair_cloexec()?.into_parts();
        child_send_record(helper.as_raw_fd(), failure_record(libc::ENOENT));
        let mut record = [0_u8; 4];
        std::fs::File::from(owner).read_exact(&mut record)?;
        let decoded = decode_group_record(libc::pid_t::from_ne_bytes(record));
        assert!(matches!(decoded, Err(error) if error.raw_os_error() == Some(libc::ENOENT)));
        Ok(())
    }

    /// `EPERM` keeps the anchor's group reserved, because macOS answers it for
    /// a group whose only member is the unreaped anchor; only `EINTR` retries,
    /// and every other answer means no group is reserved.
    #[test]
    fn group_errno_treats_eperm_as_a_reserved_group() {
        assert_eq!(classify_group_errno(Some(libc::EPERM)), GroupErrno::Present);
        assert_eq!(
            classify_group_errno(Some(libc::EINTR)),
            GroupErrno::Interrupted
        );
        for failed in [Some(libc::ESRCH), Some(libc::EINVAL), None] {
            assert_eq!(classify_group_errno(failed), GroupErrno::Failed);
        }
    }
}
