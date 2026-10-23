//! GNU/Linux deterministic pre-exit anchor stop, with self-contained preload.
//! Static musl executables cannot interpose `setpgid` through `LD_PRELOAD`; this
//! fixture is intentionally a no-op there and is covered by the GNU run.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod linux {
    use std::{
        error::Error,
        fs, io,
        path::PathBuf,
        process::Command,
        thread,
        time::{Duration, Instant},
    };

    use fork::{ProcessGroupGuard, ProcessId, Signal, signal_process, wait_event_nohang};

    const SHIM: &str = r"
#define _GNU_SOURCE
#include <sys/syscall.h>
#include <unistd.h>
#include <signal.h>
int setpgid(pid_t pid, pid_t pgid) {
    int result = syscall(SYS_setpgid, pid, pgid);
    if (result == 0 && pid == 0 && pgid == 0)
        syscall(SYS_kill, syscall(SYS_getpid), SIGSTOP);
    return result;
}
";

    struct Temporary(PathBuf);
    impl Drop for Temporary {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// On the broken implementation the anchor is adopted here after helper
    /// timeout, so cleanup still kills and reaps the stopped orphan.
    struct Anchor(ProcessId);
    impl Drop for Anchor {
        fn drop(&mut self) {
            let owned = match wait_event_nohang(self.0) {
                Ok(None) => true,
                Ok(Some(event)) => !event.is_terminal(),
                Err(_) => false,
            };
            if owned {
                let _ = signal_process(self.0, Signal::KILL);
                let deadline = Instant::now() + Duration::from_secs(3);
                while Instant::now() < deadline {
                    match wait_event_nohang(self.0) {
                        Ok(Some(event)) if event.is_terminal() => break,
                        Err(_) => break,
                        _ => thread::sleep(Duration::from_millis(2)),
                    }
                }
            }
            let _ = fork::release_subreaper();
        }
    }

    pub(super) fn run() -> Result<(), Box<dyn Error>> {
        if std::env::args().any(|arg| arg == "--child") {
            return stopped_anchor();
        }
        let directory =
            Temporary(std::env::temp_dir().join(format!("fork-anchor-{}", std::process::id())));
        fs::create_dir(&directory.0)?;
        let source = directory.0.join("stop.c");
        let library = directory.0.join("stop.so");
        fs::write(&source, SHIM)?;
        let status = Command::new("cc")
            .args(["-Wall", "-Wextra", "-shared", "-fPIC"])
            .arg(&source)
            .arg("-o")
            .arg(&library)
            .status()?;
        if !status.success() {
            return Err(io::Error::other("cannot compile anchor shim").into());
        }
        let status = Command::new(std::env::current_exe()?)
            .arg("--child")
            .env("LD_PRELOAD", &library)
            .status()?;
        if !status.success() {
            return Err(io::Error::other("stopped anchor regression failed").into());
        }
        Ok(())
    }

    fn stopped_anchor() -> Result<(), Box<dyn Error>> {
        fork::acquire_subreaper()?;
        let guard = ProcessGroupGuard::new()?;
        let anchor = Anchor(ProcessId::try_from(guard.process_group().get())?);
        let path = format!("/proc/{}/status", anchor.0);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let status = fs::read_to_string(&path)?;
            if status
                .lines()
                .any(|line| line.starts_with("State:") && line.contains('T'))
            {
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other("anchor did not stop").into());
            }
            thread::sleep(Duration::from_millis(2));
        }
        let result = guard.disarm(Duration::from_millis(500));
        let remains = fs::metadata(path).is_ok();
        drop(anchor);
        result?;
        if remains {
            return Err(io::Error::other("disarm leaked stopped anchor").into());
        }
        Ok(())
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    linux::run()
}

/// Static and non-Linux targets cannot interpose `setpgid`; the GNU/Linux run
/// covers the stopped-anchor schedule.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn main() {}
