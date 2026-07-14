//! Standalone broker-death contract without the multithreaded Rust test harness.

use std::{
    error::Error,
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, OwnedFd, RawFd},
    process::ExitCode,
    thread,
    time::{Duration, Instant},
};

use fork::{
    ChildEvent, PreparedCommand, ProcessFork, ProcessGroup, ProcessGroupGuard, ProcessGroupId,
    ProcessId, Signal, fork_process, pipe_cloexec, signal_process, signal_process_group,
    wait_event,
};

const CONTRACT_TIMEOUT: Duration = Duration::from_secs(3);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(2);
const WORKLOAD_SECONDS: &str = "30";

fn main() -> ExitCode {
    let result = broker_owner_loss_before_workload_cleans_helpers()
        .and_then(|()| broker_owner_loss_kills_workload_group());
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("broker owner-loss contract failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn broker_owner_loss_before_workload_cleans_helpers() -> Result<(), Box<dyn Error>> {
    let ready = pipe_cloexec()?;
    let (ready_reader, ready_writer) = ready.into_parts();
    match fork_process()? {
        ProcessFork::Parent(broker) => {
            drop(ready_writer);
            verify_empty_parent_contract(broker, ready_reader)
        }
        ProcessFork::Child => {
            drop(ready_reader);
            run_empty_broker(ready_writer);
        }
    }
}

fn broker_owner_loss_kills_workload_group() -> Result<(), Box<dyn Error>> {
    let ready = pipe_cloexec()?;
    let lifetime = pipe_cloexec()?;
    let (ready_reader, ready_writer) = ready.into_parts();
    let (lifetime_reader, lifetime_writer) = lifetime.into_parts();
    match fork_process()? {
        ProcessFork::Parent(broker) => {
            drop(ready_writer);
            drop(lifetime_writer);
            verify_parent_contract(broker, ready_reader, lifetime_reader)
        }
        ProcessFork::Child => {
            drop(ready_reader);
            drop(lifetime_reader);
            run_broker(ready_writer, lifetime_writer);
        }
    }
}

fn run_broker(ready: OwnedFd, lifetime: OwnedFd) -> ! {
    let result = prepare_guarded_workload(ready, lifetime);
    match result {
        Ok(guard) => {
            let _guard = guard;
            loop {
                thread::sleep(Duration::from_mins(1));
            }
        }
        Err(_) => child_exit(1),
    }
}

fn run_empty_broker(ready: OwnedFd) -> ! {
    let result = prepare_empty_guard(ready);
    match result {
        Ok(guard) => {
            let _guard = guard;
            loop {
                thread::sleep(Duration::from_mins(1));
            }
        }
        Err(_) => child_exit(1),
    }
}

fn prepare_empty_guard(ready: OwnedFd) -> Result<ProcessGroupGuard, Box<dyn Error>> {
    let guard = ProcessGroupGuard::new()?;
    let mut ready = File::from(ready);
    ready.write_all(&guard.process_group().get().to_ne_bytes())?;
    ready.write_all(&guard.guard_process().get().to_ne_bytes())?;
    drop(ready);
    Ok(guard)
}

fn prepare_guarded_workload(
    ready: OwnedFd,
    lifetime: OwnedFd,
) -> Result<ProcessGroupGuard, Box<dyn Error>> {
    let mut guard = ProcessGroupGuard::new()?;
    let mut command = PreparedCommand::new("/bin/sleep")?;
    command
        .arg(WORKLOAD_SECONDS)?
        .process_group(ProcessGroup::Join(guard.process_group()));
    command.inherit_descriptor(lifetime)?;
    let workload = command.spawn(STARTUP_TIMEOUT).map_err(io::Error::other)?;
    guard.activate(CONTRACT_TIMEOUT)?;

    let mut ready = File::from(ready);
    ready.write_all(&guard.process_group().get().to_ne_bytes())?;
    ready.write_all(&workload.process().get().to_ne_bytes())?;
    drop(ready);
    Ok(guard)
}

fn verify_parent_contract(
    broker: ProcessId,
    ready: OwnedFd,
    lifetime: OwnedFd,
) -> Result<(), Box<dyn Error>> {
    let mut cleanup = BrokerCleanup::new(broker);
    wait_readable(ready.as_raw_fd(), STARTUP_TIMEOUT)?;
    let mut ready = File::from(ready);
    let mut group_bytes = [0_u8; size_of::<libc::pid_t>()];
    let mut workload_bytes = [0_u8; size_of::<libc::pid_t>()];
    ready.read_exact(&mut group_bytes)?;
    ready.read_exact(&mut workload_bytes)?;
    let group = ProcessGroupId::try_from(libc::pid_t::from_ne_bytes(group_bytes))?;
    let _workload = ProcessId::try_from(libc::pid_t::from_ne_bytes(workload_bytes))?;
    cleanup.group = Some(group);

    kill_and_reap_broker(broker)?;
    cleanup.broker_reaped = true;

    wait_readable(lifetime.as_raw_fd(), CONTRACT_TIMEOUT)?;
    let mut lifetime = File::from(lifetime);
    let mut unexpected = [0_u8; 1];
    if lifetime.read(&mut unexpected)? != 0 {
        return Err(io::Error::other("workload lifetime pipe contained unexpected data").into());
    }
    cleanup.group = None;
    Ok(())
}

fn verify_empty_parent_contract(broker: ProcessId, ready: OwnedFd) -> Result<(), Box<dyn Error>> {
    let mut cleanup = BrokerCleanup::new(broker);
    wait_readable(ready.as_raw_fd(), STARTUP_TIMEOUT)?;
    let mut ready = File::from(ready);
    let mut group_bytes = [0_u8; size_of::<libc::pid_t>()];
    let mut helper_bytes = [0_u8; size_of::<libc::pid_t>()];
    ready.read_exact(&mut group_bytes)?;
    ready.read_exact(&mut helper_bytes)?;
    let group = ProcessGroupId::try_from(libc::pid_t::from_ne_bytes(group_bytes))?;
    let helper = ProcessId::try_from(libc::pid_t::from_ne_bytes(helper_bytes))?;
    cleanup.group = Some(group);
    cleanup.helper = Some(helper);

    kill_and_reap_broker(broker)?;
    cleanup.broker_reaped = true;
    wait_process_absent(ProcessId::try_from(group.get())?, CONTRACT_TIMEOUT)?;
    wait_process_absent(helper, CONTRACT_TIMEOUT)?;
    cleanup.group = None;
    cleanup.helper = None;
    Ok(())
}

fn kill_and_reap_broker(broker: ProcessId) -> Result<(), Box<dyn Error>> {
    signal_process(broker, Signal::KILL)?;
    let event = wait_event(broker)?;
    if matches!(
        event,
        ChildEvent::Signalled {
            signal: Signal::KILL,
            ..
        }
    ) {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "broker returned unexpected terminal event: {event:?}"
        ))
        .into())
    }
}

fn wait_process_absent(process: ProcessId, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "process wait timeout exceeds the monotonic clock range",
        )
    })?;
    loop {
        // SAFETY: signal zero performs an existence check for a positive PID.
        if unsafe { libc::kill(process.get(), 0) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("helper process {process} survived broker death"),
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn wait_readable(descriptor: RawFd, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "poll timeout exceeds the monotonic clock range",
        )
    })?;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for process-lifetime descriptor",
            ));
        }
        let remaining = deadline.saturating_duration_since(now);
        let timeout_millis = i32::try_from(remaining.as_millis())
            .unwrap_or(i32::MAX)
            .max(1);
        let mut poll_descriptor = libc::pollfd {
            fd: descriptor,
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        // SAFETY: poll_descriptor is initialized writable storage for one entry.
        let result = unsafe { libc::poll(&raw mut poll_descriptor, 1, timeout_millis) };
        if result > 0 {
            if poll_descriptor.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                return Ok(());
            }
            return Err(io::Error::other(format!(
                "lifetime descriptor returned poll flags {:#x}",
                poll_descriptor.revents
            )));
        }
        if result == 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

struct BrokerCleanup {
    broker: ProcessId,
    broker_reaped: bool,
    group: Option<ProcessGroupId>,
    helper: Option<ProcessId>,
}

impl BrokerCleanup {
    const fn new(broker: ProcessId) -> Self {
        Self {
            broker,
            broker_reaped: false,
            group: None,
            helper: None,
        }
    }
}

impl Drop for BrokerCleanup {
    fn drop(&mut self) {
        if let Some(group) = self.group {
            let _ = signal_process_group(group, Signal::KILL);
        }
        if let Some(helper) = self.helper {
            let _ = signal_process(helper, Signal::KILL);
        }
        if !self.broker_reaped {
            let _ = signal_process(self.broker, Signal::KILL);
            let _ = wait_event(self.broker);
        }
    }
}

fn child_exit(code: libc::c_int) -> ! {
    // SAFETY: the fork-only broker must not run inherited parent destructors.
    unsafe { libc::_exit(code) }
}
