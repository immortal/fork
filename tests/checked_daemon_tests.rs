//! Native acceptance tests for bounded checked double-fork startup.

use std::{
    error::Error,
    fs,
    io::{self, Read},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use fork::{
    CheckedDaemon, DaemonCleanup, DaemonError, DaemonOptions, DaemonProcess, DaemonStage,
    checked_daemon, pipe_cloexec,
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(5);
static NEXT_PATH: AtomicU64 = AtomicU64::new(0);
static INTERRUPTS: AtomicU64 = AtomicU64::new(0);

struct SignalActionGuard {
    signal: libc::c_int,
    previous: libc::sigaction,
}

impl Drop for SignalActionGuard {
    fn drop(&mut self) {
        // SAFETY: previous was initialized by a successful sigaction call.
        unsafe {
            libc::sigaction(self.signal, &raw const self.previous, std::ptr::null_mut());
        }
    }
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> io::Result<Self> {
        let sequence = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fork-checked-daemon-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn parent_returns_only_after_detached_daemon_is_ready() -> Result<(), Box<dyn Error>> {
    let directory = TestDirectory::new()?;
    let marker = directory.path().join("ready");
    let mut options = DaemonOptions::new();
    options
        .current_directory(directory.path())?
        .redirect_standard_io_to_null()?;

    match checked_daemon(options, STARTUP_TIMEOUT)? {
        CheckedDaemon::Parent(process) => {
            assert_ne!(process.process().get(), std::process::id().cast_signed());
            assert!(marker.is_file());
            wait_for_group_exit(process)?;
        }
        CheckedDaemon::Daemon(notifier) => {
            if !standard_io_is_dev_null()
                || fs::write("ready", b"initialized").is_err()
                || notifier.notify_ready().is_err()
            {
                daemon_exit(2);
            }
            daemon_exit(0);
        }
    }
    Ok(())
}

#[test]
fn failure_after_detachment_preserves_stage_and_errno() -> Result<(), Box<dyn Error>> {
    let missing =
        std::env::temp_dir().join(format!("fork-missing-daemon-dir-{}", std::process::id()));
    let mut options = DaemonOptions::new();
    options.current_directory(&missing)?;

    match checked_daemon(options, STARTUP_TIMEOUT) {
        Err(DaemonError::OperatingSystem {
            stage,
            error,
            cleanup_pending,
        }) => {
            assert_eq!(stage, DaemonStage::CurrentDirectory);
            assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
            assert!(cleanup_pending.is_empty());
        }
        other => return Err(io::Error::other(format!("unexpected result: {other:?}")).into()),
    }
    Ok(())
}

#[test]
fn descriptor_plan_failure_is_reported_before_fork() -> Result<(), Box<dyn Error>> {
    let pipe = pipe_cloexec()?;
    let (_, writer) = pipe.into_parts();
    let mut options = DaemonOptions::new();
    options.map_descriptor(writer, libc::c_int::MAX)?;

    match checked_daemon(options, STARTUP_TIMEOUT) {
        Err(DaemonError::OperatingSystem {
            stage,
            error,
            cleanup_pending,
        }) => {
            assert_eq!(stage, DaemonStage::Descriptors);
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(cleanup_pending.is_empty());
        }
        other => return Err(io::Error::other(format!("unexpected result: {other:?}")).into()),
    }
    Ok(())
}

#[test]
fn daemon_can_report_initialization_failure() -> Result<(), Box<dyn Error>> {
    match checked_daemon(DaemonOptions::new(), STARTUP_TIMEOUT) {
        Ok(CheckedDaemon::Daemon(notifier)) => {
            notifier.fail_and_exit(&io::Error::from_raw_os_error(libc::EACCES));
        }
        Err(DaemonError::OperatingSystem {
            stage,
            error,
            cleanup_pending,
        }) => {
            assert_eq!(stage, DaemonStage::Initialization);
            assert_eq!(error.raw_os_error(), Some(libc::EACCES));
            assert!(cleanup_pending.is_empty());
        }
        other => return Err(io::Error::other(format!("unexpected result: {other:?}")).into()),
    }
    Ok(())
}

#[test]
fn dropped_notifier_is_reported_as_abandoned_startup() -> Result<(), Box<dyn Error>> {
    match checked_daemon(DaemonOptions::new(), STARTUP_TIMEOUT) {
        Ok(CheckedDaemon::Daemon(notifier)) => {
            drop(notifier);
            daemon_exit(3);
        }
        Err(DaemonError::Abandoned { cleanup_pending }) => {
            assert!(cleanup_pending.is_empty());
        }
        other => return Err(io::Error::other(format!("unexpected result: {other:?}")).into()),
    }
    Ok(())
}

#[test]
fn startup_timeout_kills_the_owned_daemon_group() -> Result<(), Box<dyn Error>> {
    let timeout = Duration::from_millis(25);
    let (reader, release) = pipe_cloexec()?.into_parts();
    let descriptor = reader.as_raw_fd();
    let mut options = DaemonOptions::new();
    options.preserve_descriptor(reader)?;
    match checked_daemon(options, timeout) {
        Ok(CheckedDaemon::Daemon(_notifier)) => {
            // Do not report ready. The parent holds `release` until it has
            // checked the timeout's kill, then EOF cleans even a broken kill
            // implementation without ever signalling an unpinned group ID.
            let deadline = Instant::now() + STARTUP_TIMEOUT * 5;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    daemon_exit(6);
                }
                let mut event = libc::pollfd {
                    fd: descriptor,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let milliseconds =
                    libc::c_int::try_from(remaining.as_millis().max(1)).unwrap_or(libc::c_int::MAX);
                // SAFETY: the descriptor is explicitly preserved; event is live storage.
                let ready = unsafe { libc::poll(&raw mut event, 1, milliseconds) };
                if ready >= 0 {
                    daemon_exit(6);
                }
                if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    daemon_exit(7);
                }
            }
        }
        Err(DaemonError::TimedOut {
            cleanup_pending, ..
        }) => {
            let killed = finish_pending_cleanup(cleanup_pending);
            drop(release);
            // Cleanup precedes the assertion even if the tested group kill was omitted.
            finish_pending_cleanup(cleanup_pending)?;
            killed?;
        }
        other => return Err(io::Error::other(format!("unexpected result: {other:?}")).into()),
    }
    Ok(())
}

#[test]
fn preserved_descriptor_survives_detachment() -> Result<(), Box<dyn Error>> {
    let pipe = pipe_cloexec()?;
    let (reader, writer) = pipe.into_parts();
    let descriptor = writer.as_raw_fd();
    let mut options = DaemonOptions::new();
    options.preserve_descriptor(writer)?;

    match checked_daemon(options, STARTUP_TIMEOUT)? {
        CheckedDaemon::Parent(process) => {
            let mut output = String::new();
            std::fs::File::from(reader).read_to_string(&mut output)?;
            assert_eq!(output, "preserved");
            wait_for_group_exit(process)?;
        }
        CheckedDaemon::Daemon(notifier) => {
            // SAFETY: the descriptor is explicitly preserved and the buffer is live.
            let written = unsafe { libc::write(descriptor, c"preserved".as_ptr().cast(), 9) };
            if written != 9 || notifier.notify_ready().is_err() {
                daemon_exit(4);
            }
            daemon_exit(0);
        }
    }
    Ok(())
}

#[test]
fn startup_handshake_retries_repeated_eintr() -> Result<(), Box<dyn Error>> {
    INTERRUPTS.store(0, Ordering::SeqCst);
    let _signal_guard = install_signal_handler(libc::SIGUSR1)?;
    // SAFETY: getpid has no failure mode or pointer arguments.
    let original = unsafe { libc::getpid() };

    match checked_daemon(DaemonOptions::new(), STARTUP_TIMEOUT)? {
        CheckedDaemon::Parent(process) => {
            assert!(INTERRUPTS.load(Ordering::SeqCst) > 0);
            wait_for_group_exit(process)?;
        }
        CheckedDaemon::Daemon(notifier) => {
            for _ in 0..8 {
                // SAFETY: original is the checked positive PID captured before fork.
                unsafe {
                    libc::kill(original, libc::SIGUSR1);
                    libc::usleep(1_000);
                }
            }
            if notifier.notify_ready().is_err() {
                daemon_exit(5);
            }
            daemon_exit(0);
        }
    }
    Ok(())
}

fn finish_pending_cleanup(cleanup: DaemonCleanup) -> Result<(), Box<dyn Error>> {
    if cleanup.intermediate().is_some() {
        return Err(io::Error::other("intermediate child remained after timeout cleanup").into());
    }
    if let Some(group) = cleanup.process_group() {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        while group_exists(group.get()) {
            if Instant::now() >= deadline {
                return Err(io::Error::other("daemon group remained after timeout cleanup").into());
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
    Ok(())
}

fn wait_for_group_exit(process: DaemonProcess) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while group_exists(process.process_group().get()) {
        if Instant::now() >= deadline {
            return Err(io::Error::other("ready daemon did not exit").into());
        }
        thread::sleep(POLL_INTERVAL);
    }
    Ok(())
}

fn group_exists(group: libc::pid_t) -> bool {
    loop {
        // SAFETY: signal zero is confined to test cleanup verification.
        let result = unsafe { libc::kill(-group, 0) };
        if result == 0 {
            return true;
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return error.raw_os_error() != Some(libc::ESRCH);
    }
}

fn daemon_exit(code: libc::c_int) -> ! {
    // SAFETY: test daemon termination must not run the parent test harness.
    unsafe { libc::_exit(code) }
}

fn standard_io_is_dev_null() -> bool {
    // SAFETY: both stat buffers are initialized by successful stat/fstat calls
    // before their device fields are inspected.
    unsafe {
        let mut expected: libc::stat = std::mem::zeroed();
        if libc::stat(c"/dev/null".as_ptr(), &raw mut expected) == -1 {
            return false;
        }
        for descriptor in libc::STDIN_FILENO..=libc::STDERR_FILENO {
            let mut actual: libc::stat = std::mem::zeroed();
            if libc::fstat(descriptor, &raw mut actual) == -1 || actual.st_rdev != expected.st_rdev
            {
                return false;
            }
        }
    }
    true
}

extern "C" fn record_interrupt(_signal: libc::c_int) {
    INTERRUPTS.fetch_add(1, Ordering::SeqCst);
}

fn install_signal_handler(signal: libc::c_int) -> io::Result<SignalActionGuard> {
    // SAFETY: both values are initialized before installation/use.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: sigaction initializes this output on success.
    let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = record_interrupt as *const () as usize;
    action.sa_flags = 0;
    // SAFETY: pointers remain valid for both calls and mask is initialized first.
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
