//! Preserve successful creation and consumed terminal events without waitid.

use std::{
    error::Error,
    io, thread,
    time::{Duration, Instant},
};

use fork::{CheckedDaemon, DaemonOptions, ProcessGroupGuard};

use super::{BOUND, POLL};

pub(super) fn run(mode: &str) -> Result<(), Box<dyn Error>> {
    if mode == "waitid-guard" {
        // The shim makes the helper exit between the ordinary idle wait and
        // cleanup's CONT ownership observation. Fallback consumes exit 0: it
        // must return that event to disarm rather than lose it and get ECHILD.
        return ProcessGroupGuard::new()?.disarm(BOUND).map_err(Into::into);
    }
    let result = match fork::checked_daemon(DaemonOptions::new(), BOUND) {
        Ok(CheckedDaemon::Daemon(notifier)) => {
            let ready = notifier.notify_ready().is_ok();
            // Keep the failed-intermediate group occupied beyond cleanup so
            // its observation-only obligation is visible to the invoker.
            if mode == "waitid-failure" {
                thread::sleep(Duration::from_secs(4));
            }
            // SAFETY: exit this finite detached fixture without owner destructors.
            unsafe { libc::_exit(i32::from(!ready)) }
        }
        other => other,
    };
    let check = if mode == "waitid-failure" {
        match result {
            Err(fork::DaemonError::InvalidHandshake { cleanup_pending })
                if cleanup_pending.process_group().is_some()
                    && cleanup_pending.intermediate().is_none() =>
            {
                Ok(())
            }
            other => Err(io::Error::other(format!(
                "missing failed-intermediate cleanup obligation: {other:?}"
            ))),
        }
    } else {
        match result {
            Ok(CheckedDaemon::Parent(_)) => Ok(()),
            other => Err(io::Error::other(format!(
                "denied waitid broke successful daemon: {other:?}"
            ))),
        }
    };
    let deadline = Instant::now() + BOUND;
    loop {
        match fork::wait_any_event_nohang() {
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => break,
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        if Instant::now() >= deadline {
            return Err("daemon descendants did not finish".into());
        }
        thread::sleep(POLL);
    }
    check.map_err(Into::into)
}
