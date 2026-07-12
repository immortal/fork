//! Native lifecycle tests for the typed process, signal, group, and wait APIs.

use std::{
    collections::BTreeSet,
    error::Error,
    io,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use fork::{
    ChildEvent, ProcessFork, ProcessId, Signal, create_current_process_group, create_process_group,
    current_process_id, fork_process, join_process_group, process_group, signal_process,
    signal_process_group, wait_any_event_nohang, wait_event, wait_event_nohang, waitpid,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(5);

static SIGNAL_RECEIVED: AtomicBool = AtomicBool::new(false);
static TEST_SERIAL: Mutex<()> = Mutex::new(());

struct ChildGuard {
    process: ProcessId,
    reaped: bool,
}

impl ChildGuard {
    const fn new(process: ProcessId) -> Self {
        Self {
            process,
            reaped: false,
        }
    }

    const fn process(&self) -> ProcessId {
        self.process
    }

    fn mark_reaped(&mut self) {
        self.reaped = true;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let _ = signal_process(self.process, Signal::KILL);
        let _ = waitpid(self.process.get());
    }
}

struct SignalActionGuard {
    signal: libc::c_int,
    previous: libc::sigaction,
}

impl Drop for SignalActionGuard {
    fn drop(&mut self) {
        // SAFETY: `previous` was populated by a successful sigaction call and
        // remains initialized for this process until restoration.
        unsafe {
            libc::sigaction(self.signal, &raw const self.previous, std::ptr::null_mut());
        }
    }
}

fn test_lock() -> io::Result<MutexGuard<'static, ()>> {
    TEST_SERIAL
        .lock()
        .map_err(|_| io::Error::other("typed process test lock poisoned"))
}

#[test]
fn typed_wait_reports_exact_exit() -> Result<(), Box<dyn Error>> {
    let _lock = test_lock()?;
    let mut child = spawn_exiting(42, Duration::ZERO)?;
    let event = wait_for_process_event(child.process())?;
    assert_eq!(
        event,
        ChildEvent::Exited {
            pid: child.process(),
            code: 42
        }
    );
    child.mark_reaped();
    Ok(())
}

#[test]
fn stop_continue_and_termination_are_distinct_events() -> Result<(), Box<dyn Error>> {
    let _lock = test_lock()?;
    let mut child = spawn_paused()?;

    signal_process(child.process(), Signal::STOP)?;
    assert_eq!(
        wait_for_process_event(child.process())?,
        ChildEvent::Stopped {
            pid: child.process(),
            signal: Signal::STOP
        }
    );

    signal_process(child.process(), Signal::CONT)?;
    assert_eq!(
        wait_for_process_event(child.process())?,
        ChildEvent::Continued {
            pid: child.process()
        }
    );

    signal_process(child.process(), Signal::TERM)?;
    assert_eq!(
        wait_for_process_event(child.process())?,
        ChildEvent::Signalled {
            pid: child.process(),
            signal: Signal::TERM
        }
    );
    child.mark_reaped();
    Ok(())
}

#[test]
fn group_signal_reaches_every_owned_member() -> Result<(), Box<dyn Error>> {
    let _lock = test_lock()?;
    let leader = spawn_paused()?;
    let group = create_process_group(leader.process())?;
    assert_eq!(process_group(leader.process())?, group);

    let member = spawn_paused()?;
    join_process_group(member.process(), group)?;
    assert_eq!(process_group(member.process())?, group);

    let mut children = [leader, member];
    signal_process_group(group, Signal::TERM)?;

    let expected: BTreeSet<ProcessId> = children.iter().map(ChildGuard::process).collect();
    let mut reaped = BTreeSet::new();
    let deadline = Instant::now() + TEST_TIMEOUT;
    while reaped.len() < expected.len() {
        if let Some(event) = wait_any_event_nohang()? {
            match event {
                ChildEvent::Signalled { pid, signal } => {
                    assert_eq!(signal, Signal::TERM);
                    assert!(expected.contains(&pid));
                    reaped.insert(pid);
                }
                ChildEvent::Exited { pid, code } => {
                    return Err(io::Error::other(format!(
                        "group member {pid} exited unexpectedly with {code}"
                    ))
                    .into());
                }
                ChildEvent::Stopped { .. } | ChildEvent::Continued { .. } => {}
            }
        } else if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out reaping signalled process group",
            )
            .into());
        } else {
            thread::sleep(POLL_INTERVAL);
        }
    }

    assert_eq!(reaped, expected);
    for child in &mut children {
        if reaped.contains(&child.process()) {
            child.mark_reaped();
        }
    }
    Ok(())
}

#[test]
fn current_process_can_create_its_group_after_fork() -> Result<(), Box<dyn Error>> {
    let _lock = test_lock()?;
    match fork_process()? {
        ProcessFork::Parent(process) => {
            let mut child = ChildGuard::new(process);
            assert_eq!(
                wait_for_process_event(process)?,
                ChildEvent::Exited {
                    pid: process,
                    code: 0
                }
            );
            child.mark_reaped();
        }
        ProcessFork::Child => {
            let code = i32::from(create_current_process_group().is_err());
            child_exit(code);
        }
    }
    Ok(())
}

#[test]
fn wait_any_nohang_drains_multiple_terminal_events() -> Result<(), Box<dyn Error>> {
    let _lock = test_lock()?;
    let mut children = [
        spawn_exiting(10, Duration::from_millis(10))?,
        spawn_exiting(11, Duration::from_millis(20))?,
        spawn_exiting(12, Duration::from_millis(30))?,
    ];
    let expected: BTreeSet<ProcessId> = children.iter().map(ChildGuard::process).collect();
    let mut reaped = BTreeSet::new();
    let deadline = Instant::now() + TEST_TIMEOUT;

    while reaped.len() < expected.len() {
        if let Some(event) = wait_any_event_nohang()? {
            assert!(event.is_terminal());
            assert!(expected.contains(&event.pid()));
            reaped.insert(event.pid());
        } else if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "timed out draining terminal child events",
            )
            .into());
        } else {
            thread::sleep(POLL_INTERVAL);
        }
    }

    for child in &mut children {
        if reaped.contains(&child.process()) {
            child.mark_reaped();
        }
    }
    Ok(())
}

#[test]
fn typed_blocking_wait_retries_after_eintr() -> Result<(), Box<dyn Error>> {
    let _lock = test_lock()?;
    SIGNAL_RECEIVED.store(false, Ordering::SeqCst);
    let _signal_guard = install_signal_handler(libc::SIGUSR1)?;
    let mut child = spawn_exiting(7, Duration::from_millis(100))?;
    let parent = current_process_id();
    let child_process = child.process();
    let (completed_tx, completed_rx) = mpsc::channel();
    let interrupter = thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        signal_process(parent, Signal::USR1)?;
        if completed_rx.recv_timeout(TEST_TIMEOUT).is_err() {
            let _ = signal_process(child_process, Signal::KILL);
        }
        Ok::<(), io::Error>(())
    });

    let event = wait_event(child.process());
    let _ = completed_tx.send(());
    let interrupt_result = interrupter
        .join()
        .map_err(|_| io::Error::other("signal interrupter panicked"))?;
    interrupt_result?;
    let event = event?;

    assert_eq!(
        event,
        ChildEvent::Exited {
            pid: child.process(),
            code: 7
        }
    );
    assert!(SIGNAL_RECEIVED.load(Ordering::SeqCst));
    child.mark_reaped();
    Ok(())
}

fn spawn_exiting(code: libc::c_int, delay: Duration) -> io::Result<ChildGuard> {
    match fork_process()? {
        ProcessFork::Parent(process) => Ok(ChildGuard::new(process)),
        ProcessFork::Child => {
            let micros = delay.as_micros().min(u128::from(u32::MAX));
            let micros = u32::try_from(micros).unwrap_or(u32::MAX);
            if micros != 0 {
                // SAFETY: usleep accepts every u32 value and the child performs
                // no shared-memory work before immediately calling _exit.
                unsafe {
                    libc::usleep(micros);
                }
            }
            child_exit(code);
        }
    }
}

fn spawn_paused() -> io::Result<ChildGuard> {
    match fork_process()? {
        ProcessFork::Parent(process) => Ok(ChildGuard::new(process)),
        ProcessFork::Child => loop {
            // SAFETY: pause has no pointer preconditions. Signals control this
            // dedicated test child, which exits only through a terminal signal.
            unsafe {
                libc::pause();
            }
        },
    }
}

fn child_exit(code: libc::c_int) -> ! {
    // SAFETY: _exit is async-signal-safe and intentionally skips Rust cleanup
    // in the post-fork child.
    unsafe { libc::_exit(code) }
}

fn wait_for_process_event(process: ProcessId) -> io::Result<ChildEvent> {
    let deadline = Instant::now() + TEST_TIMEOUT;
    loop {
        if let Some(event) = wait_event_nohang(process)? {
            return Ok(event);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("timed out waiting for child {process}"),
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

extern "C" fn record_signal(_signal: libc::c_int) {
    SIGNAL_RECEIVED.store(true, Ordering::SeqCst);
}

fn install_signal_handler(signal: libc::c_int) -> io::Result<SignalActionGuard> {
    // SAFETY: zeroed sigaction is initialized below before it is installed.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: previous is an output buffer fully initialized by sigaction on
    // success.
    let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = record_signal as *const () as usize;
    action.sa_flags = 0;
    // SAFETY: action owns a valid signal set and both sigaction pointers are
    // valid for the duration of the calls.
    unsafe {
        if libc::sigemptyset(&raw mut action.sa_mask) == -1 {
            return Err(io::Error::last_os_error());
        }
        if libc::sigaction(signal, &raw const action, &raw mut previous) == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(SignalActionGuard { signal, previous })
}
