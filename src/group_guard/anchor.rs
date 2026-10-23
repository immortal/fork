//! Callback-free creation of the guard helper's anchor.
//!
//! The helper is a post-fork child of a possibly multithreaded owner, so it may
//! run only async-signal-safe code, and `fork(3)` is not: it runs the
//! application's `pthread_atfork` handlers. A parent handler running in the
//! helper after the anchor exists could reap it or restore SIGCHLD
//! auto-reaping, releasing the pin while the helper still signals the group by
//! number, and a prepare handler could block on a lock that another owner
//! thread held at the helper's own fork. The helper therefore creates the
//! anchor through a primitive that runs no user-space code in either process:
//!
//! - Linux and Android, on architectures whose `clone` returns 0 in the child
//!   through the generic `syscall` interface: the system call the C library's
//!   own `fork` issues, with its flags and argument layout, but without its
//!   handlers (glibc and bionic: `clone` with `CHILD_SETTID | CHILD_CLEARTID |
//!   SIGCHLD`; musl: `SYS_fork`, or `clone(SIGCHLD)` where none exists). A
//!   sandbox that filters these permits the anchor. Only the child TID pointer
//!   differs: glibc passes its thread descriptor's TID field, the anchor a stack
//!   slot. Both are per-process addresses under address randomization, so only
//!   a filter built inside the running process could tell them apart.
//!   SPARC returns the parent's PID in the child and other ports are
//!   unverified, so they use the spawn path below.
//! - FreeBSD: `rfork(RFPROC | RFFDG)`, a raw system-call stub. libthr
//!   interposes `fork` to run handlers, but not `rfork`.
//! - Every other target: `posix_spawn` of `/usr/bin/true` with
//!   `POSIX_SPAWN_SETPGROUP`, so the kernel or the spawn implementation
//!   creates the group before `true` runs; neither runs fork handlers. The
//!   owner prepares the attributes before the helper fork, so the helper
//!   allocates nothing, and the exited `true` stays an unreaped zombie exactly
//!   like a forked anchor. This is the guard's only exec.
//!
//! A raw-forked child calls nothing but `setpgid` and `_exit`, so C library
//! state a raw fork leaves stale (cached PIDs and thread IDs, locks) is never
//! used. Once the helper has reset its signal dispositions, no foreign code
//! runs in it, so nothing but the helper itself can reap the anchor. The
//! helper's own creation is an ordinary fork: application child callbacks run
//! in it before it sanitizes itself, exactly as for the public fork APIs, and
//! no isolation is claimed before they return.

use std::io;

/// Select the creation primitive. The complete platform predicate is written
/// once, here; the other branch is its exact negation.
macro_rules! select_creation {
    ($syscall:meta) => {
        #[cfg($syscall)]
        mod syscall;
        #[cfg($syscall)]
        pub(super) use self::syscall::PreparedAnchor;
        #[cfg(not($syscall))]
        mod spawn;
        #[cfg(not($syscall))]
        pub(super) use self::spawn::PreparedAnchor;
    };
}

select_creation!(any(
    target_os = "freebsd",
    all(
        any(target_os = "linux", target_os = "android"),
        any(
            target_arch = "aarch64",
            target_arch = "arm",
            target_arch = "loongarch64",
            target_arch = "mips",
            target_arch = "mips32r6",
            target_arch = "mips64",
            target_arch = "mips64r6",
            target_arch = "powerpc",
            target_arch = "powerpc64",
            target_arch = "riscv32",
            target_arch = "riscv64",
            target_arch = "s390x",
            target_arch = "x86",
            target_arch = "x86_64",
        )
    )
));

/// The contract every anchor creation primitive meets.
pub(super) trait AnchorCreation: Sized {
    /// Prepare owner-side state before the helper fork.
    ///
    /// # Errors
    ///
    /// Returns the preparation error; nothing has been forked then.
    fn prepare() -> io::Result<Self>;

    /// In the helper: create the anchor as the leader of a new group, without
    /// running fork callbacks or allocating. Returns its PID, or the errno of
    /// the failed creation, in which case no anchor exists to signal.
    fn child_create(&self) -> Result<libc::pid_t, libc::c_int>;
}
