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
/// that is gone yields `EPIPE`. If the descriptor is not a socket, or a
/// sandbox refuses `send` before any byte was accepted, the bytes are written
/// with plain `write`, exactly as before this function existed.
pub(crate) fn send_all_nosignal(descriptor: RawFd, bytes: &[u8]) -> Result<(), libc::c_int> {
    let mut sent = 0;
    while sent < bytes.len() {
        let Some(remaining) = bytes.get(sent..) else {
            return Err(libc::EIO);
        };
        // SAFETY: descriptor is live and remaining points to initialized bytes.
        let result = unsafe {
            libc::send(
                descriptor,
                remaining.as_ptr().cast(),
                remaining.len(),
                libc::MSG_NOSIGNAL,
            )
        };
        if result > 0 {
            sent += result.unsigned_abs();
        } else if result != -1 {
            return Err(libc::EIO);
        } else {
            match last_errno() {
                libc::EINTR => {}
                errno if sent == 0 && send_is_unavailable(errno) => {
                    return write_all(descriptor, bytes);
                }
                errno => return Err(errno),
            }
        }
    }
    Ok(())
}

/// Whether a failed `send` means the call itself cannot be used here: the
/// descriptor is a pipe, or a sandbox denied the system call with an errno.
const fn send_is_unavailable(errno: libc::c_int) -> bool {
    matches!(
        errno,
        libc::ENOTSOCK | libc::ENOSYS | libc::EPERM | libc::EACCES
    )
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
