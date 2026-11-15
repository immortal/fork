//! Unit tests for the guard helper's owner-side state and startup record.

use super::{
    GroupErrno, ProcessGroupGuard, child_send_record, classify_group_errno, decode_group_record,
    failure_record,
};

/// A stopped helper can fill the request queue. Disarm must return an
/// error rather than block before the owner-loss cleanup can resume it.
#[test]
fn disarm_full_socket_does_not_block() -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let (owner, _helper) = crate::socket_pair_cloexec()?.into_parts();
    super::set_nonblocking(&owner)?;
    let record = 0_u8;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        // SAFETY: owner is live; record is one initialized byte. This
        // fills only a test-owned socket, using nonblocking writes.
        let sent = unsafe {
            libc::send(
                owner.as_raw_fd(),
                (&raw const record).cast(),
                1,
                libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
            )
        };
        if sent < 0 {
            assert_eq!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::WouldBlock
            );
            break;
        }
        if std::time::Instant::now() >= deadline {
            return Err(std::io::Error::other("socket did not fill"));
        }
    }
    let result = super::write_disarm(owner.as_raw_fd());
    assert!(matches!(result, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    Ok(())
}

/// Disarm reaps the helper and then drops it. The drop must not repeat
/// cleanup against the reaped PID, which may by then name a newer child:
/// stand in for that reuse with a live child and require it untouched.
#[test]
fn dropped_helper_does_not_repeat_cleanup_after_disarm() -> std::io::Result<()> {
    use std::time::Duration;
    let mut helper = super::GuardHelper::spawn()?;
    helper.read_group(Duration::from_secs(3))?;
    helper.disarm_and_reap(Duration::from_secs(3))?;
    assert!(helper.retired);

    let bystander = match crate::fork_process()? {
        crate::ProcessFork::Parent(process) => process,
        crate::ProcessFork::Child => loop {
            // SAFETY: pause is async-signal-safe; the test kills this child.
            unsafe { libc::pause() };
        },
    };
    helper.process = bystander;
    drop(helper);
    let observed = crate::wait_event_nohang(bystander);
    // Always clean up before asserting; a repeated cleanup already reaped it.
    if matches!(observed, Ok(None)) {
        crate::signal_process(bystander, crate::Signal::KILL)?;
        crate::wait_event(bystander)?;
    }
    assert!(
        matches!(observed, Ok(None)),
        "repeated cleanup touched a reused helper PID: {observed:?}"
    );
    Ok(())
}

#[test]
fn guard_preserves_auto_traits() {
    fn require<T: Send + Sync + Unpin + std::panic::UnwindSafe + std::panic::RefUnwindSafe>() {}
    require::<ProcessGroupGuard>();
}

#[test]
fn signal_rejects_a_taken_owner() -> std::io::Result<()> {
    let mut guard = ProcessGroupGuard::new()?;
    if let Some(helper) = guard.guard.as_mut() {
        drop(helper.owner.take());
    }
    let result = guard.signal_group(crate::Signal::KILL, std::time::Duration::from_secs(1));
    assert!(matches!(result, Err(error) if error.kind() == std::io::ErrorKind::InvalidInput));
    Ok(())
}

/// The startup record carries a positive group or a negated errno, so the
/// owner reports why the helper could not reserve a group.
#[test]
fn group_record_reports_helper_errno() {
    assert!(matches!(decode_group_record(4242), Ok(group) if group.get() == 4242));
    let denied = decode_group_record(failure_record(libc::EACCES));
    assert!(matches!(denied, Err(error) if error.raw_os_error() == Some(libc::EACCES)));
    for malformed in [0, -1, libc::c_int::MIN] {
        assert_eq!(failure_record(malformed), -libc::EIO);
    }
    assert_eq!(failure_record(0), -libc::EIO);
    let overflow = decode_group_record(libc::pid_t::MIN);
    assert!(matches!(overflow, Err(error) if error.raw_os_error() == Some(libc::EIO)));
    assert!(
        matches!(decode_group_record(0), Err(error) if error.kind() == std::io::ErrorKind::InvalidData)
    );
}

/// The helper's failure record crosses the socket intact.
#[test]
fn helper_failure_record_crosses_the_socket() -> std::io::Result<()> {
    use std::{io::Read, os::fd::AsRawFd};
    let (owner, helper) = crate::socket_pair_cloexec()?.into_parts();
    child_send_record(helper.as_raw_fd(), failure_record(libc::ENOENT));
    let mut record = [0_u8; 4];
    std::fs::File::from(owner).read_exact(&mut record)?;
    let decoded = decode_group_record(libc::pid_t::from_ne_bytes(record));
    assert!(matches!(decoded, Err(error) if error.raw_os_error() == Some(libc::ENOENT)));
    Ok(())
}

/// The pin and cleanup's resume need default SIGCHLD and SIGCONT, and a
/// faulting helper must still die; every other signal is ignored so a
/// signal to the owner's group cannot end the helper.
#[test]
fn helper_ignores_everything_but_its_pin_resume_and_faults() {
    use super::helper_disposition;
    for signal in [
        libc::SIGCHLD,
        libc::SIGCONT,
        libc::SIGSEGV,
        libc::SIGBUS,
        libc::SIGSYS,
    ] {
        assert_eq!(helper_disposition(signal), libc::SIG_DFL);
    }
    for signal in [
        libc::SIGHUP,
        libc::SIGINT,
        libc::SIGQUIT,
        libc::SIGTERM,
        libc::SIGPIPE,
        libc::SIGUSR1,
        libc::SIGUSR2,
        libc::SIGTSTP,
    ] {
        assert_eq!(helper_disposition(signal), libc::SIG_IGN);
    }
}

/// `EPERM` keeps the anchor's group reserved, because macOS answers it for
/// a group whose only member is the unreaped anchor; only `EINTR` retries,
/// and every other answer means no group is reserved.
#[test]
fn group_errno_treats_eperm_as_a_reserved_group() {
    assert_eq!(classify_group_errno(Some(libc::EPERM)), GroupErrno::Present);
    assert_eq!(
        classify_group_errno(Some(libc::EINTR)),
        GroupErrno::Interrupted
    );
    for failed in [Some(libc::ESRCH), Some(libc::EINVAL), None] {
        assert_eq!(classify_group_errno(failed), GroupErrno::Failed);
    }
}

/// Only `ECHILD` proves the helper's PID is no longer this owner's child;
/// every other failed wait consumed nothing and must leave cleanup retryable.
#[test]
fn only_echild_marks_the_helper_gone() {
    use super::{HelperWaitFailure, wait_failure};
    assert!(matches!(
        wait_failure(std::io::Error::from_raw_os_error(libc::ECHILD)),
        HelperWaitFailure::Reaped(_)
    ));
    for errno in [libc::EPERM, libc::EINVAL, libc::ENOSYS, libc::EINTR] {
        assert!(matches!(
            wait_failure(std::io::Error::from_raw_os_error(errno)),
            HelperWaitFailure::Unobserved(_)
        ));
    }
}

/// `EINVAL` may mean "no such signal" only where that is possible: never for
/// a standard signal, and never inside the target's known realtime range.
#[test]
fn known_signals_cover_the_standard_and_realtime_ranges() {
    use super::KnownSignals;
    let known = KnownSignals::prepare();
    for signal in [libc::SIGHUP, libc::SIGUSR1, libc::SIGTERM, 31] {
        assert!(known.must_exist(signal));
    }
    assert!(known.scan_end() >= size_of::<libc::sigset_t>() * 8);
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        assert!(known.scan_end() >= usize::try_from(libc::SIGRTMAX()).unwrap_or(usize::MAX));
        assert!(known.must_exist(libc::SIGRTMIN()));
        assert!(known.must_exist(libc::SIGRTMAX()));
        // The C library reserves the first realtime numbers for itself.
        assert!(!known.must_exist(32));
        assert!(!known.must_exist(libc::SIGRTMAX() + 1));
    }
    // FreeBSD accepts every number up to 128 except 32, which libthr
    // reserves; its named realtime range is 65 to 126. The libc crate exports
    // no constants for them.
    #[cfg(target_os = "freebsd")]
    {
        for signal in [33, 65, 126, 128] {
            assert!(known.must_exist(signal));
        }
        assert!(!known.must_exist(32));
        assert!(!known.must_exist(129));
    }
    #[cfg(target_vendor = "apple")]
    assert!(!known.must_exist(32));
}
