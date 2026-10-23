//! A sandbox that permits only the C library's own `fork` must permit guards.
//!
//! Seccomp policies may filter `clone` by its arguments. This harness-free
//! fixture installs one that allows `clone` only in the exact form the linked C
//! library uses for `fork`, including zero unused slots (or denies it where
//! musl forks with `SYS_fork`), proves that `fork` works and deviating forms are
//! refused, and then requires a guard's callback-free anchor to succeed, pin
//! its group and disarm. The filter is
//! process-wide and permanent, so the fixture runs without libtest.

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    linux::run()
}

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
fn main() {}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod linux {
    use std::{
        error::Error,
        io, ptr, thread,
        time::{Duration, Instant},
    };

    use fork::{ProcessGroupGuard, process_group_exists};

    const TIMEOUT: Duration = Duration::from_secs(3);
    const POLL_INTERVAL: Duration = Duration::from_millis(5);
    /// `seccomp_data`: the syscall number, then each 64-bit argument from
    /// offset 16 in native byte order, so its low word is first only on
    /// little-endian targets (`aarch64_be` is big-endian).
    const NR_OFFSET: u32 = 0;
    const ARGS_OFFSET: u32 = 16;
    const LOAD_WORD: u16 = 0x20;
    const JUMP_EQUAL: u16 = 0x15;
    const RETURN: u16 = 0x06;

    /// musl (including `ohos`) on x86-64 forks with `SYS_fork`, so every
    /// `clone` is refused there.
    const USES_SYS_FORK: bool = cfg!(all(
        any(target_env = "musl", target_env = "ohos"),
        target_arch = "x86_64"
    ));

    /// The exact `clone` arguments, by index, of the C library's own fork. The
    /// child TID pointer's value is unpredictable and stays unchecked, but
    /// glibc passes zero stack, parent-TID and TLS slots (TLS fifth on x86-64,
    /// fourth on `aarch64`). musl passes only the flags and the stack; its other
    /// registers are unspecified.
    fn libc_fork_clone() -> Vec<(u32, u64)> {
        let glibc_flags = u64::from(
            (libc::CLONE_CHILD_SETTID | libc::CLONE_CHILD_CLEARTID | libc::SIGCHLD).unsigned_abs(),
        );
        if cfg!(any(target_env = "musl", target_env = "ohos")) {
            vec![(0, u64::from(libc::SIGCHLD.unsigned_abs())), (1, 0)]
        } else if cfg!(target_arch = "x86_64") {
            vec![(0, glibc_flags), (1, 0), (2, 0), (4, 0)]
        } else {
            vec![(0, glibc_flags), (1, 0), (2, 0), (3, 0)]
        }
    }

    pub(super) fn run() -> Result<(), Box<dyn Error>> {
        install_filter()?;
        libc_fork_is_permitted()?;
        deviating_clones_are_refused()?;
        let guard = ProcessGroupGuard::new()?;
        let group = guard.process_group();
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            if !process_group_exists(group)? {
                return Err("the anchor did not pin its group".into());
            }
            thread::sleep(POLL_INTERVAL);
        }
        guard.disarm(TIMEOUT)?;
        if process_group_exists(group)? {
            return Err("the group outlived its disarmed guard".into());
        }
        drain_children()
    }

    fn instruction(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
        libc::sock_filter { code, jt, jf, k }
    }

    /// `clone` is allowed only when every checked argument word matches; any
    /// other `clone` gets EACCES, and every other system call is allowed.
    fn install_filter() -> Result<(), Box<dyn Error>> {
        let clone = u32::try_from(libc::SYS_clone)?;
        let denied = libc::SECCOMP_RET_ERRNO | u32::try_from(libc::EACCES)?;
        let mut filter = vec![instruction(LOAD_WORD, 0, 0, NR_OFFSET)];
        if USES_SYS_FORK {
            filter.push(instruction(JUMP_EQUAL, 0, 1, clone));
            filter.push(instruction(RETURN, 0, 0, denied));
        } else {
            let mut words = Vec::new();
            let (low, high) = if cfg!(target_endian = "big") {
                (4, 0)
            } else {
                (0, 4)
            };
            for (index, value) in libc_fork_clone() {
                let offset = ARGS_OFFSET + index * 8;
                words.push((offset + low, u32::try_from(value & u64::from(u32::MAX))?));
                words.push((offset + high, u32::try_from(value >> 32)?));
            }
            // A non-clone call skips every word check, the match's ALLOW
            // and the DENY. A mismatched word skips the remaining checks
            // and the match's ALLOW.
            let count = words.len();
            filter.push(instruction(
                JUMP_EQUAL,
                0,
                u8::try_from(count * 2 + 2)?,
                clone,
            ));
            for (position, (offset, value)) in words.into_iter().enumerate() {
                filter.push(instruction(LOAD_WORD, 0, 0, offset));
                let remaining = (count - position - 1) * 2;
                filter.push(instruction(
                    JUMP_EQUAL,
                    0,
                    u8::try_from(remaining + 1)?,
                    value,
                ));
            }
            filter.push(instruction(RETURN, 0, 0, libc::SECCOMP_RET_ALLOW));
            filter.push(instruction(RETURN, 0, 0, denied));
        }
        filter.push(instruction(RETURN, 0, 0, libc::SECCOMP_RET_ALLOW));
        let program = libc::sock_fprog {
            len: u16::try_from(filter.len())?,
            filter: filter.as_mut_ptr(),
        };
        // SAFETY: a standalone single-threaded fixture; the live program only
        // returns EACCES for clone forms the C library's fork does not use.
        let installed = unsafe {
            libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0
                && libc::prctl(
                    libc::PR_SET_SECCOMP,
                    libc::SECCOMP_MODE_FILTER,
                    &raw const program,
                ) == 0
        };
        if installed {
            Ok(())
        } else {
            Err(io::Error::last_os_error().into())
        }
    }

    /// The filter must still admit the C library's own fork.
    fn libc_fork_is_permitted() -> Result<(), Box<dyn Error>> {
        // SAFETY: single-threaded fixture; the child exits at once.
        match unsafe { libc::fork() } {
            -1 => Err(format!(
                "the filter refused libc fork: {}",
                io::Error::last_os_error()
            )
            .into()),
            0 => {
                // SAFETY: leave the child without running fixture destructors.
                unsafe { libc::_exit(0) }
            }
            child => {
                let event = fork::wait_event(fork::ProcessId::try_from(child)?)?;
                if event.is_terminal() {
                    Ok(())
                } else {
                    Err(format!("unexpected libc fork child event: {event:?}").into())
                }
            }
        }
    }

    /// Clone forms other than the C library's must be refused, or the fixture
    /// proves nothing: other flags, and on glibc the right flags with the child
    /// TID pointer repeated in the unused TLS slot. A wrongly admitted child
    /// exits at once and is reaped.
    fn deviating_clones_are_refused() -> Result<(), Box<dyn Error>> {
        let mut tid: libc::pid_t = 0;
        let tid_pointer: *mut libc::pid_t = &raw mut tid;
        let no_pointer: *mut libc::pid_t = ptr::null_mut();
        let sigchld = libc::c_long::from(libc::SIGCHLD);
        let glibc_flags = libc::c_long::from(
            libc::CLONE_CHILD_SETTID | libc::CLONE_CHILD_CLEARTID | libc::SIGCHLD,
        );
        if USES_SYS_FORK {
            expect_refused(sigchld, no_pointer, no_pointer)
        } else if cfg!(any(target_env = "musl", target_env = "ohos")) {
            // musl without SYS_fork: glibc's flags instead of SIGCHLD.
            expect_refused(glibc_flags, tid_pointer, tid_pointer)
        } else {
            // glibc: other flags, and its flags with the TLS slot not zero.
            expect_refused(sigchld, no_pointer, no_pointer)?;
            expect_refused(glibc_flags, tid_pointer, tid_pointer)
        }
    }

    fn expect_refused(
        flags: libc::c_long,
        fourth: *mut libc::pid_t,
        fifth: *mut libc::pid_t,
    ) -> Result<(), Box<dyn Error>> {
        let no_stack: libc::c_long = 0;
        let no_parent_tid: *mut libc::pid_t = ptr::null_mut();
        // SAFETY: without CLONE_VM or a new stack, a successful clone forks this
        // single-threaded process; the child exits at once below.
        let result = unsafe {
            libc::syscall(
                libc::SYS_clone,
                flags,
                no_stack,
                no_parent_tid,
                fourth,
                fifth,
            )
        };
        match result {
            0 => {
                // SAFETY: leave the wrongly admitted child immediately.
                unsafe { libc::_exit(0) }
            }
            -1 if io::Error::last_os_error().raw_os_error() == Some(libc::EACCES) => Ok(()),
            -1 => Err(io::Error::last_os_error().into()),
            child => {
                fork::wait_event(fork::ProcessId::try_from(libc::pid_t::try_from(child)?)?)?;
                Err(format!(
                    "the filter admitted a clone form libc fork does not use (flags {flags:#x})"
                )
                .into())
            }
        }
    }

    fn drain_children() -> Result<(), Box<dyn Error>> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match fork::wait_any_event_nohang() {
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => return Ok(()),
                Err(error) => return Err(error.into()),
                Ok(_) if Instant::now() >= deadline => {
                    return Err("fixture children did not finish".into());
                }
                Ok(_) => thread::sleep(POLL_INTERVAL),
            }
        }
    }
}
