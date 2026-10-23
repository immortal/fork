//! Serial, process-local lowered-rlimit descriptor isolation contracts.
//! Linux also disables `close_range` with seccomp; macOS uses the fallback natively.

use std::{
    error::Error,
    fs::File,
    io::{self, Read},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    time::Duration,
};

use fork::{ChildEvent, PreparedCommand, ProcessGroupGuard, Signal, signal_process, wait_event};

const TIMEOUT: Duration = Duration::from_secs(2);

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(target_os = "linux")]
    if std::env::args().any(|arg| arg == "--reject-real-ranges") {
        reject_real_ranges()?;
        return check_range_failure();
    }
    #[cfg(target_os = "linux")]
    if std::env::args().any(|arg| arg == "--deny-inventory") {
        // The high fd is below the hard limit but above the later soft limit.
        // No /dev/fd open is possible, and close_range is unavailable.
        let limit = libc::rlimit {
            rlim_cur: 256,
            rlim_max: 256,
        };
        // SAFETY: this isolated subprocess only lowers its own finite limit.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        deny_inventory()?;
        force_fallback()?;
        return check_descriptors();
    }
    check_descriptors()?;
    check_inventory_exhaustion()?;
    #[cfg(target_os = "linux")]
    {
        let status = std::process::Command::new(std::env::current_exe()?)
            .arg("--reject-real-ranges")
            .status()?;
        if !status.success() {
            return Err(io::Error::other("successful-probe/failed-range contract failed").into());
        }
        let status = std::process::Command::new(std::env::current_exe()?)
            .arg("--deny-inventory")
            .status()?;
        assert!(
            status.success(),
            "denied inventory must still close high descriptors"
        );
        force_fallback()?;
        check_descriptors()?;
        check_inventory_exhaustion()?;
    }
    Ok(())
}

/// Restore the process-local limit even on an early error.
struct Limit(libc::rlimit);
impl Limit {
    fn lower() -> io::Result<Self> {
        let mut old = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: old is writable storage for the process-local limit.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut old) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let lower = libc::rlimit {
            rlim_cur: 64,
            ..old
        };
        // SAFETY: lower retains the hard limit; only this standalone process changes.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const lower) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(old))
    }
}
impl Drop for Limit {
    fn drop(&mut self) {
        // SAFETY: the original soft and hard limits came from getrlimit above.
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const self.0) };
    }
}

fn high_pipe() -> io::Result<(File, OwnedFd)> {
    let (reader, writer) = fork::pipe_cloexec()?.into_parts();
    // SAFETY: the live writer is duplicated above the later lowered soft limit.
    // F_DUPFD deliberately leaves CLOEXEC unset to test prepared exec isolation.
    let raw = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_DUPFD, 128) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a newly owned descriptor.
    let high = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: reader is owned and this test needs bounded, nonblocking reads.
    if unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((File::from(reader), high))
}

fn check_descriptors() -> Result<(), Box<dyn Error>> {
    let (mut reader, high) = high_pipe()?;
    let limit = Limit::lower()?;
    let guard = ProcessGroupGuard::new()?;
    drop(high);
    let guard_result = reader.read(&mut [0]);
    guard.disarm(TIMEOUT)?;
    drop(limit);

    let (mut reader, high) = high_pipe()?;
    let limit = Limit::lower()?;
    let mut command = PreparedCommand::new("/bin/sleep")?;
    command.arg("30")?;
    let child = command.spawn(TIMEOUT)?;
    drop(high);
    let result = reader.read(&mut [0]);
    // Always clean up before checking the contract, including on the old code.
    signal_process(child.process(), Signal::KILL)?;
    let event = wait_event(child.process())?;
    drop(limit);
    if !matches!(
        event,
        ChildEvent::Signalled {
            signal: Signal::KILL,
            ..
        }
    ) {
        return Err(io::Error::other(format!("unexpected child event: {event:?}")).into());
    }
    if !matches!(guard_result, Ok(0)) || !matches!(result, Ok(0)) {
        return Err(io::Error::other(format!(
            "high descriptor isolation: guard={guard_result:?}, prepared child={result:?}"
        ))
        .into());
    }
    Ok(())
}

/// Exhaust low descriptors, then leave only enough for pre-inventory setup.
fn fill_descriptors(spare: usize) -> io::Result<Vec<File>> {
    let mut fillers = Vec::new();
    loop {
        match File::open("/dev/null") {
            Ok(file) => fillers.push(file),
            Err(error) if error.raw_os_error() == Some(libc::EMFILE) => break,
            Err(error) => return Err(error),
        }
    }
    for _ in 0..spare {
        drop(fillers.pop());
    }
    Ok(fillers)
}

/// EMFILE cannot open an inventory: descriptor-free probing must still permit
/// preparation, without silently using only the lowered soft limit.
fn check_inventory_exhaustion() -> Result<(), Box<dyn Error>> {
    let (mut reader, high) = high_pipe()?;
    let limit = Limit::lower()?;
    let fillers = fill_descriptors(2)?; // exactly the guard socketpair
    let guard_rejected = match ProcessGroupGuard::new() {
        Ok(guard) => {
            guard.disarm(TIMEOUT)?;
            false
        }
        Err(error) if error.raw_os_error() == Some(libc::EMFILE) => true,
        Err(error) => return Err(error.into()),
    };
    drop(high);
    let guard_eof = reader.read(&mut [0]);
    drop(fillers);
    drop(limit);

    let (mut reader, high) = high_pipe()?;
    let mut command = PreparedCommand::new("/bin/sleep")?;
    command.arg("30")?.map_descriptor(high, 3)?;
    let limit = Limit::lower()?;
    // The status pipe needs two slots and its duplicated writer a third.
    // Mapping the high source then fills the freed original status slot;
    // dropping the source above the soft limit does not free a low slot.
    let fillers = fill_descriptors(3)?;
    let child_rejected = match command.spawn(TIMEOUT) {
        Ok(child) => {
            signal_process(child.process(), Signal::KILL)?;
            wait_event(child.process())?;
            false
        }
        Err(fork::SpawnError::OperatingSystem {
            stage: fork::SpawnStage::DescriptorDuplication,
            error,
            cleanup_pending: None,
        }) if error.raw_os_error() == Some(libc::EMFILE) => true,
        Err(error) => return Err(error.into()),
    };
    let child_eof = reader.read(&mut [0]);
    drop(fillers);
    drop(limit);
    assert!(matches!(guard_eof, Ok(0)) && matches!(child_eof, Ok(0)));
    assert!(
        matches!(fork::wait_any_event_nohang(), Err(error) if error.raw_os_error() == Some(libc::ECHILD))
    );
    if guard_rejected || child_rejected {
        return Err(io::Error::other(format!(
            "descriptor-free fallback failed: guard rejected={guard_rejected}, prepared spawn rejected={child_rejected}"
        )).into());
    }
    Ok(())
}

/// Model a sandbox without directory access, rather than descriptor exhaustion.
#[cfg(target_os = "linux")]
fn deny_inventory() -> io::Result<()> {
    let mut filter = [
        libc::sock_filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 0,
        },
        libc::sock_filter {
            code: 0x15,
            jt: 0,
            jf: 3,
            k: u32::try_from(libc::SYS_openat).map_err(io::Error::other)?,
        },
        libc::sock_filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: if cfg!(target_endian = "little") {
                32
            } else {
                36
            },
        },
        libc::sock_filter {
            code: 0x45,
            jt: 0,
            jf: 1,
            k: libc::O_DIRECTORY.cast_unsigned(),
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ERRNO | libc::EACCES.cast_unsigned(),
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ];
    let program = libc::sock_fprog {
        len: 6,
        filter: filter.as_mut_ptr(),
    };
    // SAFETY: live BPF storage; only this standalone subprocess is restricted.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0
        || unsafe {
            libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER,
                &raw const program,
            )
        } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn force_fallback() -> io::Result<()> {
    let mut filter = [
        libc::sock_filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 0,
        },
        libc::sock_filter {
            code: 0x15,
            jt: 0,
            jf: 1,
            k: u32::try_from(libc::SYS_close_range).map_err(io::Error::other)?,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ERRNO | libc::ENOSYS.cast_unsigned(),
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ];
    let program = libc::sock_fprog {
        len: 4,
        filter: filter.as_mut_ptr(),
    };
    // SAFETY: this single-threaded standalone test restricts only itself and
    // its children. The BPF program is live and permits all but close_range.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0
        // SAFETY: program points to the initialized filter for this syscall.
        || unsafe { libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &raw const program) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// No incomplete soft-limit fallback may turn a failed range close into success.
#[cfg(target_os = "linux")]
fn check_range_failure() -> Result<(), Box<dyn Error>> {
    let (mut reader, high) = high_pipe()?;
    let limit = Limit::lower()?;
    let guard_rejected = match ProcessGroupGuard::new() {
        Ok(guard) => {
            guard.disarm(TIMEOUT)?;
            false
        }
        // The helper reports the failed real close's errno, as the prepared
        // command and checked daemon do.
        Err(error) if error.raw_os_error() == Some(libc::ENOSYS) => true,
        Err(error) => return Err(error.into()),
    };
    drop(high);
    let guard_eof = reader.read(&mut [0]);
    drop(limit);

    let (mut reader, high) = high_pipe()?;
    let limit = Limit::lower()?;
    let mut command = PreparedCommand::new("/bin/sleep")?;
    command.arg("30")?;
    let child_rejected = match command.spawn(TIMEOUT) {
        Ok(child) => {
            signal_process(child.process(), Signal::KILL)?;
            wait_event(child.process())?;
            false
        }
        Err(fork::SpawnError::OperatingSystem {
            stage: fork::SpawnStage::DescriptorDuplication,
            error,
            cleanup_pending: None,
        }) if error.raw_os_error() == Some(libc::ENOSYS) => true,
        Err(error) => return Err(error.into()),
    };
    drop(high);
    let child_eof = reader.read(&mut [0]);
    drop(limit);
    assert!(matches!(guard_eof, Ok(0)) && matches!(child_eof, Ok(0)));
    assert!(
        matches!(fork::wait_any_event_nohang(), Err(error) if error.raw_os_error() == Some(libc::ECHILD))
    );
    let daemon_rejected = daemon_rejects_range_failure()?;
    assert!(
        guard_rejected && child_rejected && daemon_rejected,
        "real close_range failure was accepted: guard={guard_rejected}, child={child_rejected}, daemon={daemon_rejected}"
    );
    Ok(())
}

/// An argument-aware sandbox can permit the harmless probe but deny real closes.
#[cfg(target_os = "linux")]
fn reject_real_ranges() -> io::Result<()> {
    let mut filter = [
        libc::sock_filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: 0,
        },
        libc::sock_filter {
            code: 0x15,
            jt: 0,
            jf: 3,
            k: u32::try_from(libc::SYS_close_range).map_err(io::Error::other)?,
        },
        // Offset 16 is the low word of seccomp_data.args[0] on the CI targets.
        libc::sock_filter {
            code: 0x20,
            jt: 0,
            jf: 0,
            k: if cfg!(target_endian = "little") {
                16
            } else {
                20
            },
        },
        libc::sock_filter {
            code: 0x15,
            jt: 1,
            jf: 0,
            k: u32::MAX,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ERRNO | libc::ENOSYS.cast_unsigned(),
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ];
    let program = libc::sock_fprog {
        len: 6,
        filter: filter.as_mut_ptr(),
    };
    // SAFETY: the standalone child installs only a process-local filter. The
    // program is initialized and live; all syscalls except real ranges pass.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0
        // SAFETY: program points to the complete BPF array during this call.
        || unsafe { libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &raw const program) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Checked daemonization shares the same isolation plan, including its failure.
#[cfg(target_os = "linux")]
fn daemon_rejects_range_failure() -> Result<bool, Box<dyn Error>> {
    match fork::checked_daemon(fork::DaemonOptions::new(), TIMEOUT) {
        Err(fork::DaemonError::OperatingSystem {
            stage: fork::DaemonStage::Descriptors,
            error,
            cleanup_pending,
        }) if error.raw_os_error() == Some(libc::ENOSYS) && cleanup_pending.is_empty() => Ok(true),
        Ok(fork::CheckedDaemon::Daemon(notifier)) => {
            let _ = notifier.notify_ready();
            // SAFETY: a regressed plan returned in the detached fixture child.
            unsafe { libc::_exit(0) }
        }
        Ok(fork::CheckedDaemon::Parent(process)) => {
            // The fixture exits immediately, but its ID is no longer ours to
            // signal. Await init reaping it before reporting the regression.
            let deadline = std::time::Instant::now() + TIMEOUT;
            while fork::process_group_exists(process.process_group())? {
                if std::time::Instant::now() >= deadline {
                    return Err("daemon fixture did not exit".into());
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}
