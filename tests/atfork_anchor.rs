//! Application `pthread_atfork` callbacks must never run in the guard helper.
//!
//! Registrations are permanent and process-wide, so each case runs in a fresh
//! process without libtest. The callbacks act only outside the owner: one reaps
//! the helper's children, the other restores SIGCHLD auto-reaping in the
//! helper while delaying the anchor so it exits after startup. Either loses the
//! anchor pin when the helper creates its anchor with `fork(3)`. Each schedule
//! ends in disarm and in owner loss; every fixture child is collected.

use std::{
    env,
    error::Error,
    fs::File,
    io::{self, Read},
    os::fd::AsRawFd,
    process::Command,
    ptr,
    sync::atomic::{AtomicI32, Ordering},
    thread,
    time::{Duration, Instant},
};

use fork::{
    PreparedCommand, ProcessGroup, ProcessGroupGuard, ProcessGroupId, ProcessId, Signal,
    process_group_exists,
};

const CASE: &str = "FORK_ATFORK_CASE";
const CASES: [&str; 4] = [
    "reaper-disarm",
    "reaper-loss",
    "policy-disarm",
    "policy-loss",
];
const TIMEOUT: Duration = Duration::from_secs(3);
/// Longer than the anchor delay below, so a lost pin is observed.
const PIN_WINDOW: Duration = Duration::from_millis(600);
const ANCHOR_DELAY: libc::timespec = libc::timespec {
    tv_sec: 0,
    tv_nsec: 300_000_000,
};
const POLL_INTERVAL: Duration = Duration::from_millis(5);
const CASE_BOUND: Duration = Duration::from_secs(20);
static OWNER: AtomicI32 = AtomicI32::new(0);

fn main() -> Result<(), Box<dyn Error>> {
    if let Ok(case) = env::var(CASE) {
        return run_case(&case);
    }
    for case in CASES {
        let mut child = Command::new(env::current_exe()?).env(CASE, case).spawn()?;
        let deadline = Instant::now() + CASE_BOUND;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                return Err(format!("{case}: case exceeded its bound").into());
            }
            thread::sleep(POLL_INTERVAL);
        };
        if !status.success() {
            return Err(format!("{case}: {status}").into());
        }
    }
    Ok(())
}

/// In the helper (any non-owner parent), collect every child, blocking for the
/// anchor's exit. The owner's own forks are untouched.
extern "C" fn reap_outside_owner() {
    // SAFETY: getpid and waitpid are async-signal-safe; no allocation or locks.
    unsafe {
        if libc::getpid() != OWNER.load(Ordering::Relaxed) {
            while libc::waitpid(-1, ptr::null_mut(), 0) > 0 {}
        }
    }
}

/// In the helper, restore SIGCHLD auto-reaping after a fork.
extern "C" fn ignore_sigchld_outside_owner() {
    // SAFETY: getpid, sigemptyset and sigaction are async-signal-safe; the
    // zeroed action is initialized before use and changes only this process.
    unsafe {
        if libc::getpid() != OWNER.load(Ordering::Relaxed) {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = libc::SIG_IGN;
            libc::sigemptyset(&raw mut action.sa_mask);
            libc::sigaction(libc::SIGCHLD, &raw const action, ptr::null_mut());
        }
    }
}

/// In a grandchild of the owner (the anchor), delay so it exits after startup.
extern "C" fn delay_grandchild() {
    // SAFETY: getppid and nanosleep are async-signal-safe; the remaining time
    // is written to local storage and the bounded delay resumes after EINTR.
    unsafe {
        if libc::getppid() != OWNER.load(Ordering::Relaxed) {
            let mut remaining = ANCHOR_DELAY;
            while libc::nanosleep(&raw const remaining, &raw mut remaining) != 0
                && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
            {}
        }
    }
}

fn run_case(case: &str) -> Result<(), Box<dyn Error>> {
    OWNER.store(fork::current_process_id().get(), Ordering::Relaxed);
    let (parent, child): (unsafe extern "C" fn(), Option<unsafe extern "C" fn()>) =
        if case.starts_with("reaper") {
            (reap_outside_owner, None)
        } else {
            (ignore_sigchld_outside_owner, Some(delay_grandchild))
        };
    // SAFETY: the callbacks are async-signal-safe and live for the process.
    if unsafe { libc::pthread_atfork(None, Some(parent), child) } != 0 {
        return Err("pthread_atfork failed".into());
    }
    let result = guarded_lifecycle(case.ends_with("loss"));
    drain_children()?;
    result
}

fn guarded_lifecycle(owner_loss: bool) -> Result<(), Box<dyn Error>> {
    let guard = ProcessGroupGuard::new()?;
    let group = guard.process_group();
    if !stays_present(group)? {
        return Err("an atfork callback released the armed anchor pin".into());
    }
    let (workload, mut lifetime) = spawn_workload(group)?;
    let mut guard = Some(guard);
    if let Some(guard) = guard.as_mut() {
        guard.activate(TIMEOUT)?;
    }
    if owner_loss {
        drop(guard.take());
    } else if let Some(guard) = guard.as_ref() {
        guard.signal_group(Signal::KILL, TIMEOUT)?;
    }
    let closed = lifetime_closed(&mut lifetime);
    if closed.is_err() {
        // Still this fixture's unreaped child, so its PID is safe to signal.
        fork::signal_process(workload, Signal::KILL)?;
    }
    let event = fork::wait_event(workload)?;
    closed?;
    if !matches!(
        event,
        fork::ChildEvent::Signalled {
            signal: Signal::KILL,
            ..
        }
    ) {
        return Err(format!("workload was not killed: {event:?}").into());
    }
    if let Some(guard) = guard {
        guard.disarm(TIMEOUT)?;
    }
    if process_group_exists(group)? {
        return Err("the group outlived its guard".into());
    }
    if !fork::children_are_waitable()? {
        return Err("a helper-only callback changed the owner's policy".into());
    }
    Ok(())
}

/// Start `sleep` in the group, holding a pipe end whose closure proves death.
fn spawn_workload(group: ProcessGroupId) -> Result<(ProcessId, File), Box<dyn Error>> {
    let (reader, writer) = fork::pipe_cloexec()?.into_parts();
    let mut command = PreparedCommand::new("/bin/sleep")?;
    command.arg("30")?.process_group(ProcessGroup::Join(group));
    command.map_descriptor(writer, 4)?;
    let child = command.spawn(TIMEOUT)?;
    Ok((child.process(), File::from(reader)))
}

fn lifetime_closed(reader: &mut File) -> Result<(), Box<dyn Error>> {
    let mut poll = libc::pollfd {
        fd: reader.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: the descriptor is live and poll points to one initialized entry.
    if unsafe { libc::poll(&raw mut poll, 1, 3000) } <= 0 {
        return Err("the workload survived the group signal".into());
    }
    let mut byte = [0];
    if reader.read(&mut byte)? != 0 {
        return Err("unexpected workload output".into());
    }
    Ok(())
}

/// The group must remain present for the whole window while armed.
fn stays_present(group: ProcessGroupId) -> io::Result<bool> {
    let deadline = Instant::now() + PIN_WINDOW;
    while Instant::now() < deadline {
        if !process_group_exists(group)? {
            return Ok(false);
        }
        thread::sleep(POLL_INTERVAL);
    }
    Ok(true)
}

/// Collect everything this fixture started, including late exits.
fn drain_children() -> io::Result<()> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match fork::wait_any_event_nohang() {
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => return Ok(()),
            Err(error) => return Err(error),
            Ok(_) if Instant::now() >= deadline => {
                return Err(io::Error::other("fixture children did not finish"));
            }
            Ok(_) => thread::sleep(POLL_INTERVAL),
        }
    }
}
