//! A real seccomp denial must not break a previously successful daemon call.

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        io, thread,
        time::{Duration, Instant},
    };

    fork::acquire_subreaper()?;
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
            k: u32::try_from(libc::SYS_waitid)?,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ERRNO | u32::try_from(libc::EACCES)?,
        },
        libc::sock_filter {
            code: 0x06,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ];
    let program = libc::sock_fprog {
        len: u16::try_from(filter.len())?,
        filter: filter.as_mut_ptr(),
    };
    // SAFETY: standalone single-threaded fixture. The live BPF program denies
    // only waitid with EACCES, leaving the exact-child waitpid fallback usable.
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            || libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER,
                &raw const program,
            ) != 0
        {
            return Err(io::Error::last_os_error().into());
        }
    }
    let result = match fork::checked_daemon(fork::DaemonOptions::new(), Duration::from_secs(2)) {
        Ok(fork::CheckedDaemon::Daemon(notifier)) => {
            let ready = notifier.notify_ready().is_ok();
            // SAFETY: finite detached fixture, no inherited destructors.
            unsafe { libc::_exit(i32::from(!ready)) }
        }
        other => other,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut daemon_exited = false;
    loop {
        match fork::wait_any_event_nohang() {
            Ok(Some(fork::ChildEvent::Exited { code: 0, .. })) => daemon_exited = true,
            Ok(Some(event)) if event.is_terminal() => {
                return Err(format!("daemon failed: {event:?}").into());
            }
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => break,
            Err(error) => return Err(error.into()),
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err("daemon descendants remained".into());
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        matches!(result, Ok(fork::CheckedDaemon::Parent(_))),
        "{result:?}"
    );
    assert!(daemon_exited);
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {}
