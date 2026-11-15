//! `SIGPIPE` suppression for writes to a pipe whose reader may be gone.
//!
//! A pipe has no per-write signal suppression, unlike a socket's
//! `MSG_NOSIGNAL`. The signal is therefore blocked in the calling thread for
//! the write, and the one that write raised is consumed before the mask is
//! restored. A signal that was already pending is left for its owner. A write
//! raises its `SIGPIPE` for the calling thread, so other threads are
//! unaffected. If the mask cannot be read or changed, the write runs
//! unprotected, exactly as it did before this module existed.

/// Run `write`, turning the `SIGPIPE` of a lost reader into its `EPIPE` result.
///
/// `write` returns the errno of a failed write. Only `EPIPE` consumes a newly
/// pending `SIGPIPE`; every result is returned unchanged.
pub(crate) fn without_sigpipe(
    write: impl FnOnce() -> Result<(), libc::c_int>,
) -> Result<(), libc::c_int> {
    let Some(guard) = SigpipeBlock::enter() else {
        return write();
    };
    let result = write();
    guard.leave(result == Err(libc::EPIPE));
    result
}

/// `SIGPIPE` blocked in the calling thread, with the state needed to undo it.
struct SigpipeBlock {
    signal: libc::sigset_t,
    previous: libc::sigset_t,
    already_pending: bool,
}

impl SigpipeBlock {
    /// Block `SIGPIPE`; `None` if the thread's mask cannot be read or changed.
    fn enter() -> Option<Self> {
        // SAFETY: zeroed sets are valid storage, initialized by the calls below.
        let mut signal: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: as above; pthread_sigmask writes the previous mask on success.
        let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: both pointers refer to live writable signal sets.
        if unsafe { libc::sigemptyset(&raw mut signal) } != 0
            || unsafe { libc::sigaddset(&raw mut signal, libc::SIGPIPE) } != 0
        {
            return None;
        }
        let already_pending = is_pending()?;
        // SAFETY: initialized input set and writable output set; only the
        // calling thread's mask changes.
        if unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const signal, &raw mut previous) }
            != 0
        {
            return None;
        }
        Some(Self {
            signal,
            previous,
            already_pending,
        })
    }

    /// Consume the `SIGPIPE` a failed write raised, then restore the mask.
    fn leave(self, write_raised_sigpipe: bool) {
        if write_raised_sigpipe && !self.already_pending && is_pending() == Some(true) {
            let mut consumed: libc::c_int = 0;
            // The set holds only SIGPIPE, which is pending and blocked, so
            // sigwait returns at once; EINTR is retried.
            loop {
                // SAFETY: both pointers refer to live storage owned by this frame.
                let result = unsafe { libc::sigwait(&raw const self.signal, &raw mut consumed) };
                if result != libc::EINTR {
                    break;
                }
            }
        }
        // SAFETY: restores the mask saved by `enter` for this same thread.
        unsafe {
            libc::pthread_sigmask(
                libc::SIG_SETMASK,
                &raw const self.previous,
                std::ptr::null_mut(),
            );
        }
    }
}

/// Whether `SIGPIPE` is pending for the calling thread; `None` if unknown.
pub(crate) fn is_pending() -> Option<bool> {
    // SAFETY: zeroed set is valid storage; sigpending initializes it on success.
    let mut pending: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: pending is live writable storage.
    if unsafe { libc::sigpending(&raw mut pending) } != 0 {
        return None;
    }
    // SAFETY: pending was initialized by sigpending.
    match unsafe { libc::sigismember(&raw const pending, libc::SIGPIPE) } {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}
