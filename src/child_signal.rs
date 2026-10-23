//! Child signal reset and explicit owner-side waitable-child policy.
//!
//! Checked daemon creation and numeric-PID cleanup query the owner's policy. Applications may opt into
//! correcting inherited auto-reaping at their single-threaded startup boundary;
//! neither observation nor correction serializes later policy changes or reaps.

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

/// Numeric child-PID cleanup requires the owner to retain exited children.
/// This does not change process-global policy. Callers must serialize policy
/// changes and child collection throughout each checked operation/lifetime.
pub(crate) fn require_waitable_children() -> io::Result<()> {
    if !children_are_waitable()? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "checked child ownership requires SIGCHLD not ignored and SA_NOCLDWAIT unset",
        ));
    }
    Ok(())
}

/// Whether the calling process's exited children remain waitable.
///
/// Reads `SIGCHLD` without changing it: explicit `SIG_IGN` or `SA_NOCLDWAIT`
/// means false. This is only a snapshot, not child ownership or a lock; callers
/// must serialize policy changes and collection throughout checked operations.
///
/// # Errors
///
/// Returns an OS error querying `sigaction`; interrupted queries are retried.
pub fn children_are_waitable() -> io::Result<bool> {
    query_sigchld().map(|action| action_is_waitable(&action))
}

/// Explicitly correct auto-reaping owner policy before creating children.
///
/// If `SIGCHLD` is ignored or has `SA_NOCLDWAIT`, installs `SIG_DFL` with an
/// empty handler mask and no flags, returning true. Otherwise returns false
/// without changing anything, including an installed handler. An ignored
/// `SIGCHLD` survives `exec`; checked daemon creation and numeric-PID cleanup
/// require a waitable owner. Guard creation and prepared spawn still accept it.
///
/// This changes process-wide state. Call only while single-threaded, before
/// children or handlers depending on that state exist. Afterwards keep children
/// waitable and serialize policy changes and collection with checked operations.
/// This does not recover already auto-reaped children or alter the signal mask.
///
/// # Errors
///
/// Returns an OS error querying or installing the disposition or initializing
/// its empty mask. Interrupted `sigaction` calls are retried.
pub fn ensure_waitable_children() -> io::Result<bool> {
    if children_are_waitable()? {
        return Ok(false);
    }
    // SAFETY: all fields are initialized before this action is installed.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = libc::SIG_DFL;
    // SAFETY: the pointer refers to writable mask storage.
    if unsafe { libc::sigemptyset(&raw mut action.sa_mask) } != 0 {
        return Err(io::Error::last_os_error());
    }
    loop {
        // SAFETY: installs only the default disposition, never a Rust handler.
        if unsafe { libc::sigaction(libc::SIGCHLD, &raw const action, std::ptr::null_mut()) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Signal only while policy and an exact-child observation prove a child pin.
/// Return any terminal event consumed by the waitid-unavailable fallback instead
/// of signalling. Callers must handle it, not wait again or lose its exit status.
/// Serialize policy changes and collection through delivery; errors grant no
/// numeric-PID fallback or unbounded wait authority.
pub(crate) fn signal_owned_child(
    process: crate::ProcessId,
    signal: crate::Signal,
) -> io::Result<Option<crate::ChildEvent>> {
    require_waitable_children()?;
    match crate::wait::observe_exit_nohang(process)? {
        crate::wait::ExitObservation::Reaped(event) => Ok(Some(event)),
        crate::wait::ExitObservation::Pinned(_) => {
            crate::signal_process(process, signal)?;
            Ok(None)
        }
    }
}

/// Query without modifying any process-wide state.
fn query_sigchld() -> io::Result<libc::sigaction> {
    // SAFETY: sigaction fills this output on success; no input action is supplied.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: this only reads the caller's SIGCHLD disposition into live storage.
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &raw mut action) } == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(action)
}

/// The shared classifier: default SIGCHLD is not explicit ignoring.
fn action_is_waitable(action: &libc::sigaction) -> bool {
    action.sa_sigaction != libc::SIG_IGN && action.sa_flags & libc::SA_NOCLDWAIT == 0
}

#[cfg(test)]
mod tests {
    use super::{ChildSignalState, Prepared, RESET_SIGNALS, action_is_waitable};

    #[test]
    fn waitable_classifier_distinguishes_explicit_ignoring_and_flags() {
        for handler in [libc::SIG_DFL, libc::SIG_IGN, 123] {
            for flags in [
                0,
                libc::SA_RESTART,
                libc::SA_NOCLDWAIT,
                libc::SA_NOCLDWAIT | libc::SA_RESTART,
            ] {
                // SAFETY: no syscall or handler invocation uses this test value.
                let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
                action.sa_sigaction = handler;
                action.sa_flags = flags;
                assert_eq!(
                    action_is_waitable(&action),
                    handler != libc::SIG_IGN && flags & libc::SA_NOCLDWAIT == 0
                );
            }
        }
    }

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
