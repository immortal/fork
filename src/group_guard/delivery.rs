//! Signal request/reply framing on the guard's owner socket.
//!
//! The owner serializes exchanges and retains a partial reply across deadlines.
//! Monotonic request IDs distinguish even repeated signals; stale replies are
//! consumed and discarded. The helper signals only while owning its anchor.

use std::{
    io,
    os::fd::RawFd,
    sync::{Mutex, TryLockError},
    time::{Duration, Instant},
};

use crate::{ProcessGroupId, Signal};

use super::{MAX_POLL_INTERVAL, child_kill_group, wait_ready};

#[cfg(test)]
mod tests;

pub(super) const SIGNAL_RECORD: u8 = 0xa6;
const REQUEST_BYTES: usize = 9;
const REPLY_BYTES: usize = 12;
const PIN_LOST: &str = "process-group helper is gone; the pin may be lost and the group must not be signalled directly";

/// Owner-only, fixed-size framing state protected for the whole exchange.
/// IDs never wrap: exhaustion rejects further requests rather than accepting
/// an old acknowledgment. Partial bytes remain owned here until a full reply.
#[derive(Debug, Default)]
pub(super) struct Exchange {
    last_id: u32,
    reply: [u8; REPLY_BYTES],
    filled: usize,
}

impl Exchange {
    fn next_id(&mut self) -> io::Result<u32> {
        self.last_id = self.last_id.checked_add(1).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "guard signal request IDs exhausted",
            )
        })?;
        Ok(self.last_id)
    }
}

/// Serialize a bounded exchange without changing the guard's auto traits.
/// A timeout does not cancel a queued signal, but its reply cannot acknowledge
/// another request, even when that request asks for the same signal.
pub(super) fn signal_group(
    descriptor: RawFd,
    exchange: &Mutex<Exchange>,
    signal: Signal,
    deadline: Instant,
) -> io::Result<()> {
    let mut state = loop {
        match exchange.try_lock() {
            Ok(lock) => break lock,
            Err(TryLockError::Poisoned(_)) => {
                return Err(io::Error::other("guard signal exchange lock is poisoned"));
            }
            Err(TryLockError::WouldBlock) => pause(deadline)?,
        }
    };
    let id = state.next_id()?;
    let mut request = [0_u8; REQUEST_BYTES];
    request[0] = SIGNAL_RECORD;
    request[1..5].copy_from_slice(&id.to_ne_bytes());
    request[5..].copy_from_slice(&signal.get().to_ne_bytes());
    loop {
        wait_ready(descriptor, libc::POLLOUT, deadline)?;
        // SAFETY: request is readable for its length and descriptor is the
        // borrowed live socket. The owner endpoint is nonblocking, so even a
        // full buffer returns WouldBlock; MSG_DONTWAIT alone is not portable.
        let sent = unsafe {
            libc::send(
                descriptor,
                request.as_ptr().cast(),
                request.len(),
                libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
            )
        };
        if usize::try_from(sent) == Ok(REQUEST_BYTES) {
            break;
        }
        if sent >= 0 {
            // SAFETY: shut down this owned endpoint's write half on incomplete
            // framing. EOF makes the helper fail closed, never misread a record.
            unsafe { libc::shutdown(descriptor, libc::SHUT_WR) };
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, PIN_LOST));
        }
        let error = socket_error();
        if !matches!(
            error.kind(),
            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
        ) {
            return Err(error);
        }
    }
    receive_reply(descriptor, &mut state, id, signal, deadline)
}

/// Consume bytes, including EOF after a partial reply, without relying on
/// POLLHUP. A deadline preserves the prefix for the next serialized exchange.
fn receive_reply(
    descriptor: RawFd,
    state: &mut Exchange,
    id: u32,
    signal: Signal,
    deadline: Instant,
) -> io::Result<()> {
    loop {
        wait_ready(descriptor, libc::POLLIN, deadline)?;
        let remaining = state.reply.get_mut(state.filled..).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid guard reply offset")
        })?;
        // SAFETY: remaining is writable storage of its specified length.
        // This is a consuming nonblocking read, not a peek; EOF is always 0.
        let read = unsafe {
            libc::recv(
                descriptor,
                remaining.as_mut_ptr().cast(),
                remaining.len(),
                libc::MSG_DONTWAIT,
            )
        };
        match usize::try_from(read) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, PIN_LOST)),
            Ok(count) => state.filled += count,
            Err(_) => {
                let error = socket_error();
                if !matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) {
                    return Err(error);
                }
                continue;
            }
        }
        if state.filled == REPLY_BYTES {
            let [r0, r1, r2, r3, s0, s1, s2, s3, e0, e1, e2, e3] = state.reply;
            let (request, signal_bytes, error_bytes) =
                ([r0, r1, r2, r3], [s0, s1, s2, s3], [e0, e1, e2, e3]);
            state.filled = 0;
            if u32::from_ne_bytes(request) != id {
                continue;
            }
            if i32::from_ne_bytes(signal_bytes) != signal.get() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "guard reply signal mismatch",
                ));
            }
            let errno = i32::from_ne_bytes(error_bytes);
            return if errno == 0 {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(errno))
            };
        }
    }
}

fn pause(deadline: Instant) -> io::Result<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "timed out waiting for process-group helper",
        ));
    }
    std::thread::sleep(
        remaining
            .min(MAX_POLL_INTERVAL)
            .min(Duration::from_millis(1)),
    );
    Ok(())
}

fn socket_error() -> io::Error {
    let error = io::Error::last_os_error();
    if matches!(error.raw_os_error(), Some(libc::EPIPE | libc::ECONNRESET)) {
        io::Error::new(io::ErrorKind::BrokenPipe, PIN_LOST)
    } else {
        error
    }
}

/// Read one complete payload, failing closed on truncated records or errors.
/// This runs after fork: only syscalls and stack storage are used.
pub(super) fn child_signal(descriptor: RawFd, group: ProcessGroupId) {
    let mut record = [0_u8; 8];
    let mut filled = 0;
    while filled < record.len() {
        let Some(remaining) = record.get_mut(filled..) else {
            child_kill_group(group);
        };
        // SAFETY: remaining is writable storage and descriptor is the helper socket.
        let read =
            unsafe { libc::read(descriptor, remaining.as_mut_ptr().cast(), remaining.len()) };
        if read > 0 {
            filled += read.cast_unsigned();
        } else if !(read == -1 && crate::raw::last_errno() == libc::EINTR) {
            child_kill_group(group);
        }
    }
    let [_, _, _, _, a, b, c, d] = record;
    let signal = i32::from_ne_bytes([a, b, c, d]);
    if signal <= 0 {
        child_kill_group(group);
    }
    let errno = loop {
        // SAFETY: the helper still owns the unreaped anchor, pinning exactly
        // this positive group ID. No owner-side liveness assumption is needed.
        if unsafe { libc::kill(-group.get(), signal) } == 0 {
            break 0_i32;
        }
        let errno = crate::raw::last_errno();
        if errno != libc::EINTR {
            break errno;
        }
    };
    let mut reply = [0_u8; REPLY_BYTES];
    reply[..8].copy_from_slice(&record);
    reply[8..].copy_from_slice(&errno.to_ne_bytes());
    let mut sent = 0;
    while sent < reply.len() {
        let Some(remaining) = reply.get(sent..) else {
            return;
        };
        // SAFETY: remaining is readable stack storage, and MSG_NOSIGNAL keeps
        // a lost owner from killing the helper before its owner-loss cleanup.
        let written = unsafe {
            libc::send(
                descriptor,
                remaining.as_ptr().cast(),
                remaining.len(),
                libc::MSG_NOSIGNAL,
            )
        };
        if written > 0 {
            sent += written.cast_unsigned();
        } else if !(written == -1 && crate::raw::last_errno() == libc::EINTR) {
            // The next read observes owner loss and kills the pinned group.
            return;
        }
    }
}
