//! Small platform-specific primitives used only on reviewed post-fork paths.

use std::os::fd::RawFd;

pub(crate) fn write_all(descriptor: RawFd, bytes: &[u8]) -> Result<(), libc::c_int> {
    let mut written = 0;
    while written < bytes.len() {
        let Some(remaining) = bytes.get(written..) else {
            return Err(libc::EIO);
        };
        // SAFETY: descriptor is live and remaining points to initialized bytes.
        let result = unsafe { libc::write(descriptor, remaining.as_ptr().cast(), remaining.len()) };
        if result > 0 {
            written += result.unsigned_abs();
        } else if result != -1 {
            return Err(libc::EIO);
        } else {
            let errno = last_errno();
            if errno != libc::EINTR {
                return Err(errno);
            }
        }
    }
    Ok(())
}

/// Send every byte on a socket without ever raising `SIGPIPE`.
///
/// `MSG_NOSIGNAL` suppresses the signal for this one call, so no signal mask
/// or disposition is read or changed and no other thread is affected. A peer
/// that is gone yields `EPIPE`. If `send` fails for any other reason before a
/// byte was accepted, because the descriptor is a pipe or a sandbox refused
/// the call with whatever errno, repeated `EINTR` included, the bytes are
/// written with plain `write`,
/// exactly as before this function existed: that write reports the real
/// error, and raises `SIGPIPE` if the reader is gone. Once a byte was sent,
/// nothing is written again.
pub(crate) fn send_all_nosignal(descriptor: RawFd, bytes: &[u8]) -> Result<(), libc::c_int> {
    send_all_or_fall_back(
        bytes,
        |remaining| {
            // SAFETY: descriptor is live and remaining points to initialized bytes.
            let result = unsafe {
                libc::send(
                    descriptor,
                    remaining.as_ptr().cast(),
                    remaining.len(),
                    libc::MSG_NOSIGNAL,
                )
            };
            match result {
                -1 => Err(last_errno()),
                count => usize::try_from(count).map_err(|_| libc::EIO),
            }
        },
        || write_all(descriptor, bytes),
    )
}

/// Interrupted first sends tolerated before `write` takes over.
const MAX_INTERRUPTED_FIRST_SENDS: u8 = 8;

/// Drive `send` until every byte is accepted, or hand the whole record to
/// `fall_back` when the first send cannot be used.
///
/// The fallback runs at most once and only while nothing was accepted, so a
/// record is never written twice. A first send that keeps being interrupted
/// is refused in effect, as a sandbox answering `EINTR` would be, and falls
/// back too; once a byte was accepted, an interrupted send is simply retried.
fn send_all_or_fall_back(
    bytes: &[u8],
    mut send: impl FnMut(&[u8]) -> Result<usize, libc::c_int>,
    fall_back: impl FnOnce() -> Result<(), libc::c_int>,
) -> Result<(), libc::c_int> {
    let mut accepted = 0;
    let mut interrupted = 0_u8;
    while accepted < bytes.len() {
        let Some(remaining) = bytes.get(accepted..) else {
            return Err(libc::EIO);
        };
        match send(remaining) {
            Ok(0) => return Err(libc::EIO),
            Ok(count) => accepted = accepted.saturating_add(count),
            Err(libc::EINTR) if accepted > 0 => {}
            Err(libc::EINTR) if interrupted < MAX_INTERRUPTED_FIRST_SENDS => interrupted += 1,
            Err(errno) if accepted == 0 && send_is_unavailable(errno) => return fall_back(),
            Err(errno) => return Err(errno),
        }
    }
    Ok(())
}

/// Whether a failed first `send` says nothing about the peer, so `write`
/// must decide. Only the errors that mean the peer is gone are final; a
/// sandbox may refuse the call with any other errno.
const fn send_is_unavailable(errno: libc::c_int) -> bool {
    !matches!(errno, libc::EPIPE | libc::ECONNRESET | libc::ENOTCONN)
}

#[cfg(any(target_os = "linux", target_os = "dragonfly"))]
pub(crate) fn last_errno() -> libc::c_int {
    // SAFETY: libc returns a thread-local errno pointer for the current thread.
    unsafe { *libc::__errno_location() }
}

#[cfg(target_os = "android")]
pub(crate) fn last_errno() -> libc::c_int {
    // SAFETY: libc returns a thread-local errno pointer for the current thread.
    unsafe { *libc::__errno() }
}

#[cfg(any(target_os = "netbsd", target_os = "openbsd"))]
pub(crate) fn last_errno() -> libc::c_int {
    // SAFETY: libc returns a thread-local errno pointer for the current thread.
    unsafe { *libc::__errno() }
}

#[cfg(any(target_os = "freebsd", target_vendor = "apple"))]
pub(crate) fn last_errno() -> libc::c_int {
    // SAFETY: libc returns a thread-local errno pointer for the current thread.
    unsafe { *libc::__error() }
}

#[cfg(all(
    not(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "android",
        target_os = "netbsd",
        target_os = "openbsd"
    )),
    not(any(target_os = "freebsd", target_vendor = "apple"))
))]
pub(crate) fn last_errno() -> libc::c_int {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::{MAX_INTERRUPTED_FIRST_SENDS, send_all_or_fall_back, send_is_unavailable};

    const RECORD: &[u8] = b"twelve bytes";

    /// A sandbox answering every first send with `EINTR` must reach the
    /// fallback after a bounded number of tries instead of spinning.
    #[test]
    fn persistently_interrupted_first_send_falls_back_once() {
        let attempts = Cell::new(0_u32);
        let fallbacks = Cell::new(0_u32);
        let result = send_all_or_fall_back(
            RECORD,
            |_| {
                attempts.set(attempts.get() + 1);
                Err(libc::EINTR)
            },
            || {
                fallbacks.set(fallbacks.get() + 1);
                Ok(())
            },
        );
        assert_eq!(result, Ok(()));
        assert_eq!(fallbacks.get(), 1);
        assert_eq!(attempts.get(), u32::from(MAX_INTERRUPTED_FIRST_SENDS) + 1);
    }

    /// Once a byte was accepted the record is never handed to the fallback:
    /// interruptions are retried on the suffix and a refusal is the error.
    #[test]
    fn a_partly_sent_record_is_never_written_again() {
        let calls = Cell::new(0_u32);
        let delivered = Cell::new(0_usize);
        let result = send_all_or_fall_back(
            RECORD,
            |remaining| {
                calls.set(calls.get() + 1);
                match calls.get() {
                    1 => {
                        delivered.set(3);
                        Ok(3)
                    }
                    2..=20 => Err(libc::EINTR),
                    _ => {
                        delivered.set(delivered.get() + remaining.len());
                        Ok(remaining.len())
                    }
                }
            },
            || Err(libc::EBADF),
        );
        assert_eq!(result, Ok(()));
        assert_eq!(delivered.get(), RECORD.len());

        let refused = send_all_or_fall_back(
            RECORD,
            {
                let first = Cell::new(true);
                move |_| {
                    if first.replace(false) {
                        Ok(3)
                    } else {
                        Err(libc::EPERM)
                    }
                }
            },
            || Ok(()),
        );
        assert_eq!(refused, Err(libc::EPERM));
    }

    /// A peer that is gone is final for a first send; a zero-length send is
    /// an error, not progress.
    #[test]
    fn peer_loss_and_empty_sends_do_not_fall_back() {
        assert_eq!(
            send_all_or_fall_back(RECORD, |_| Err(libc::EPIPE), || Ok(())),
            Err(libc::EPIPE)
        );
        assert_eq!(
            send_all_or_fall_back(RECORD, |_| Ok(0), || Ok(())),
            Err(libc::EIO)
        );
    }

    /// A refused `send` falls back to `write` whatever errno the sandbox
    /// chose; only a peer that is gone is a final answer.
    #[test]
    fn only_peer_loss_is_final_for_a_first_send() {
        for errno in [
            libc::ENOTSOCK,
            libc::ENOSYS,
            libc::EPERM,
            libc::EACCES,
            libc::EINVAL,
            libc::EIO,
        ] {
            assert!(send_is_unavailable(errno));
        }
        for errno in [libc::EPIPE, libc::ECONNRESET, libc::ENOTCONN] {
            assert!(!send_is_unavailable(errno));
        }
    }
}
