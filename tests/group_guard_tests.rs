//! Native contracts for fail-closed process-group owner loss.

use std::{
    error::Error,
    fs::File,
    io::{self, Read, Write},
    mem::MaybeUninit,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use fork::{
    ChildEvent, PreparedCommand, ProcessFork, ProcessGroup, ProcessGroupGuard, ProcessId, Signal,
    close_fd, fork_process, join_process_group, setsid, signal_process, signal_process_group,
    wait_event, wait_event_nohang,
};

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(2);

#[test]
fn process_group_guard_contains_owner_loss_and_disarms_cleanly() -> Result<(), Box<dyn Error>> {
    require_case(
        "owner loss kills a running group",
        owner_loss_kills_running_group(),
    )?;
    require_case(
        "owner loss kills a stopped group",
        owner_loss_kills_stopped_group(),
    )?;
    require_case(
        "group signals preserve the guard",
        group_signals_do_not_disable_guard(),
    )?;
    require_case(
        "explicit disarm preserves the group",
        explicit_disarm_preserves_running_group(),
    )?;
    require_case(
        "zero timeout fails closed",
        zero_timeout_is_rejected_fail_closed(),
    )?;
    require_case(
        "oversized timeout fails closed",
        oversized_timeout_is_rejected_fail_closed(),
    )?;
    require_case("activation is single use", activation_is_single_use())?;
    require_case(
        "pre-activation disarm reaps helpers",
        disarm_before_activation_reaps_both_helpers(),
    )?;
    require_case(
        "exec failure reaps helpers",
        workload_exec_failure_reaps_both_helpers(),
    )?;
    require_case(
        "post-exit disarm is clean",
        immediate_disarm_after_workload_exit_is_clean(),
    )?;
    require_case(
        "early owner loss reaps helpers",
        owner_loss_before_workload_leaves_no_helper(),
    )?;
    require_case(
        "helpers isolate descriptors",
        helpers_close_unintended_descriptors(),
    )?;
    require_case(
        "closed standard descriptors are supported",
        guard_supports_closed_standard_descriptor(),
    )?;
    require_case(
        "stopped helper cleanup preserves the workload",
        stopped_helper_is_terminated_without_touching_workload(),
    )?;
    require_case(
        "dead helper disarm avoids SIGPIPE",
        dead_helper_makes_disarm_error_without_sigpipe(),
    )?;
    require_case(
        "session escape remains outside containment",
        session_escape_is_explicitly_outside_group_containment(),
    )?;
    Ok(())
}

fn require_case(name: &str, result: Result<(), Box<dyn Error>>) -> Result<(), Box<dyn Error>> {
    result.map_err(|error| io::Error::other(format!("{name}: {error}")).into())
}

fn activation_is_single_use() -> Result<(), Box<dyn Error>> {
    let mut guard = ProcessGroupGuard::new()?;
    let mut workload = WorkloadGuard::new(spawn_workload(guard.process_group())?);
    guard.activate(CLEANUP_TIMEOUT)?;
    let error = guard
        .activate(CLEANUP_TIMEOUT)
        .err()
        .ok_or_else(|| io::Error::other("process-group guard activated twice"))?;
    if error.kind() != io::ErrorKind::InvalidInput {
        return Err(io::Error::other(format!(
            "second activation returned unexpected error: {error}"
        ))
        .into());
    }
    guard.disarm(CLEANUP_TIMEOUT)?;
    terminate_workload(&mut workload)
}

fn disarm_before_activation_reaps_both_helpers() -> Result<(), Box<dyn Error>> {
    let guard = ProcessGroupGuard::new()?;
    let helper = guard.guard_process();
    guard.disarm(CLEANUP_TIMEOUT)?;
    require_not_a_child(helper)
}

fn group_signals_do_not_disable_guard() -> Result<(), Box<dyn Error>> {
    terminal_group_signal_does_not_disable_guard(Signal::TERM)?;
    terminal_group_signal_does_not_disable_guard(Signal::KILL)?;

    let mut guard = ProcessGroupGuard::new()?;
    let mut workload = WorkloadGuard::new(spawn_workload(guard.process_group())?);
    guard.activate(CLEANUP_TIMEOUT)?;
    signal_process_group(guard.process_group(), Signal::STOP)?;
    require_event(workload.process(), |event| {
        matches!(
            event,
            ChildEvent::Stopped {
                signal: Signal::STOP,
                ..
            }
        )
    })?;
    require_helper_running(guard.guard_process())?;
    signal_process_group(guard.process_group(), Signal::CONT)?;
    require_event(workload.process(), |event| {
        matches!(event, ChildEvent::Continued { .. })
    })?;
    require_helper_running(guard.guard_process())?;
    drop(guard);
    require_killed(workload.process())?;
    workload.mark_reaped();
    Ok(())
}

fn terminal_group_signal_does_not_disable_guard(signal: Signal) -> Result<(), Box<dyn Error>> {
    let mut guard = ProcessGroupGuard::new()?;
    let mut workload = WorkloadGuard::new(spawn_workload(guard.process_group())?);
    guard.activate(CLEANUP_TIMEOUT)?;
    signal_process_group(guard.process_group(), signal)?;
    require_event(
        workload.process(),
        |event| matches!(event, ChildEvent::Signalled { signal: observed, .. } if observed == signal),
    )?;
    workload.mark_reaped();
    require_helper_running(guard.guard_process())?;
    guard.disarm(CLEANUP_TIMEOUT)?;
    Ok(())
}

fn stopped_helper_is_terminated_without_touching_workload() -> Result<(), Box<dyn Error>> {
    let mut guard = ProcessGroupGuard::new()?;
    let mut workload = WorkloadGuard::new(spawn_workload(guard.process_group())?);
    guard.activate(CLEANUP_TIMEOUT)?;
    let helper = guard.guard_process();
    signal_process(helper, Signal::STOP)?;
    wait_for_stopped_without_reaping(helper)?;
    let error = guard
        .disarm(CLEANUP_TIMEOUT)
        .err()
        .ok_or_else(|| io::Error::other("stopped helper accepted clean disarm"))?;
    if error.kind() != io::ErrorKind::Other {
        return Err(
            io::Error::other(format!("stopped helper returned unexpected error: {error}")).into(),
        );
    }
    require_not_a_child(helper)?;
    if wait_event_nohang(workload.process())?.is_some() {
        return Err(io::Error::other("stopped helper cleanup terminated the workload").into());
    }
    terminate_workload(&mut workload)
}

fn oversized_timeout_is_rejected_fail_closed() -> Result<(), Box<dyn Error>> {
    let mut guard = ProcessGroupGuard::new()?;
    let mut workload = WorkloadGuard::new(spawn_workload(guard.process_group())?);
    guard.activate(CLEANUP_TIMEOUT)?;
    let error = guard
        .disarm(Duration::MAX)
        .err()
        .ok_or_else(|| io::Error::other("oversized disarm timeout was accepted"))?;
    if error.kind() != io::ErrorKind::InvalidInput {
        return Err(io::Error::other(format!(
            "oversized disarm timeout returned unexpected error: {error}"
        ))
        .into());
    }
    require_killed(workload.process())?;
    workload.mark_reaped();
    Ok(())
}

fn workload_exec_failure_reaps_both_helpers() -> Result<(), Box<dyn Error>> {
    let guard = ProcessGroupGuard::new()?;
    let helper = guard.guard_process();
    let mut command = PreparedCommand::new("/this/path/must/not/exist")?;
    command.process_group(ProcessGroup::Join(guard.process_group()));
    if command.spawn(STARTUP_TIMEOUT).is_ok() {
        return Err(io::Error::other("nonexistent workload unexpectedly executed").into());
    }
    drop(guard);
    require_not_a_child(helper)
}

fn immediate_disarm_after_workload_exit_is_clean() -> Result<(), Box<dyn Error>> {
    let mut guard = ProcessGroupGuard::new()?;
    let mut command = PreparedCommand::new("/bin/sh")?;
    command.arg("-c")?.arg("exit 0")?;
    command.process_group(ProcessGroup::Join(guard.process_group()));
    let child = command.spawn(STARTUP_TIMEOUT).map_err(io::Error::other)?;
    guard.activate(CLEANUP_TIMEOUT)?;
    require_event(child.process(), |event| {
        matches!(event, ChildEvent::Exited { code: 0, .. })
    })?;
    guard.disarm(CLEANUP_TIMEOUT)?;
    Ok(())
}

fn zero_timeout_is_rejected_fail_closed() -> Result<(), Box<dyn Error>> {
    let mut inactive = ProcessGroupGuard::new()?;
    let activation_error = inactive
        .activate(Duration::ZERO)
        .err()
        .ok_or_else(|| io::Error::other("zero activation timeout was accepted"))?;
    if activation_error.kind() != io::ErrorKind::InvalidInput {
        return Err(io::Error::other(format!(
            "zero activation timeout returned unexpected error: {activation_error}"
        ))
        .into());
    }
    inactive.disarm(CLEANUP_TIMEOUT)?;

    let mut active = ProcessGroupGuard::new()?;
    let mut workload = WorkloadGuard::new(spawn_workload(active.process_group())?);
    active.activate(CLEANUP_TIMEOUT)?;
    let disarm_error = active
        .disarm(Duration::ZERO)
        .err()
        .ok_or_else(|| io::Error::other("zero disarm timeout was accepted"))?;
    if disarm_error.kind() != io::ErrorKind::InvalidInput {
        return Err(io::Error::other(format!(
            "zero disarm timeout returned unexpected error: {disarm_error}"
        ))
        .into());
    }
    require_killed(workload.process())?;
    workload.mark_reaped();
    Ok(())
}

fn dead_helper_makes_disarm_error_without_sigpipe() -> Result<(), Box<dyn Error>> {
    let mut guard = ProcessGroupGuard::new()?;
    let mut workload = WorkloadGuard::new(spawn_workload(guard.process_group())?);
    guard.activate(CLEANUP_TIMEOUT)?;
    let helper = guard.guard_process();
    signal_process(helper, Signal::KILL)?;
    let helper_event = wait_event(helper)?;
    if !matches!(
        helper_event,
        ChildEvent::Signalled {
            signal: Signal::KILL,
            ..
        }
    ) {
        return Err(io::Error::other(format!(
            "expected killed guard helper, received {helper_event:?}"
        ))
        .into());
    }
    let error = guard
        .disarm(CLEANUP_TIMEOUT)
        .err()
        .ok_or_else(|| io::Error::other("dead helper accepted explicit disarm"))?;
    if error.raw_os_error() != Some(libc::EPIPE) {
        return Err(io::Error::other(format!(
            "dead helper returned unexpected disarm error: {error}"
        ))
        .into());
    }

    signal_process(workload.process(), Signal::KILL)?;
    require_killed(workload.process())?;
    workload.mark_reaped();
    Ok(())
}

fn session_escape_is_explicitly_outside_group_containment() -> Result<(), Box<dyn Error>> {
    let gate = fork::pipe_cloexec()?;
    let report = fork::pipe_cloexec()?;
    let (gate_reader, gate_writer) = gate.into_parts();
    let (report_reader, report_writer) = report.into_parts();
    match fork_process()? {
        ProcessFork::Parent(process) => {
            drop(gate_reader);
            drop(report_writer);
            let mut workload = WorkloadGuard::new(process);
            let mut guard = ProcessGroupGuard::new()?;
            join_process_group(process, guard.process_group())?;
            guard.activate(CLEANUP_TIMEOUT)?;
            File::from(gate_writer).write_all(&[1])?;
            let mut report = [0_u8; 1];
            File::from(report_reader).read_exact(&mut report)?;
            if report != [1] {
                return Err(io::Error::other("workload failed to create a new session").into());
            }

            drop(guard);
            if wait_event_nohang(process)?.is_some() {
                return Err(io::Error::other(
                    "process-group guard claimed to contain a session escape",
                )
                .into());
            }
            signal_process(process, Signal::KILL)?;
            require_killed(process)?;
            workload.mark_reaped();
            Ok(())
        }
        ProcessFork::Child => {
            drop(gate_writer);
            drop(report_reader);
            let mut gate = [0_u8; 1];
            let ready = File::from(gate_reader).read_exact(&mut gate).is_ok()
                && gate == [1]
                && setsid().is_ok();
            let mut report = File::from(report_writer);
            let reported = report.write_all(&[u8::from(ready)]).is_ok();
            if !(ready && reported) {
                child_exit(1);
            }
            thread::sleep(Duration::from_secs(30));
            child_exit(0);
        }
    }
}

fn guard_supports_closed_standard_descriptor() -> Result<(), Box<dyn Error>> {
    let report = fork::pipe_cloexec()?;
    let (reader, writer) = report.into_parts();
    match fork_process()? {
        ProcessFork::Parent(process) => {
            drop(writer);
            let mut report = Vec::new();
            File::from(reader).read_to_end(&mut report)?;
            let event = wait_event(process)?;
            if report == [1] && matches!(event, ChildEvent::Exited { code: 0, .. }) {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "guard failed with closed stdin: report {report:?}, child {event:?}"
                ))
                .into())
            }
        }
        ProcessFork::Child => {
            drop(reader);
            let success = close_fd()
                .and_then(|()| ProcessGroupGuard::new())
                .and_then(|guard| guard.disarm(CLEANUP_TIMEOUT))
                .is_ok();
            let mut report = File::from(writer);
            let written = report.write_all(&[u8::from(success)]).is_ok();
            child_exit(i32::from(!(success && written)));
        }
    }
}

fn child_exit(code: libc::c_int) -> ! {
    // SAFETY: this fork-only test child must not run inherited test-harness destructors.
    unsafe { libc::_exit(code) }
}

fn helpers_close_unintended_descriptors() -> Result<(), Box<dyn Error>> {
    let inherited = fork::pipe_cloexec()?;
    let (reader, writer) = inherited.into_parts();
    let guard = ProcessGroupGuard::new()?;
    drop(writer);

    let (sender, receiver) = mpsc::channel();
    let reader_task = thread::spawn(move || {
        let mut file = File::from(reader);
        let mut byte = [0_u8; 1];
        let result = file.read(&mut byte);
        let _ = sender.send(result);
    });
    let result = match receiver.recv_timeout(Duration::from_millis(250)) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            drop(guard);
            let _ = receiver.recv_timeout(CLEANUP_TIMEOUT);
            reader_task
                .join()
                .map_err(|_| io::Error::other("descriptor reader task panicked"))?;
            return Err(io::Error::other("group helper retained an unrelated descriptor").into());
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            return Err(io::Error::other("descriptor reader task disconnected").into());
        }
    }?;
    drop(guard);
    reader_task
        .join()
        .map_err(|_| io::Error::other("descriptor reader task panicked"))?;
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::other("descriptor reader received unexpected data").into())
    }
}

fn owner_loss_kills_running_group() -> Result<(), Box<dyn Error>> {
    let mut guard = ProcessGroupGuard::new()?;
    let mut workload = WorkloadGuard::new(spawn_workload(guard.process_group())?);
    guard.activate(CLEANUP_TIMEOUT)?;

    drop(guard);
    require_killed(workload.process())?;
    workload.mark_reaped();
    Ok(())
}

fn owner_loss_kills_stopped_group() -> Result<(), Box<dyn Error>> {
    let mut guard = ProcessGroupGuard::new()?;
    let mut workload = WorkloadGuard::new(spawn_workload(guard.process_group())?);
    guard.activate(CLEANUP_TIMEOUT)?;
    signal_process_group(guard.process_group(), Signal::STOP)?;
    let stopped = wait_for_event(workload.process())?;
    if !matches!(
        stopped,
        ChildEvent::Stopped {
            signal: Signal::STOP,
            ..
        }
    ) {
        return Err(
            io::Error::other(format!("expected stopped workload, received {stopped:?}")).into(),
        );
    }

    drop(guard);
    require_killed(workload.process())?;
    workload.mark_reaped();
    Ok(())
}

fn explicit_disarm_preserves_running_group() -> Result<(), Box<dyn Error>> {
    let mut guard = ProcessGroupGuard::new()?;
    let mut workload = WorkloadGuard::new(spawn_workload(guard.process_group())?);
    guard.activate(CLEANUP_TIMEOUT)?;
    guard.disarm(CLEANUP_TIMEOUT)?;
    if wait_event_nohang(workload.process())?.is_some() {
        return Err(io::Error::other("disarm terminated the guarded workload").into());
    }

    signal_process(workload.process(), Signal::KILL)?;
    require_killed(workload.process())?;
    workload.mark_reaped();
    Ok(())
}

fn owner_loss_before_workload_leaves_no_helper() -> Result<(), Box<dyn Error>> {
    let guard = ProcessGroupGuard::new()?;
    let persistent = guard.guard_process();
    drop(guard);
    match wait_event_nohang(persistent) {
        Err(error) if error.raw_os_error() == Some(libc::ECHILD) => Ok(()),
        Ok(None) => Err(io::Error::other("group helper remained live after owner loss").into()),
        Ok(Some(event)) => Err(io::Error::other(format!(
            "group helper was not reaped after owner loss: {event:?}"
        ))
        .into()),
        Err(error) => Err(error.into()),
    }
}

fn spawn_workload(group: fork::ProcessGroupId) -> io::Result<ProcessId> {
    let mut command = PreparedCommand::new("/bin/sleep")?;
    command.arg("30")?.process_group(ProcessGroup::Join(group));
    command
        .spawn(STARTUP_TIMEOUT)
        .map(|child| child.process())
        .map_err(io::Error::other)
}

fn require_killed(process: ProcessId) -> Result<(), Box<dyn Error>> {
    require_event(process, |event| {
        matches!(
            event,
            ChildEvent::Signalled {
                signal: Signal::KILL,
                ..
            }
        )
    })
}

fn require_event(
    process: ProcessId,
    predicate: impl FnOnce(ChildEvent) -> bool,
) -> Result<(), Box<dyn Error>> {
    let event = wait_for_event(process)?;
    if predicate(event) {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "process {process} returned unexpected event: {event:?}"
        ))
        .into())
    }
}

fn require_helper_running(process: ProcessId) -> Result<(), Box<dyn Error>> {
    if wait_event_nohang(process)?.is_none() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "process-group helper {process} was affected by a workload-group signal"
        ))
        .into())
    }
}

fn require_not_a_child(process: ProcessId) -> Result<(), Box<dyn Error>> {
    match wait_event_nohang(process) {
        Err(error) if error.raw_os_error() == Some(libc::ECHILD) => Ok(()),
        Ok(None) => Err(io::Error::other(format!("helper {process} remains live")).into()),
        Ok(Some(event)) => Err(io::Error::other(format!(
            "helper {process} remained waitable after cleanup: {event:?}"
        ))
        .into()),
        Err(error) => Err(error.into()),
    }
}

fn terminate_workload(workload: &mut WorkloadGuard) -> Result<(), Box<dyn Error>> {
    signal_process(workload.process(), Signal::KILL)?;
    require_killed(workload.process())?;
    workload.mark_reaped();
    Ok(())
}

fn wait_for_event(process: ProcessId) -> io::Result<ChildEvent> {
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    loop {
        if let Some(event) = wait_event_nohang(process)? {
            return Ok(event);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out waiting for guarded workload",
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_stopped_without_reaping(process: ProcessId) -> io::Result<()> {
    let process_id = libc::id_t::try_from(i128::from(process.get())).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "helper process ID is outside the waitid range",
        )
    })?;
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    loop {
        let mut information = MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: information points to writable siginfo storage; P_PID and the
        // checked positive process ID select the owned helper without reaping it.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                process_id,
                information.as_mut_ptr(),
                libc::WSTOPPED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            // SAFETY: successful waitid initializes siginfo; si_pid is zero when
            // WNOHANG found no matching state transition.
            let observed = unsafe { information.assume_init().si_pid() };
            if observed == process.get() {
                return Ok(());
            }
            if observed != 0 {
                return Err(io::Error::other(format!(
                    "waitid observed unexpected helper {observed}"
                )));
            }
        } else {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out observing stopped helper",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

struct WorkloadGuard {
    process: ProcessId,
    reaped: bool,
}

impl WorkloadGuard {
    const fn new(process: ProcessId) -> Self {
        Self {
            process,
            reaped: false,
        }
    }

    const fn process(&self) -> ProcessId {
        self.process
    }

    const fn mark_reaped(&mut self) {
        self.reaped = true;
    }
}

impl Drop for WorkloadGuard {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let _ = signal_process(self.process, Signal::KILL);
        let _ = wait_event(self.process);
    }
}
