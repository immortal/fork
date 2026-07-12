use std::io;

use crate::raw::last_errno;

const RESET_SIGNALS: &[libc::c_int] = &[
    libc::SIGHUP,
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGILL,
    libc::SIGABRT,
    libc::SIGFPE,
    libc::SIGBUS,
    libc::SIGSEGV,
    libc::SIGPIPE,
    libc::SIGALRM,
    libc::SIGTERM,
    libc::SIGUSR1,
    libc::SIGUSR2,
    libc::SIGCHLD,
    libc::SIGCONT,
    libc::SIGTSTP,
    libc::SIGTTIN,
    libc::SIGTTOU,
    libc::SIGWINCH,
];

/// Signal-mask and disposition behavior for a new child context.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum ChildSignalState {
    /// Clear the signal mask and restore portable catchable signals to default.
    #[default]
    Reset,
    /// Explicitly retain the caller's mask and dispositions.
    Inherit,
}

pub(crate) struct Prepared {
    default_action: libc::sigaction,
    empty_mask: libc::sigset_t,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PreparedChildSignalState")
    }
}

impl Prepared {
    pub(crate) fn new(state: ChildSignalState) -> io::Result<Option<Self>> {
        if state == ChildSignalState::Inherit {
            return Ok(None);
        }
        // SAFETY: both C structures are initialized below before use.
        let mut default_action: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: sigemptyset fully initializes this signal set on success.
        let mut empty_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        default_action.sa_sigaction = libc::SIG_DFL;
        default_action.sa_flags = 0;
        // SAFETY: both mask pointers refer to writable initialized storage.
        unsafe {
            if libc::sigemptyset(&raw mut default_action.sa_mask) == -1
                || libc::sigemptyset(&raw mut empty_mask) == -1
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Some(Self {
            default_action,
            empty_mask,
        }))
    }

    /// Apply the precomputed state using only reviewed signal syscalls.
    pub(crate) fn apply_in_child(&self) -> Result<(), libc::c_int> {
        loop {
            // SAFETY: empty_mask is initialized and the old mask is not requested.
            if unsafe {
                libc::sigprocmask(
                    libc::SIG_SETMASK,
                    &raw const self.empty_mask,
                    std::ptr::null_mut(),
                )
            } == 0
            {
                break;
            }
            let errno = last_errno();
            if errno != libc::EINTR {
                return Err(errno);
            }
        }

        for signal in RESET_SIGNALS {
            loop {
                // SAFETY: default_action is initialized; KILL and STOP are not
                // in RESET_SIGNALS, and no old action is requested.
                if unsafe {
                    libc::sigaction(
                        *signal,
                        &raw const self.default_action,
                        std::ptr::null_mut(),
                    )
                } == 0
                {
                    break;
                }
                let errno = last_errno();
                if errno == libc::EINTR {
                    continue;
                }
                if errno == libc::EINVAL {
                    break;
                }
                return Err(errno);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ChildSignalState, Prepared, RESET_SIGNALS};

    #[test]
    fn reset_list_excludes_uncatchable_signals() {
        assert!(!RESET_SIGNALS.contains(&libc::SIGKILL));
        assert!(!RESET_SIGNALS.contains(&libc::SIGSTOP));
    }

    #[test]
    fn explicit_inheritance_requires_no_child_operations() -> Result<(), Box<dyn std::error::Error>>
    {
        assert!(Prepared::new(ChildSignalState::Inherit)?.is_none());
        assert!(Prepared::new(ChildSignalState::Reset)?.is_some());
        Ok(())
    }
}
