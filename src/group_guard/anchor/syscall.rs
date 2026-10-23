//! Anchor creation by raw system call: `clone` or `fork` on Linux/Android,
//! `rfork` on FreeBSD. No application callback runs and no address space is
//! shared. On Linux the call uses the system call, flags and argument layout of
//! the target C library's own `fork`, so a seccomp policy that filters those
//! permits the anchor; only the per-process child TID address differs.

use std::io;

use super::AnchorCreation;
use crate::group_guard::child_exit;

/// Owner-side anchor state; the raw-fork targets need none.
#[derive(Debug)]
pub(in crate::group_guard) struct PreparedAnchor;

impl AnchorCreation for PreparedAnchor {
    /// Nothing to prepare: the helper forks the anchor with a raw system call.
    fn prepare() -> io::Result<Self> {
        Ok(Self)
    }

    /// Fork the anchor, which creates the group and exits at once.
    ///
    /// The helper also moves the anchor into its own group, closing the race
    /// in which it continues before the anchor ran; failure means the anchor
    /// already did so or exited. `child_tid` receives the anchor's TID in the
    /// anchor's own memory, where glibc and bionic fork ask the kernel to keep
    /// it; this frame outlives the anchor, which never returns from here.
    fn child_create(&self) -> Result<libc::pid_t, libc::c_int> {
        let mut child_tid: libc::pid_t = 0;
        let anchor = raw_fork(&mut child_tid);
        if anchor == 0 {
            child_lead_new_group();
        }
        if anchor < 0 {
            return Err(crate::raw::last_errno());
        }
        // SAFETY: anchor is this helper's unreaped child; setpgid only changes
        // its group.
        let _ = unsafe { libc::setpgid(anchor, anchor) };
        Ok(anchor)
    }
}

/// In the anchor: lead a new group, then exit at once.
///
/// Only raw system calls: no `getpid`, which bionic answers from a cache that
/// a raw fork leaves holding the helper's PID. A failed `setpgid` is covered by
/// the helper's own `setpgid` and its group check.
fn child_lead_new_group() -> ! {
    loop {
        // SAFETY: both zero arguments select this child and a new group.
        if unsafe { libc::setpgid(0, 0) } == 0 || crate::raw::last_errno() != libc::EINTR {
            child_exit(0);
        }
    }
}

/// glibc's and bionic's `fork` without its callbacks: `clone` with
/// `CHILD_SETTID | CHILD_CLEARTID | SIGCHLD`, a child TID pointer, and zero in
/// every other argument, in glibc's `arch_fork` order for the architecture.
///
/// glibc's `kernel-features.h` selects the order: `x86_64` and `loongarch64`
/// use the default (child TID fourth, TLS fifth); `x86`, `arm`, `aarch64`,
/// `powerpc`, `mips` and `riscv` take the TLS before the child TID; `s390x`
/// passes the stack first.
/// Every allow-listed architecture must appear in exactly one list below, or
/// this does not compile. A misplaced pointer would only make the kernel skip
/// the TID write, since the TLS slot is ignored without `CLONE_SETTLS`.
#[cfg(all(
    any(target_os = "linux", target_os = "android"),
    not(any(target_env = "musl", target_env = "ohos"))
))]
fn raw_fork(child_tid: &mut libc::pid_t) -> libc::pid_t {
    let flags =
        libc::c_long::from(libc::CLONE_CHILD_SETTID | libc::CLONE_CHILD_CLEARTID | libc::SIGCHLD);
    let zero: libc::c_long = 0;
    let no_parent_tid: *mut libc::pid_t = std::ptr::null_mut();
    let child_tid: *mut libc::pid_t = child_tid;
    // SAFETY: without CLONE_VM, CLONE_THREAD or a new stack, clone duplicates
    // this single-threaded helper exactly as fork does; the child continues on
    // its own copy of the stack and only makes raw system calls. The kernel
    // writes and clears the TID only in the child's copy of `child_tid`.
    #[cfg(any(target_arch = "x86_64", target_arch = "loongarch64"))]
    let result = unsafe {
        libc::syscall(
            libc::SYS_clone,
            flags,
            zero,
            no_parent_tid,
            child_tid,
            zero,
        )
    };
    // SAFETY: as above, with the TLS before the child TID.
    #[cfg(any(
        target_arch = "aarch64",
        target_arch = "arm",
        target_arch = "mips",
        target_arch = "mips32r6",
        target_arch = "mips64",
        target_arch = "mips64r6",
        target_arch = "powerpc",
        target_arch = "powerpc64",
        target_arch = "riscv32",
        target_arch = "riscv64",
        target_arch = "x86",
    ))]
    let result = unsafe {
        libc::syscall(
            libc::SYS_clone,
            flags,
            zero,
            no_parent_tid,
            zero,
            child_tid,
        )
    };
    // SAFETY: as above, with the stack before the flags.
    #[cfg(target_arch = "s390x")]
    let result = unsafe {
        libc::syscall(
            libc::SYS_clone,
            zero,
            flags,
            no_parent_tid,
            child_tid,
            zero,
        )
    };
    libc::pid_t::try_from(result).unwrap_or(-1)
}

/// musl's `fork` without its callbacks: `SYS_fork` where the architecture has
/// it, as musl itself does.
#[cfg(all(
    target_os = "linux",
    any(target_env = "musl", target_env = "ohos"),
    any(
        target_arch = "arm",
        target_arch = "mips",
        target_arch = "mips64",
        target_arch = "powerpc",
        target_arch = "powerpc64",
        target_arch = "s390x",
        target_arch = "x86",
        target_arch = "x86_64",
    )
))]
fn raw_fork(_child_tid: &mut libc::pid_t) -> libc::pid_t {
    // SAFETY: fork's own system call duplicates this single-threaded helper;
    // musl's user-space fork would add only its atfork handling.
    let result = unsafe { libc::syscall(libc::SYS_fork) };
    libc::pid_t::try_from(result).unwrap_or(-1)
}

/// musl's `fork` without its callbacks where no `SYS_fork` exists:
/// `clone(SIGCHLD, 0)`, as musl itself does.
#[cfg(all(
    target_os = "linux",
    any(target_env = "musl", target_env = "ohos"),
    not(any(
        target_arch = "arm",
        target_arch = "mips",
        target_arch = "mips64",
        target_arch = "powerpc",
        target_arch = "powerpc64",
        target_arch = "s390x",
        target_arch = "x86",
        target_arch = "x86_64",
    ))
))]
fn raw_fork(_child_tid: &mut libc::pid_t) -> libc::pid_t {
    let flags = libc::c_long::from(libc::SIGCHLD);
    let no_stack: libc::c_long = 0;
    // SAFETY: without CLONE_VM or a new stack, clone duplicates this
    // single-threaded helper exactly as fork does.
    let result = unsafe { libc::syscall(libc::SYS_clone, flags, no_stack) };
    libc::pid_t::try_from(result).unwrap_or(-1)
}

/// `fork` without application callbacks: FreeBSD's raw `rfork` stub.
#[cfg(target_os = "freebsd")]
fn raw_fork(_child_tid: &mut libc::pid_t) -> libc::pid_t {
    // SAFETY: RFPROC | RFFDG is fork's process and descriptor-table copy;
    // libthr does not interpose rfork, so no atfork handler runs. The child
    // returns 0 and only makes raw system calls.
    unsafe { libc::rfork(libc::RFPROC | libc::RFFDG) }
}
