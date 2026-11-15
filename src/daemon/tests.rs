//! Unit tests for the daemon wire record, notifier and intermediate handling.

use super::{DaemonStage, WireRecord, daemon_stage_from_wire};

#[test]
fn wire_record_has_fixed_pipe_friendly_size() {
    assert_eq!(size_of::<WireRecord>(), 12);
}

#[test]
fn wire_stage_rejects_parent_only_first_fork() {
    assert_eq!(daemon_stage_from_wire(2), Some(DaemonStage::CreateSession));
    assert_eq!(daemon_stage_from_wire(6), Some(DaemonStage::Initialization));
    assert_eq!(daemon_stage_from_wire(8), Some(DaemonStage::SignalState));
    assert_eq!(daemon_stage_from_wire(1), None);
}

/// With SIGPIPE at its default action, as `ChildSignalState::Reset`
/// leaves it, a notifier whose invoker is gone must get `EPIPE` back and
/// keep its signal mask and pending set unchanged.
#[test]
fn notifier_reports_a_lost_invoker_without_sigpipe() -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let (reader, writer) = crate::pipe_cloexec()?.into_parts();
    // No reader may remain anywhere, or the child's write would succeed.
    drop(reader);
    let process = match crate::fork_process()? {
        crate::ProcessFork::Parent(process) => process,
        crate::ProcessFork::Child => {
            // SAFETY: restores the default action in this test child only.
            unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
            let record = WireRecord::new(super::RECORD_READY, DaemonStage::Initialization, 0);
            let result = super::write_record_without_sigpipe(writer.as_raw_fd(), record);
            let code = match (result, crate::sigpipe::is_pending()) {
                (Err(libc::EPIPE), Some(false)) => 0,
                (Err(libc::EPIPE), _) => 3,
                _ => 4,
            };
            // SAFETY: the test child must not run the harness's destructors.
            unsafe { libc::_exit(code) }
        }
    };
    drop(writer);
    let event = crate::wait_event(process)?;
    assert_eq!(
        event,
        crate::ChildEvent::Exited {
            pid: process,
            code: 0
        },
        "the notifier did not report EPIPE cleanly"
    );
    Ok(())
}

/// A denied collecting wait is the operating system's error, not a
/// malformed handshake; a lost or unexpected child still is one.
#[test]
fn collecting_wait_failure_is_an_operating_system_error() -> std::io::Result<()> {
    use super::{DaemonError, collected_intermediate};
    let pid = crate::ProcessId::new(42).ok_or_else(|| std::io::Error::other("pid"))?;
    assert!(collected_intermediate(Ok(Some(crate::ChildEvent::Exited { pid, code: 0 }))).is_ok());
    let denied = collected_intermediate(Err(std::io::Error::from_raw_os_error(libc::EPERM)));
    assert!(matches!(
        denied,
        Err(DaemonError::OperatingSystem {
            stage: DaemonStage::Handshake,
            error,
            ..
        }) if error.raw_os_error() == Some(libc::EPERM)
    ));
    for lost in [
        Err(std::io::Error::from_raw_os_error(libc::ECHILD)),
        Ok(None),
        Ok(Some(crate::ChildEvent::Exited { pid, code: 1 })),
    ] {
        assert!(matches!(
            collected_intermediate(lost),
            Err(DaemonError::InvalidHandshake { .. })
        ));
    }
    Ok(())
}

/// Error cleanup must retain the intermediate's PID/group reservation.
/// Where a sandbox denies `waitid`, the documented consuming fallback has
/// already collected the exit, and cleanup then finds no child.
#[test]
fn failed_intermediate_remains_unreaped_until_cleanup() -> std::io::Result<()> {
    use std::time::{Duration, Instant};
    let retaining = waitid_is_available();
    let process = match crate::fork_process()? {
        crate::ProcessFork::Parent(process) => process,
        crate::ProcessFork::Child => {
            // SAFETY: only async-signal-safe calls in this test child.
            unsafe {
                libc::setpgid(0, 0);
                libc::_exit(70);
            }
        }
    };
    let result = super::wait_for_intermediate(process, Instant::now(), Duration::from_secs(2));
    let observed = crate::wait_event_nohang(process);
    // Always clean before asserting, without signalling a reaped PID.
    if matches!(&observed, Ok(None)) || matches!(&observed, Ok(Some(event)) if !event.is_terminal())
    {
        crate::signal_process(process, crate::Signal::KILL)?;
        crate::wait_event(process)?;
    }
    assert!(matches!(
        result,
        Err(super::DaemonError::InvalidHandshake { .. })
    ));
    if retaining {
        assert!(
            matches!(
                observed,
                Ok(Some(crate::ChildEvent::Exited { code: 70, .. }))
            ),
            "the failed intermediate's reservation was released before cleanup: {observed:?}"
        );
    } else {
        assert!(
            matches!(&observed, Err(error) if error.raw_os_error() == Some(libc::ECHILD)),
            "the consuming fallback left the failed intermediate behind: {observed:?}"
        );
    }
    Ok(())
}

/// Whether `waitid` can observe children here. Without children it
/// answers `ECHILD`; a sandbox that denies it answers another errno.
fn waitid_is_available() -> bool {
    // SAFETY: zeroed siginfo is valid storage for waitid to fill.
    let mut information: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: a non-consuming, non-blocking query of any child.
    let result = unsafe {
        libc::waitid(
            libc::P_ALL,
            0,
            &raw mut information,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)
}
