//! Spawned anchor creation: `posix_spawn` of `/usr/bin/true` into a new group.

use std::{io, mem::MaybeUninit, ptr};

use super::AnchorCreation;

/// Spawn attributes the owner prepares before the helper fork.
///
/// The owner destroys its copy on drop. The helper only reads its inherited
/// copy and leaves through `_exit`, so it never frees or allocates.
#[derive(Debug)]
pub(in crate::group_guard) struct PreparedAnchor {
    attributes: libc::posix_spawnattr_t,
}

impl AnchorCreation for PreparedAnchor {
    /// Prepare attributes for an anchor leading a new group.
    ///
    /// The anchor starts with default dispositions and an empty mask; on Apple
    /// targets it also inherits no descriptor. Fails with the failing
    /// attribute call's error.
    fn prepare() -> io::Result<Self> {
        let mut attributes = MaybeUninit::<libc::posix_spawnattr_t>::uninit();
        // SAFETY: init writes a valid attribute object into this storage.
        spawn_result(unsafe { libc::posix_spawnattr_init(attributes.as_mut_ptr()) })?;
        // SAFETY: init succeeded; Drop now destroys it on every later error.
        let mut spawn = Self {
            attributes: unsafe { attributes.assume_init() },
        };
        // SAFETY: zeroed sets are valid storage, initialized by the calls below.
        let mut defaults: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: live writable sets; SIGKILL and SIGSTOP cannot be changed.
        if unsafe { libc::sigfillset(&raw mut defaults) } != 0
            || unsafe { libc::sigdelset(&raw mut defaults, libc::SIGKILL) } != 0
            || unsafe { libc::sigdelset(&raw mut defaults, libc::SIGSTOP) } != 0
            || unsafe { libc::sigemptyset(&raw mut mask) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let flags = libc::c_short::try_from(spawn_flags())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        // SAFETY: each call updates the initialized attributes from live values.
        spawn_result(unsafe {
            libc::posix_spawnattr_setsigdefault(&raw mut spawn.attributes, &raw const defaults)
        })?;
        // SAFETY: as above.
        spawn_result(unsafe {
            libc::posix_spawnattr_setsigmask(&raw mut spawn.attributes, &raw const mask)
        })?;
        // SAFETY: group 0 makes the spawned anchor lead a new group.
        spawn_result(unsafe { libc::posix_spawnattr_setpgroup(&raw mut spawn.attributes, 0) })?;
        // SAFETY: the flags fit this platform's attribute type.
        spawn_result(unsafe { libc::posix_spawnattr_setflags(&raw mut spawn.attributes, flags) })?;
        Ok(spawn)
    }

    /// Spawn `/usr/bin/true` as the leader of a new group.
    ///
    /// Uses only the inherited attributes and stack-held argument vectors. A
    /// failed spawn leaves no anchor to signal.
    fn child_create(&self) -> Result<libc::pid_t, libc::c_int> {
        let mut anchor: libc::pid_t = 0;
        let argv = [c"true".as_ptr().cast_mut(), ptr::null_mut()];
        let envp = [ptr::null_mut::<libc::c_char>()];
        // SAFETY: the path and both vectors are NUL-terminated and outlive the
        // call; the attributes were initialized by the owner before the fork.
        let result = unsafe {
            libc::posix_spawn(
                &raw mut anchor,
                c"/usr/bin/true".as_ptr(),
                ptr::null(),
                &raw const self.attributes,
                argv.as_ptr(),
                envp.as_ptr(),
            )
        };
        match (result, anchor) {
            (0, anchor) if anchor > 0 => Ok(anchor),
            (0, _) => Err(libc::EIO),
            (error, _) => Err(error),
        }
    }
}

impl Drop for PreparedAnchor {
    fn drop(&mut self) {
        // SAFETY: initialized in `new` and destroyed once, by the owner; the
        // helper exits without running destructors.
        let _ = unsafe { libc::posix_spawnattr_destroy(&raw mut self.attributes) };
    }
}

#[cfg(target_vendor = "apple")]
const fn spawn_flags() -> libc::c_int {
    libc::POSIX_SPAWN_SETPGROUP
        | libc::POSIX_SPAWN_SETSIGDEF
        | libc::POSIX_SPAWN_SETSIGMASK
        | libc::POSIX_SPAWN_CLOEXEC_DEFAULT
}

/// The helper has closed every descriptor but its close-on-exec socket, so
/// `true` inherits nothing even without Apple's `CLOEXEC_DEFAULT`.
#[cfg(not(target_vendor = "apple"))]
const fn spawn_flags() -> libc::c_int {
    libc::POSIX_SPAWN_SETPGROUP | libc::POSIX_SPAWN_SETSIGDEF | libc::POSIX_SPAWN_SETSIGMASK
}

/// `posix_spawn` functions return an errno instead of setting it.
fn spawn_result(result: libc::c_int) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result))
    }
}
