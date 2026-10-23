//! Private interposition driver and finite helper/spawn cleanup scenarios.

use std::{
    env,
    error::Error,
    fs, io, mem,
    path::PathBuf,
    process::{Command, ExitStatus, Stdio},
    ptr, thread,
    time::{Duration, Instant},
};

use fork::{ChildEvent, ProcessGroupGuard, ProcessId, Signal};

#[path = "inherited_reaper.rs"]
mod inherited_reaper;
#[path = "waitid_fallback.rs"]
mod waitid_fallback;

const BOUND: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(5);
const CLEANUP: Duration = Duration::from_millis(50);

/// Exercise the production cleanup paths, never an extracted implementation.
pub(super) fn run() -> Result<(), Box<dyn Error>> {
    if let Ok(mode) = env::var("FORK_CLEANUP_MODE") {
        fork::acquire_subreaper()?;
        let _children = Children;
        return match mode.as_str() {
            "guard-ignore" | "guard-nocldwait" => stopped_auto_owner(&mode),
            "spawn-ignore" | "spawn-nocldwait" => failed_auto_spawn(&mode),
            "guard-deadline" => deadline_collection(),
            "continue-handler" => inherited_continue_handler(),
            "continue-denied" => unavailable_continue(),
            "reaper-delivery" => inherited_reaper::run(false),
            "reaper-loss" => inherited_reaper::run(true),
            "waitid-daemon" | "waitid-failure" | "waitid-guard" => waitid_fallback::run(&mode),
            _ => Err("unknown cleanup mode".into()),
        };
    }
    fork::acquire_subreaper()?;
    let directory =
        Temporary(env::temp_dir().join(format!("fork-owner-cleanup-{}", std::process::id())));
    fs::create_dir(&directory.0)?;
    let _children = Children;
    let source = directory.0.join("schedule.c");
    let library = directory.0.join("schedule.so");
    fs::write(&source, include_str!("schedule.c"))?;
    if !run_command(
        Command::new("cc")
            .args(["-shared", "-fPIC", "-Wall", "-Werror"])
            .arg(&source)
            .args(["-ldl", "-o"])
            .arg(&library),
    )?
    .success()
    {
        return Err("cleanup shim compilation failed".into());
    }
    for (mode, denial) in cases() {
        if env::var("FORK_ONLY_CASE").is_ok_and(|selected| selected != mode)
            || env::var("FORK_ONLY_ERRNO")
                .is_ok_and(|selected| denial.is_none_or(|errno| selected != errno.to_string()))
        {
            continue;
        }
        let case = denial.map_or_else(|| mode.to_owned(), |errno| format!("{mode}-{errno}"));
        let log = directory.0.join(&case);
        let pin_log = directory.0.join(format!("pins-{case}"));
        let mut command = Command::new(env::current_exe()?);
        command
            .env("FORK_CLEANUP_MODE", mode)
            .env(
                "FORK_CLEANUP_RELEASE",
                directory.0.join(format!("release-{case}")),
            )
            .env("LD_PRELOAD", &library)
            .env("FORK_PIN_LOG", &pin_log)
            .stderr(Stdio::from(fs::File::create(&log)?));
        if let Some(errno) = denial {
            command.env("FORK_WAITID_ERRNO", errno.to_string());
        }
        let status = run_command(&mut command)?;
        let output = fs::read_to_string(&log)?;
        let pins = fs::read_to_string(&pin_log).unwrap_or_default();
        eprintln!("{pins}");
        eprintln!("{case}: {status}: {output}");
        if !status.success()
            || output.contains("UNSAFE")
            || pins.contains("UNSAFE")
            || (mode.starts_with("reaper-") && !pins.contains("PINNED"))
            || (mode == "guard-deadline" && !output.contains("STOLE"))
            || (mode == "waitid-guard" && !output.contains("CONSUMED"))
        {
            return Err(format!("cleanup regression failed: {case}").into());
        }
    }
    Ok(())
}

/// Every schedule, with each waitid schedule repeated per denial errno.
///
/// waitid is optional, so seccomp `ENOSYS`/`EPERM`, an LSM's `EACCES`, an old
/// kernel's `EINVAL` (no `WNOWAIT`) and any other errno, here `EIO`, must all
/// keep 0.10.0's exact-child waitpid path.
fn cases() -> Vec<(&'static str, Option<libc::c_int>)> {
    let mut cases: Vec<_> = [
        "guard-ignore",
        "guard-nocldwait",
        "spawn-ignore",
        "spawn-nocldwait",
        "guard-deadline",
        "continue-handler",
        "continue-denied",
        "reaper-delivery",
        "reaper-loss",
    ]
    .into_iter()
    .map(|mode| (mode, None))
    .collect();
    for mode in ["waitid-daemon", "waitid-failure", "waitid-guard"] {
        for errno in [
            libc::ENOSYS,
            libc::EPERM,
            libc::EACCES,
            libc::EINVAL,
            libc::EIO,
        ] {
            cases.push((mode, Some(errno)));
        }
    }
    cases
}

/// Stop while the owner is waitable, then prove auto-reaping forbids its resume.
fn stopped_auto_owner(mode: &str) -> Result<(), Box<dyn Error>> {
    let guard = ProcessGroupGuard::new()?;
    let helper = guard.guard_process();
    stop(helper)?;
    let action = install_auto_policy(mode)?;
    let result = guard.disarm(CLEANUP);
    drop(action);
    finish(helper)?;
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::InvalidInput));
    Ok(())
}

/// A stopped pre-exec child cannot be safely killed under an auto-reaping owner.
fn failed_auto_spawn(mode: &str) -> Result<(), Box<dyn Error>> {
    let action = install_auto_policy(mode)?;
    let result = fork::PreparedCommand::new("/bin/true")?.spawn(CLEANUP);
    drop(action);
    let error = result
        .err()
        .ok_or("stopped pre-exec child unexpectedly succeeded")?;
    let child = error
        .cleanup_pending()
        .ok_or("failed spawn did not report its unkillable child")?;
    finish(child)?;
    assert!(
        matches!(error, fork::SpawnError::TimedOut { .. }),
        "{error:?}"
    );
    Ok(())
}

/// The shim consumes the helper after the last idle wait, before deadline KILL.
fn deadline_collection() -> Result<(), Box<dyn Error>> {
    let guard = ProcessGroupGuard::new()?;
    let helper = guard.guard_process();
    stop(helper)?;
    let result = guard.disarm(CLEANUP);
    finish(helper)?;
    assert!(result.is_err(), "lost helper ownership was hidden");
    Ok(())
}

/// An owner callback must not be invoked by cleanup's new helper-only CONT.
extern "C" fn exit_on_continue(_: libc::c_int) {
    // SAFETY: _exit is async-signal-safe and terminates only this fixture.
    unsafe { libc::_exit(42) }
}

fn inherited_continue_handler() -> Result<(), Box<dyn Error>> {
    let _action = install(libc::SIGCONT, exit_on_continue as *const () as usize, 0)?;
    let guard = ProcessGroupGuard::new()?;
    guard.disarm(BOUND)?;
    Ok(())
}

/// A denied resume must not fail a running helper's otherwise successful disarm.
fn unavailable_continue() -> Result<(), Box<dyn Error>> {
    ProcessGroupGuard::new()?.disarm(BOUND)?;
    Ok(())
}

/// Keep STOP pending state out of the production wait, matching an all-child drain.
fn stop(child: ProcessId) -> io::Result<()> {
    fork::signal_process(child, Signal::STOP)?;
    let deadline = Instant::now() + BOUND;
    loop {
        if matches!(
            fork::wait_event_nohang(child)?,
            Some(ChildEvent::Stopped { .. })
        ) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "helper did not stop",
            ));
        }
        thread::sleep(POLL);
    }
}

/// Only an exact nonterminal wait under restored default policy permits cleanup.
fn finish(child: ProcessId) -> io::Result<()> {
    match fork::wait_event_nohang(child) {
        Ok(Some(event)) if event.is_terminal() => return Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::ECHILD) => return Ok(()),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    fork::signal_process(child, Signal::CONT)?;
    let deadline = Instant::now() + BOUND;
    loop {
        match fork::wait_event_nohang(child) {
            Ok(Some(event)) if event.is_terminal() => return Ok(()),
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => return Ok(()),
            Err(error) => return Err(error),
            Ok(_) => {}
        }
        if Instant::now() >= deadline {
            fork::signal_process(child, Signal::KILL)?;
            fork::wait_event(child)?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "fixture child did not finish",
            ));
        }
        thread::sleep(POLL);
    }
}

/// Scope a global disposition change to this standalone scenario.
struct Action {
    signal: libc::c_int,
    previous: libc::sigaction,
}
impl Drop for Action {
    fn drop(&mut self) {
        // SAFETY: saved action is initialized and no other thread changes it.
        unsafe {
            libc::sigaction(self.signal, &raw const self.previous, ptr::null_mut());
        }
    }
}
fn install_auto_policy(mode: &str) -> io::Result<Action> {
    if mode.ends_with("ignore") {
        install(libc::SIGCHLD, libc::SIG_IGN, 0)
    } else {
        install(libc::SIGCHLD, libc::SIG_DFL, libc::SA_NOCLDWAIT)
    }
}
fn install(
    signal: libc::c_int,
    handler: libc::sighandler_t,
    flags: libc::c_int,
) -> io::Result<Action> {
    // SAFETY: signal structures are initialized before use below.
    let mut action: libc::sigaction = unsafe { mem::zeroed() };
    // SAFETY: sigaction fills this output on success.
    let mut previous: libc::sigaction = unsafe { mem::zeroed() };
    action.sa_sigaction = handler;
    action.sa_flags = flags;
    // SAFETY: valid signal, handler/default, and live initialized mask/output.
    unsafe {
        libc::sigemptyset(&raw mut action.sa_mask);
        if libc::sigaction(signal, &raw const action, &raw mut previous) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(Action { signal, previous })
}

/// Bound compiler and re-executed scenario commands, collecting the direct child.
fn run_command(command: &mut Command) -> io::Result<ExitStatus> {
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "cleanup contract command timed out",
            ));
        }
        thread::sleep(POLL);
    }
}

/// Signal-free success drain; exact ownership checks guard failed-scenario kills.
struct Children;
impl Drop for Children {
    fn drop(&mut self) {
        let deadline = Instant::now() + BOUND;
        loop {
            match fork::wait_any_event_nohang() {
                Err(_) => return,
                Ok(Some(_)) => continue,
                Ok(None) => {}
            }
            if let Ok(raw) =
                fs::read_to_string(format!("/proc/self/task/{}/children", std::process::id()))
            {
                for child in raw
                    .split_whitespace()
                    .filter_map(|s| s.parse::<i32>().ok())
                    .filter_map(ProcessId::new)
                {
                    if matches!(fork::wait_event_nohang(child), Ok(None)) {
                        let _ = fork::signal_process(child, Signal::KILL);
                    }
                }
            }
            if Instant::now() >= deadline {
                return;
            }
            thread::sleep(POLL);
        }
    }
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
