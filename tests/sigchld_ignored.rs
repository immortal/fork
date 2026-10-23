//! Auto-reaping preserves guard/spawn success, but never permits cleanup PID signals.
//!
//! Signal disposition is process-wide, so these contracts run without libtest.
//! The helper owns its waitable anchor independently of the owner's policy.
//! Checked daemonization still rejects auto-reaping before fork; cleanup refuses
//! to resume a stopped helper under it. Fixtures restore policy before collection.

use std::{
    error::Error,
    fs::File,
    io::{self, Read},
    mem,
    os::fd::AsRawFd,
    ptr,
    sync::atomic::{AtomicI32, Ordering},
    thread,
    time::{Duration, Instant},
};

use fork::{ChildEvent, ProcessGroupGuard, Signal, process_group_exists};

const TIMEOUT: Duration = Duration::from_secs(3);
const PIN_WINDOW: Duration = Duration::from_millis(300);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
static OWNER: AtomicI32 = AtomicI32::new(0);

fn main() -> Result<(), Box<dyn Error>> {
    for (handler, flags) in [(libc::SIG_IGN, 0), (libc::SIG_DFL, libc::SA_NOCLDWAIT)] {
        auto_reaping_preserves_creation_and_group_delivery(handler, flags)?;
        daemon_rejects_auto_reaping(handler, flags)?;
        cleanup_rejects_changed_owner_policy(handler, flags)?;
    }
    inherited_handler_cannot_reap_anchor()
}

/// Keep all signal-capable ownership until the policy is restored on every path.
fn auto_reaping_preserves_creation_and_group_delivery(
    handler: libc::sighandler_t,
    flags: libc::c_int,
) -> Result<(), Box<dyn Error>> {
    let mut guard = None;
    let action = set_sigchld(handler, flags)?;
    let result = (|| -> Result<(), Box<dyn Error>> {
        let guard = guard.insert(ProcessGroupGuard::new()?);
        let group = guard.process_group();
        if !stays_present(group)? {
            return Err("auto-reaping owner lost the helper's anchor pin".into());
        }
        let pipe = fork::pipe_cloexec()?;
        let (reader, writer) = pipe.into_parts();
        let mut reader = File::from(reader);
        let mut command = fork::PreparedCommand::new("/bin/sleep")?;
        command
            .arg("30")?
            .process_group(fork::ProcessGroup::Join(group));
        command.map_descriptor(writer, 4)?;
        command.spawn(TIMEOUT)?;
        guard.activate(TIMEOUT)?;
        guard.signal_group(Signal::KILL, TIMEOUT)?;
        let mut poll = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: the read descriptor is live and poll points to one initialized entry.
        if unsafe { libc::poll(&raw mut poll, 1, 3000) } <= 0 {
            return Err("helper-mediated signal did not close the workload lifetime".into());
        }
        let mut byte = [0];
        if reader.read(&mut byte)? != 0 || !stays_present(group)? {
            return Err("workload signal failed or released the armed anchor".into());
        }
        if fork::children_are_waitable()? {
            return Err("creation or group delivery silently changed owner policy".into());
        }
        Ok(())
    })();
    drop(action);
    if let Some(guard) = guard {
        if result.is_ok() {
            guard.disarm(TIMEOUT)?;
        } else {
            drop(guard);
        }
    }
    drain_finished_children()?;
    result
}

/// A pipe-backed spawn succeeds under auto-reaping; daemon's required wait cannot.
fn daemon_rejects_auto_reaping(
    handler: libc::sighandler_t,
    flags: libc::c_int,
) -> Result<(), Box<dyn Error>> {
    let action = set_sigchld(handler, flags)?;
    let detached = fork::checked_daemon(fork::DaemonOptions::new(), TIMEOUT);
    drop(action);
    match detached {
        Err(fork::DaemonError::OperatingSystem {
            stage: fork::DaemonStage::SignalState,
            error,
            cleanup_pending,
        }) if error.kind() == io::ErrorKind::InvalidInput && cleanup_pending.is_empty() => {}
        Ok(fork::CheckedDaemon::Daemon(notifier)) => {
            let _ = notifier.notify_ready();
            // SAFETY: a broken implementation forked; do not return into the fixture.
            unsafe { libc::_exit(0) }
        }
        other => return Err(format!("daemon accepted auto-reaping owner: {other:?}").into()),
    }
    assert!(
        matches!(fork::wait_any_event_nohang(), Err(error) if error.raw_os_error() == Some(libc::ECHILD))
    );
    Ok(())
}

/// A stopped helper cannot clean itself under auto-reaping; cleanup must refuse
/// numeric signalling rather than trust a nonterminal wait followed by auto-reap.
fn cleanup_rejects_changed_owner_policy(
    handler: libc::sighandler_t,
    flags: libc::c_int,
) -> Result<(), Box<dyn Error>> {
    let guard = ProcessGroupGuard::new()?;
    let helper = guard.guard_process();
    fork::signal_process(helper, Signal::STOP)?;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if matches!(
            fork::wait_event_nohang(helper)?,
            Some(ChildEvent::Stopped { .. })
        ) {
            break;
        }
        if Instant::now() >= deadline {
            return Err("helper did not stop".into());
        }
        thread::sleep(POLL_INTERVAL);
    }
    let action = set_sigchld(handler, flags)?;
    let result = guard.disarm(TIMEOUT);
    drop(action);
    // A stopped helper remains waitable. Restore the owner's policy before
    // resuming it, then let it consume the queued disarm and reap its anchor.
    let deadline = Instant::now() + TIMEOUT;
    let mut resumed = false;
    loop {
        match fork::wait_event_nohang(helper) {
            Ok(Some(event)) if event.is_terminal() => break,
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => break,
            Err(error) => return Err(error.into()),
            Ok(_) => {
                if !resumed {
                    fork::signal_process(helper, Signal::CONT)?;
                    resumed = true;
                }
            }
        }
        if Instant::now() >= deadline {
            fork::signal_process(helper, Signal::KILL)?;
            while !fork::wait_event(helper)?.is_terminal() {}
            return Err("resumed helper did not disarm".into());
        }
        thread::sleep(POLL_INTERVAL);
    }
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::InvalidInput));
    Ok(())
}

/// A valid owner handler does not reap its own children, but would reap the
/// helper's anchor if inherited there. Disabling the helper reset loses the pin.
extern "C" fn reap_only_in_forked_child(_signal: libc::c_int) {
    // SAFETY: getpid and waitpid are async-signal-safe; no allocation or locks.
    unsafe {
        if libc::getpid() != OWNER.load(Ordering::Relaxed) {
            while libc::waitpid(-1, ptr::null_mut(), libc::WNOHANG) > 0 {}
        }
    }
}

fn inherited_handler_cannot_reap_anchor() -> Result<(), Box<dyn Error>> {
    OWNER.store(fork::current_process_id().get(), Ordering::Relaxed);
    let action = set_sigchld(reap_only_in_forked_child as *const () as usize, 0)?;
    let created = ProcessGroupGuard::new();
    drop(action);
    let guard = created?;
    let group = guard.process_group();
    let pinned = stays_present(group);
    guard.disarm(TIMEOUT)?;
    assert!(pinned?, "inherited handler reaped the helper's anchor");
    assert!(!process_group_exists(group)?);
    Ok(())
}

/// After every signal owner is retired, collect any late exits from this fixture.
fn drain_finished_children() -> io::Result<()> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match fork::wait_any_event_nohang() {
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => return Ok(()),
            Err(error) => return Err(error),
            Ok(_) => {}
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "fixture children remained",
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn stays_present(group: fork::ProcessGroupId) -> io::Result<bool> {
    let deadline = Instant::now() + PIN_WINDOW;
    while Instant::now() < deadline {
        if !process_group_exists(group)? {
            return Ok(false);
        }
        thread::sleep(POLL_INTERVAL);
    }
    process_group_exists(group)
}

struct Action(libc::sigaction);
impl Drop for Action {
    fn drop(&mut self) {
        // SAFETY: the saved disposition was initialized by sigaction.
        unsafe {
            libc::sigaction(libc::SIGCHLD, &raw const self.0, ptr::null_mut());
        }
    }
}

fn set_sigchld(handler: libc::sighandler_t, flags: libc::c_int) -> io::Result<Action> {
    // SAFETY: these C structures are initialized before use below.
    let mut action: libc::sigaction = unsafe { mem::zeroed() };
    // SAFETY: sigaction initializes the previous disposition on success.
    let mut previous: libc::sigaction = unsafe { mem::zeroed() };
    action.sa_sigaction = handler;
    action.sa_flags = flags;
    // SAFETY: all pointers refer to initialized storage in this standalone test.
    unsafe {
        libc::sigemptyset(&raw mut action.sa_mask);
        if libc::sigaction(libc::SIGCHLD, &raw const action, &raw mut previous) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(Action(previous))
}
