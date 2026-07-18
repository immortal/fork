//! Process subreaper support for supervisors.

use std::io;

/// Register the current process as a subreaper.
///
/// Orphaned descendants created under this process can then be reparented to
/// it, allowing the [`crate::wait_any`] and [`crate::wait_any_event`] families
/// to observe and reap their terminal state. The operation is idempotent.
///
/// The setting is process-wide, is not inherited by children created with
/// `fork`, and is preserved across `exec`. Acquisition does not affect
/// processes that have already been orphaned. On FreeBSD, children created
/// before acquisition also retain their previously assigned reaper.
///
/// Subreaping does not enumerate descendants or provide a way to signal a
/// still-running descendant that has escaped an owned process group.
///
/// # Errors
///
/// Returns the operating-system error when acquisition fails. On platforms
/// other than Linux and FreeBSD, returns [`io::ErrorKind::Unsupported`].
pub fn acquire_subreaper() -> io::Result<()> {
    imp::acquire()
}

/// Relinquish the current process's explicitly acquired subreaper role.
///
/// Future orphaned descendants will be assigned according to the platform's
/// normal reparenting rules. The operation is idempotent.
///
/// # Errors
///
/// Returns the operating-system error when release fails. On platforms other
/// than Linux and FreeBSD, returns [`io::ErrorKind::Unsupported`].
pub fn release_subreaper() -> io::Result<()> {
    imp::release()
}

/// Report whether the current process explicitly acquired the subreaper role.
///
/// This queries the platform attribute, not whether the process has an
/// implicit reaping responsibility by being PID 1.
///
/// # Errors
///
/// Returns the operating-system error when the query fails. On platforms
/// other than Linux and FreeBSD, returns [`io::ErrorKind::Unsupported`].
pub fn is_subreaper() -> io::Result<bool> {
    imp::is_acquired()
}

#[cfg(target_os = "linux")]
mod imp {
    use std::io;

    pub(super) fn acquire() -> io::Result<()> {
        set(true)
    }

    pub(super) fn release() -> io::Result<()> {
        set(false)
    }

    pub(super) fn is_acquired() -> io::Result<bool> {
        let mut acquired = 0_i32;
        // SAFETY: PR_GET_CHILD_SUBREAPER expects a writable pointer to an int;
        // the remaining variadic arguments are unused and supplied as zero.
        let result = unsafe {
            libc::prctl(
                libc::PR_GET_CHILD_SUBREAPER,
                &raw mut acquired,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            )
        };
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(acquired != 0)
        }
    }

    fn set(acquired: bool) -> io::Result<()> {
        let value: libc::c_ulong = acquired.into();
        // SAFETY: PR_SET_CHILD_SUBREAPER accepts zero or one as its second
        // argument; the remaining variadic arguments are unused.
        let result = unsafe {
            libc::prctl(
                libc::PR_SET_CHILD_SUBREAPER,
                value,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            )
        };
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(target_os = "freebsd")]
mod imp {
    use std::{io, ptr};

    // Flags reported in `procctl_reaper_status::rs_flags` by PROC_REAP_STATUS.
    // OWNED marks any reaper; REALINIT additionally marks PID 1, whose role is
    // implicit rather than explicitly acquired.
    const REAPER_STATUS_OWNED: libc::c_uint = 1;
    const REAPER_STATUS_REALINIT: libc::c_uint = 2;

    #[repr(C)]
    struct ReaperStatus {
        flags: libc::c_uint,
        children: libc::c_uint,
        descendants: libc::c_uint,
        reaper: libc::pid_t,
        pid: libc::pid_t,
        padding: [libc::c_uint; 15],
    }

    impl ReaperStatus {
        const fn zeroed() -> Self {
            Self {
                flags: 0,
                children: 0,
                descendants: 0,
                reaper: 0,
                pid: 0,
                padding: [0; 15],
            }
        }
    }

    pub(super) fn acquire() -> io::Result<()> {
        match command(libc::PROC_REAP_ACQUIRE, ptr::null_mut()) {
            Ok(()) => Ok(()),
            // Acquisition reports EBUSY when the process is already a reaper;
            // treat that as success so acquisition stays idempotent, including
            // for PID 1 whose reaper role is implicit.
            Err(error) if error.raw_os_error() == Some(libc::EBUSY) => {
                if reaper_flags()? & REAPER_STATUS_OWNED != 0 {
                    Ok(())
                } else {
                    Err(error)
                }
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn release() -> io::Result<()> {
        // Only an explicitly acquired role can be released. A process with no
        // role, or PID 1 whose reaper role is implicit (REALINIT) and which the
        // kernel refuses to release with EINVAL, is already in the target state,
        // so report success to keep release idempotent.
        if !holds_acquired_role(reaper_flags()?) {
            return Ok(());
        }
        match command(libc::PROC_REAP_RELEASE, ptr::null_mut()) {
            Ok(()) => Ok(()),
            // Tolerate a concurrent release that already cleared the role.
            Err(error) => match reaper_flags() {
                Ok(flags) if !holds_acquired_role(flags) => Ok(()),
                _ => Err(error),
            },
        }
    }

    pub(super) fn is_acquired() -> io::Result<bool> {
        Ok(holds_acquired_role(reaper_flags()?))
    }

    /// Whether `flags` describe an explicitly acquired role. PID 1 owns the
    /// reaper role implicitly and is additionally flagged REALINIT; exclude it
    /// so the query matches the Linux `PR_GET_CHILD_SUBREAPER` semantics and the
    /// crate never attempts to release init's role.
    const fn holds_acquired_role(flags: libc::c_uint) -> bool {
        flags & REAPER_STATUS_OWNED != 0 && flags & REAPER_STATUS_REALINIT == 0
    }

    fn reaper_flags() -> io::Result<libc::c_uint> {
        let mut status = ReaperStatus::zeroed();
        command(
            libc::PROC_REAP_STATUS,
            (&raw mut status).cast::<libc::c_void>(),
        )?;
        Ok(status.flags)
    }

    fn command(request: libc::c_int, data: *mut libc::c_void) -> io::Result<()> {
        // SAFETY: P_PID with id zero selects the current process. `data` is
        // either null for acquire/release or points to writable ReaperStatus
        // storage with the ABI layout defined by <sys/procctl.h>.
        let result = unsafe { libc::procctl(libc::P_PID, 0, request, data) };
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
mod imp {
    use std::io;

    pub(super) fn acquire() -> io::Result<()> {
        Err(unsupported())
    }

    pub(super) fn release() -> io::Result<()> {
        Err(unsupported())
    }

    pub(super) fn is_acquired() -> io::Result<bool> {
        Err(unsupported())
    }

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "process subreaping is supported only on Linux and FreeBSD",
        )
    }
}
