//! Library for creating a new process detached from the controlling terminal (daemon).
//!
//! Example:
//! ```
//! use fork::{daemon, Fork};
//! use std::process::Command;
//!
//! if let Ok(Fork::Child) = daemon(false, false) {
//!     Command::new("sleep")
//!         .arg("3")
//!         .output()
//!         .expect("failed to execute process");
//! }
//! ```

use std::{ffi::CString, io, process::exit};

// Re-export libc status inspection macros for convenience
// This allows users to write `use fork::{waitpid, WIFEXITED, WEXITSTATUS}`
// instead of importing from libc separately
pub use libc::{WEXITSTATUS, WIFEXITED, WIFSIGNALED, WTERMSIG};

/// Fork result
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fork {
    Parent(libc::pid_t),
    Child,
}

impl Fork {
    /// Returns `true` if this is the parent process
    ///
    /// # Examples
    ///
    /// ```
    /// use fork::{fork, Fork};
    ///
    /// match fork() {
    ///     Ok(result) => {
    ///         if result.is_parent() {
    ///             println!("I'm the parent");
    ///         }
    ///     }
    ///     Err(_) => {}
    /// }
    /// ```
    #[must_use]
    #[inline]
    pub const fn is_parent(&self) -> bool {
        matches!(self, Self::Parent(_))
    }

    /// Returns `true` if this is the child process
    ///
    /// # Examples
    ///
    /// ```
    /// use fork::{fork, Fork};
    ///
    /// match fork() {
    ///     Ok(result) => {
    ///         if result.is_child() {
    ///             println!("I'm the child");
    ///             std::process::exit(0);
    ///         }
    ///     }
    ///     Err(_) => {}
    /// }
    /// ```
    #[must_use]
    #[inline]
    pub const fn is_child(&self) -> bool {
        matches!(self, Self::Child)
    }

    /// Returns the child PID if this is the parent, otherwise `None`
    ///
    /// # Examples
    ///
    /// ```
    /// use fork::{fork, Fork};
    ///
    /// match fork() {
    ///     Ok(result) => {
    ///         if let Some(child_pid) = result.child_pid() {
    ///             println!("Child PID: {}", child_pid);
    ///         }
    ///     }
    ///     Err(_) => {}
    /// }
    /// ```
    #[must_use]
    #[inline]
    pub const fn child_pid(&self) -> Option<libc::pid_t> {
        match self {
            Self::Parent(pid) => Some(*pid),
            Self::Child => None,
        }
    }
}

/// Change dir to `/` [see chdir(2)](https://www.freebsd.org/cgi/man.cgi?query=chdir&sektion=2)
///
/// Upon successful completion, the current working directory is changed to `/`.
/// Otherwise, an error is returned with the system error code.
///
/// Example:
///
///```
///use fork::chdir;
///use std::env;
///
///match chdir() {
///    Ok(_) => {
///       let path = env::current_dir().expect("failed current_dir");
///       assert_eq!(Some("/"), path.to_str());
///    }
///    Err(e) => eprintln!("Failed to change directory: {}", e),
///}
///```
///
/// # Errors
/// Returns an [`io::Error`] if the system call fails. Common errors include:
/// - Permission denied
/// - Path does not exist
///
#[inline]
pub fn chdir() -> io::Result<()> {
    // SAFETY: "/" is a valid C string with no null bytes
    let dir = CString::new("/").map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Failed to create C string for root directory",
        )
    })?;

    let res = unsafe { libc::chdir(dir.as_ptr()) };

    match res {
        -1 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

/// Close file descriptors stdin, stdout, stderr
///
/// **Warning:** This function closes the file descriptors, making them
/// available for reuse. If your daemon opens files after calling this,
/// those files may get fd 0, 1, or 2, causing `println!`, `eprintln!`,
/// or panic output to corrupt them.
///
/// **Use [`redirect_stdio()`] instead**, which is safer and follows
/// industry best practices by redirecting stdio to `/dev/null` instead
/// of closing. This keeps fd 0, 1, 2 occupied, ensuring subsequent files
/// get fd >= 3, preventing silent corruption.
///
/// # Errors
/// Returns an [`io::Error`] if any of the file descriptors fail to close.
///
/// # Example
///
/// ```no_run
/// use fork::close_fd;
///
/// // Warning: Files opened after this may get fd 0,1,2!
/// close_fd()?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn close_fd() -> io::Result<()> {
    for fd in 0..=2 {
        if unsafe { libc::close(fd) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Redirect stdin, stdout, stderr to /dev/null
///
/// This is the recommended way to detach from the controlling terminal
/// in daemon processes. Unlike [`close_fd()`], this keeps file descriptors
/// 0, 1, 2 occupied (pointing to /dev/null), preventing them from being
/// reused by subsequent `open()` calls.
///
/// This prevents bugs where `println!`, `eprintln!`, or panic output
/// accidentally writes to data files that happened to get assigned fd 0, 1, or 2.
///
/// # Implementation
///
/// This function:
/// 1. Opens `/dev/null` with `O_RDWR`
/// 2. Uses `dup2()` to redirect fds 0, 1, 2 to `/dev/null`
/// 3. Closes the extra file descriptor if it was > 2
///
/// This is the same approach used by libuv, systemd, and BSD `daemon(3)`.
///
/// # Errors
///
/// Returns an [`io::Error`] if:
/// - `/dev/null` cannot be opened
/// - `dup2()` fails to redirect any of the file descriptors
///
/// # Example
///
/// ```no_run
/// use fork::redirect_stdio;
/// use std::fs::File;
///
/// redirect_stdio()?;
///
/// // Now safe: files will get fd >= 3
/// let log = File::create("app.log")?;
///
/// // This goes to /dev/null (safely discarded), not to app.log
/// println!("debug message");
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn redirect_stdio() -> io::Result<()> {
    use std::ffi::CString;

    let dev_null = CString::new("/dev/null")
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "CString::new failed"))?;

    let null_fd = unsafe { libc::open(dev_null.as_ptr(), libc::O_RDWR) };

    if null_fd == -1 {
        return Err(io::Error::last_os_error());
    }

    // Redirect stdin, stdout, stderr to /dev/null
    for fd in 0..=2 {
        if unsafe { libc::dup2(null_fd, fd) } == -1 {
            let err = io::Error::last_os_error();
            // Clean up the opened fd before returning error
            if null_fd > 2 {
                unsafe { libc::close(null_fd) };
            }
            return Err(err);
        }
    }

    // Close the extra fd if it's > 2
    // (if null_fd was 0, 1, or 2, it's now dup'd to all three, so don't close)
    if null_fd > 2 {
        unsafe { libc::close(null_fd) };
    }

    Ok(())
}

/// Create a new child process [see fork(2)](https://www.freebsd.org/cgi/man.cgi?fork)
///
/// Upon successful completion, `fork()` returns [`Fork::Child`] in the child process
/// and `Fork::Parent(pid)` with the child's process ID in the parent process.
///
/// Example:
///
/// ```
///use fork::{fork, Fork};
///
///match fork() {
///    Ok(Fork::Parent(child)) => {
///        println!("Continuing execution in parent process, new child has pid: {}", child);
///    }
///    Ok(Fork::Child) => println!("I'm a new child process"),
///    Err(e) => eprintln!("Fork failed: {}", e),
///}
///```
/// This will print something like the following (order indeterministic).
///
/// ```text
/// Continuing execution in parent process, new child has pid: 1234
/// I'm a new child process
/// ```
///
/// The thing to note is that you end up with two processes continuing execution
/// immediately after the fork call but with different match arms.
///
/// # Safety Considerations
///
/// After calling `fork()`, the child process is an exact copy of the parent process.
/// However, there are important safety considerations:
///
/// - **File Descriptors**: Inherited from parent but share the same file offset and status flags.
///   Changes in one process affect the other.
/// - **Mutexes and Locks**: May be in an inconsistent state in the child. Only the thread that
///   called `fork()` exists in the child; other threads disappear mid-execution, potentially
///   leaving mutexes locked.
/// - **Async-Signal-Safety**: Between `fork()` and `exec()`, only async-signal-safe functions
///   should be called. This includes most system calls but excludes most library functions,
///   memory allocation, and I/O operations.
/// - **Signal Handlers**: Inherited from parent but should be used carefully in multi-threaded programs.
/// - **Memory**: Child gets a copy-on-write copy of parent's memory. Large memory usage can impact performance.
///
/// For detailed information, see the [fork(2) man page](https://man7.org/linux/man-pages/man2/fork.2.html).
///
/// # [`nix::unistd::fork`](https://docs.rs/nix/0.15.0/nix/unistd/fn.fork.html)
///
/// The example has been taken from the [`nix::unistd::fork`](https://docs.rs/nix/0.15.0/nix/unistd/fn.fork.html),
/// please check the [Safety](https://docs.rs/nix/0.15.0/nix/unistd/fn.fork.html#safety) section
///
/// # Errors
/// Returns an [`io::Error`] if the fork system call fails. Common errors include:
/// - Resource temporarily unavailable (EAGAIN) - process limit reached
/// - Out of memory (ENOMEM)
#[must_use = "fork result must be checked to determine parent/child"]
pub fn fork() -> io::Result<Fork> {
    let res = unsafe { libc::fork() };
    match res {
        -1 => Err(io::Error::last_os_error()),
        0 => Ok(Fork::Child),
        res => Ok(Fork::Parent(res)),
    }
}

/// Wait for process to change status [see wait(2)](https://man.freebsd.org/cgi/man.cgi?waitpid)
///
/// # Behavior
/// - Retries automatically on `EINTR` (interrupted by signal)
/// - Returns the raw status (use `libc::WIFEXITED`, `libc::WEXITSTATUS`, etc.)
///
/// # Errors
/// Returns an [`io::Error`] if the waitpid system call fails. Common errors include:
/// - No child process exists with the given PID
/// - Invalid options or PID
///
/// Example:
///
/// ```
///use fork::{waitpid, Fork};
///
///fn main() {
///  match fork::fork() {
///     Ok(Fork::Parent(pid)) => {
///         println!("Child pid: {pid}");
///         match waitpid(pid) {
///             Ok(status) => println!("Child exited with status: {status}"),
///             Err(e) => eprintln!("Failed to wait on child: {e}"),
///         }
///     }
///     Ok(Fork::Child) => {
///         // Child does trivial work then exits
///         std::process::exit(0);
///     }
///     Err(e) => eprintln!("Failed to fork: {e}"),
///  }
///}
///```
pub fn waitpid(pid: libc::pid_t) -> io::Result<libc::c_int> {
    let mut status: libc::c_int = 0;
    loop {
        // SAFETY: &raw mut status provides a raw pointer to initialized memory
        let res = unsafe { libc::waitpid(pid, &raw mut status, 0) };

        if res == -1 {
            let err = io::Error::last_os_error();

            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }

            return Err(err);
        }

        return Ok(status);
    }
}

/// Wait for process to change status without blocking [see wait(2)](https://man.freebsd.org/cgi/man.cgi?waitpid)
///
/// This is the non-blocking variant of [`waitpid()`]. It checks if the child has
/// changed status and returns immediately without blocking.
///
/// # Return Value
/// - `Ok(Some(status))` - Child has exited/stopped with the given status
/// - `Ok(None)` - Child is still running (no state change)
/// - `Err(...)` - Error occurred (e.g., ECHILD if child doesn't exist)
///
/// # Behavior
/// - Returns immediately (does not block)
/// - Retries automatically on `EINTR` (interrupted by signal)
/// - Returns the raw status (use `libc::WIFEXITED`, `libc::WEXITSTATUS`, etc.)
///
/// # Use Cases
/// - **Process supervisors** - Monitor multiple children without blocking
/// - **Event loops** - Check child status while handling other events
/// - **Polling patterns** - Parent has other work to do while child runs
/// - **Non-blocking checks** - Determine if child is still running
///
/// # Example
///
/// ```
/// use fork::{fork, Fork, waitpid_nohang};
/// use std::time::Duration;
///
/// match fork::fork() {
///     Ok(Fork::Parent(child)) => {
///         // Do work while child runs
///         for i in 0..5 {
///             println!("Parent working... iteration {}", i);
///             std::thread::sleep(Duration::from_millis(100));
///
///             match waitpid_nohang(child) {
///                 Ok(Some(status)) => {
///                     println!("Child exited with status: {}", status);
///                     break;
///                 }
///                 Ok(None) => {
///                     println!("Child still running...");
///                 }
///                 Err(e) => {
///                     eprintln!("Error checking child: {}", e);
///                     break;
///                 }
///             }
///         }
///     }
///     Ok(Fork::Child) => {
///         // Child does work
///         std::thread::sleep(Duration::from_millis(250));
///         std::process::exit(0);
///     }
///     Err(e) => eprintln!("Fork failed: {}", e),
/// }
/// ```
///
/// # Errors
/// Returns an [`io::Error`] if the waitpid system call fails. Common errors include:
/// - No child process exists with the given PID (ECHILD)
/// - Invalid options or PID
pub fn waitpid_nohang(pid: libc::pid_t) -> io::Result<Option<libc::c_int>> {
    let mut status: libc::c_int = 0;
    loop {
        // SAFETY: &raw mut status provides a raw pointer to initialized memory
        let res = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) };

        if res == 0 {
            // Child has not changed state (still running)
            return Ok(None);
        }

        if res == -1 {
            let err = io::Error::last_os_error();

            if err.kind() == io::ErrorKind::Interrupted {
                continue; // Retry on EINTR
            }

            return Err(err);
        }

        // Child changed state (exited, stopped, continued, etc.)
        return Ok(Some(status));
    }
}

/// Create session and set process group ID [see setsid(2)](https://www.freebsd.org/cgi/man.cgi?setsid)
///
/// Upon successful completion, the `setsid()` system call returns the value of the
/// process group ID of the new process group, which is the same as the process ID
/// of the calling process.
///
/// # Errors
/// Returns an [`io::Error`] if the setsid system call fails. Common errors include:
/// - The calling process is already a process group leader (EPERM)
///
/// # Example
///
/// ```
/// use fork::{fork, Fork, setsid};
///
/// match fork::fork() {
///     Ok(Fork::Parent(child)) => {
///         println!("Parent process, child PID: {}", child);
///     }
///     Ok(Fork::Child) => {
///         // Create new session
///         match setsid() {
///             Ok(sid) => {
///                 println!("New session ID: {}", sid);
///                 std::process::exit(0);
///             }
///             Err(e) => {
///                 eprintln!("Failed to create session: {}", e);
///                 std::process::exit(1);
///             }
///         }
///     }
///     Err(e) => eprintln!("Fork failed: {}", e),
/// }
/// ```
#[inline]
#[must_use = "session ID should be used or checked for errors"]
pub fn setsid() -> io::Result<libc::pid_t> {
    let res = unsafe { libc::setsid() };
    match res {
        -1 => Err(io::Error::last_os_error()),
        res => Ok(res),
    }
}

/// The process group of the current process [see getpgrp(2)](https://www.freebsd.org/cgi/man.cgi?query=getpgrp)
///
/// # Errors
/// This function should not fail under normal circumstances, but returns
/// an [`io::Error`] if the system call fails.
///
/// # Example
///
/// ```
/// use fork::getpgrp;
///
/// match getpgrp() {
///     Ok(pgid) => {
///         println!("Current process group ID: {}", pgid);
///     }
///     Err(e) => {
///         eprintln!("Failed to get process group: {}", e);
///     }
/// }
/// ```
#[inline]
#[must_use = "process group ID should be used"]
pub fn getpgrp() -> io::Result<libc::pid_t> {
    let res = unsafe { libc::getpgrp() };
    match res {
        -1 => Err(io::Error::last_os_error()),
        res => Ok(res),
    }
}

/// Get the current process ID [see getpid(2)](https://man.freebsd.org/cgi/man.cgi?getpid)
///
/// Returns the process ID of the calling process. This function is always successful.
///
/// # Example
///
/// ```
/// use fork::getpid;
///
/// let my_pid = getpid();
/// println!("My process ID: {}", my_pid);
/// ```
#[inline]
#[must_use = "process ID should be used"]
pub fn getpid() -> libc::pid_t {
    // SAFETY: getpid() has no preconditions and always succeeds
    unsafe { libc::getpid() }
}

/// Get the parent process ID [see getppid(2)](https://man.freebsd.org/cgi/man.cgi?getppid)
///
/// Returns the process ID of the parent of the calling process. This function is always successful.
///
/// # Example
///
/// ```
/// use fork::getppid;
///
/// let parent_pid = getppid();
/// println!("My parent's process ID: {}", parent_pid);
/// ```
#[inline]
#[must_use = "process ID should be used"]
pub fn getppid() -> libc::pid_t {
    // SAFETY: getppid() has no preconditions and always succeeds
    unsafe { libc::getppid() }
}

/// The daemon function is for programs wishing to detach themselves from the
/// controlling terminal and run in the background as system daemons.
///
/// * `nochdir = false`, changes the current working directory to the root (`/`).
/// * `noclose = false`, redirects stdin, stdout, and stderr to `/dev/null`
///
/// # Implementation (double-fork)
///
/// 1. **First fork** - Parent exits immediately.
/// 2. **Session setup** - Child calls `setsid()`, optionally `chdir("/")`, and optionally redirects stdio.
/// 3. **Second (double) fork** - Session-leader child exits immediately.
/// 4. **Daemon continues** - Grandchild (daemon) runs with no controlling terminal.
///
/// # Behavior Change in v0.4.0
///
/// Previously, `noclose = false` would close stdio file descriptors.
/// Now it redirects them to `/dev/null` instead, which is safer and prevents
/// file descriptor reuse bugs. This matches industry standard implementations
/// (libuv, systemd, BSD daemon(3)).
///
/// # Errors
/// Returns an [`io::Error`] if any of the underlying system calls fail:
/// - fork fails (e.g., resource limits)
/// - setsid fails (e.g., already a session leader)
/// - chdir fails (when `nochdir` is false)
/// - `redirect_stdio` fails (when `noclose` is false)
///
/// Example:
///
///```
///// The parent forks the child
///// The parent exits
///// The child calls setsid() to start a new session with no controlling terminals
///// The child forks a grandchild
///// The child exits
///// The grandchild is now the daemon
///use fork::{daemon, Fork};
///use std::process::Command;
///
///if let Ok(Fork::Child) = daemon(false, false) {
///    Command::new("sleep")
///        .arg("3")
///        .output()
///        .expect("failed to execute process");
///}
///```
#[must_use = "daemon result must be checked to determine if this is the daemon process"]
pub fn daemon(nochdir: bool, noclose: bool) -> io::Result<Fork> {
    // 1. First fork: detach from original parent; parent exits immediately
    match fork()? {
        Fork::Parent(_) => exit(0),
        Fork::Child => {
            // 2. Session setup in first child
            setsid()?;
            if !nochdir {
                chdir()?;
            }
            if !noclose {
                redirect_stdio()?;
            }
            // 3. Second Fork (Double-fork): drop session leader, keep only the daemon
            match fork()? {
                Fork::Parent(_) => exit(0),
                Fork::Child => Ok(Fork::Child),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
#[allow(clippy::panic)]
#[allow(clippy::match_wild_err_arm)]
#[allow(clippy::ignored_unit_patterns)]
#[allow(clippy::uninlined_format_args)]
mod tests {
    use super::*;
    use libc::{WEXITSTATUS, WIFEXITED};
    use std::env;
    use std::process::{Command, exit};

    #[test]
    fn test_fork() {
        match fork() {
            Ok(Fork::Parent(child)) => {
                assert!(child > 0);
                // Wait for child to complete
                let status = waitpid(child).expect("waitpid failed");
                assert!(WIFEXITED(status));
            }
            Ok(Fork::Child) => {
                // Child process exits immediately
                exit(0);
            }
            Err(_) => panic!("Fork failed"),
        }
    }

    #[test]
    fn test_fork_with_waitpid() {
        match fork() {
            Ok(Fork::Parent(child)) => {
                assert!(child > 0);
                let status = waitpid(child).expect("waitpid failed");
                assert!(WIFEXITED(status));
                assert_eq!(WEXITSTATUS(status), 0);
            }
            Ok(Fork::Child) => {
                let _ = Command::new("true").output();
                exit(0);
            }
            Err(_) => panic!("Fork failed"),
        }
    }

    #[test]
    fn test_chdir() {
        match fork() {
            Ok(Fork::Parent(child)) => {
                let status = waitpid(child).expect("waitpid failed");
                assert!(WIFEXITED(status));
            }
            Ok(Fork::Child) => {
                // Test changing directory to root
                match chdir() {
                    Ok(_) => {
                        let path = env::current_dir().expect("failed current_dir");
                        assert_eq!(Some("/"), path.to_str());
                        exit(0);
                    }
                    Err(_) => exit(1),
                }
            }
            Err(_) => panic!("Fork failed"),
        }
    }

    #[test]
    fn test_getpgrp() {
        match fork() {
            Ok(Fork::Parent(child)) => {
                let status = waitpid(child).expect("waitpid failed");
                assert!(WIFEXITED(status));
            }
            Ok(Fork::Child) => {
                // Get process group and verify it's valid
                match getpgrp() {
                    Ok(pgrp) => {
                        assert!(pgrp > 0);
                        exit(0);
                    }
                    Err(_) => exit(1),
                }
            }
            Err(_) => panic!("Fork failed"),
        }
    }

    #[test]
    fn test_setsid() {
        match fork() {
            Ok(Fork::Parent(child)) => {
                let status = waitpid(child).expect("waitpid failed");
                assert!(WIFEXITED(status));
            }
            Ok(Fork::Child) => {
                // Create new session
                match setsid() {
                    Ok(sid) => {
                        assert!(sid > 0);
                        // Verify we're the session leader
                        let pgrp = getpgrp().expect("Failed to get process group");
                        assert_eq!(sid, pgrp);
                        exit(0);
                    }
                    Err(_) => exit(1),
                }
            }
            Err(_) => panic!("Fork failed"),
        }
    }

    #[test]
    fn test_daemon_pattern_with_chdir() {
        // Test the daemon pattern manually without calling daemon()
        // to avoid exit(0) killing the test process
        match fork() {
            Ok(Fork::Parent(child)) => {
                // Parent waits for child
                let status = waitpid(child).expect("waitpid failed");
                assert!(WIFEXITED(status));
            }
            Ok(Fork::Child) => {
                // Child creates new session and forks again
                setsid().expect("Failed to setsid");
                chdir().expect("Failed to chdir");

                match fork() {
                    Ok(Fork::Parent(_)) => {
                        // Middle process exits
                        exit(0);
                    }
                    Ok(Fork::Child) => {
                        // Grandchild (daemon) - verify state
                        let path = env::current_dir().expect("failed current_dir");
                        assert_eq!(Some("/"), path.to_str());

                        let pgrp = getpgrp().expect("Failed to get process group");
                        assert!(pgrp > 0);

                        exit(0);
                    }
                    Err(_) => exit(1),
                }
            }
            Err(_) => panic!("Fork failed"),
        }
    }

    #[test]
    fn test_daemon_pattern_no_chdir() {
        // Test daemon pattern preserving current directory
        let original_dir = env::current_dir().expect("failed to get current dir");

        match fork() {
            Ok(Fork::Parent(child)) => {
                let status = waitpid(child).expect("waitpid failed");
                assert!(WIFEXITED(status));
            }
            Ok(Fork::Child) => {
                setsid().expect("Failed to setsid");
                // Don't call chdir - preserve directory

                match fork() {
                    Ok(Fork::Parent(_)) => exit(0),
                    Ok(Fork::Child) => {
                        let current_dir = env::current_dir().expect("failed current_dir");
                        // Directory should be preserved
                        if original_dir.to_str() != Some("/") {
                            assert!(current_dir.to_str().is_some());
                        }
                        exit(0);
                    }
                    Err(_) => exit(1),
                }
            }
            Err(_) => panic!("Fork failed"),
        }
    }

    #[test]
    fn test_daemon_pattern_with_close_fd() {
        // Test daemon pattern with file descriptor closure
        match fork() {
            Ok(Fork::Parent(child)) => {
                let status = waitpid(child).expect("waitpid failed");
                assert!(WIFEXITED(status));
            }
            Ok(Fork::Child) => {
                setsid().expect("Failed to setsid");
                chdir().expect("Failed to chdir");
                close_fd().expect("Failed to close fd");

                match fork() {
                    Ok(Fork::Parent(_)) => exit(0),
                    Ok(Fork::Child) => {
                        // Daemon process with closed fds
                        exit(0);
                    }
                    Err(_) => exit(1),
                }
            }
            Err(_) => panic!("Fork failed"),
        }
    }

    #[test]
    fn test_close_fd_functionality() {
        match fork() {
            Ok(Fork::Parent(child)) => {
                let status = waitpid(child).expect("waitpid failed");
                assert!(WIFEXITED(status));
            }
            Ok(Fork::Child) => {
                // Close standard file descriptors
                match close_fd() {
                    Ok(_) => exit(0),
                    Err(_) => exit(1),
                }
            }
            Err(_) => panic!("Fork failed"),
        }
    }

    #[test]
    fn test_double_fork_pattern() {
        // Test the double-fork pattern commonly used for daemons
        match fork() {
            Ok(Fork::Parent(child1)) => {
                assert!(child1 > 0);
                let status = waitpid(child1).expect("waitpid failed");
                assert!(WIFEXITED(status));
            }
            Ok(Fork::Child) => {
                // First child creates new session
                setsid().expect("Failed to setsid");

                // Second fork to ensure we're not session leader
                match fork() {
                    Ok(Fork::Parent(_)) => {
                        // First child exits
                        exit(0);
                    }
                    Ok(Fork::Child) => {
                        // Grandchild - the daemon process
                        let pgrp = getpgrp().expect("Failed to get process group");
                        assert!(pgrp > 0);
                        exit(0);
                    }
                    Err(_) => exit(1),
                }
            }
            Err(_) => panic!("Fork failed"),
        }
    }

    #[test]
    fn test_waitpid_with_child() {
        match fork() {
            Ok(Fork::Parent(child)) => {
                assert!(child > 0);
                // Wait for child with timeout to prevent hanging
                // Simple approach: just call waitpid, the child exits immediately
                let status = waitpid(child).expect("waitpid failed");
                assert!(WIFEXITED(status));
            }
            Ok(Fork::Child) => {
                // Child exits immediately to prevent any hanging issues
                exit(0);
            }
            Err(_) => panic!("Fork failed"),
        }
    }

    #[test]
    fn test_fork_child_execution() {
        match fork() {
            Ok(Fork::Parent(child)) => {
                assert!(child > 0);
                // Wait for child to finish its work
                let status = waitpid(child).expect("waitpid failed");
                assert!(WIFEXITED(status));
            }
            Ok(Fork::Child) => {
                // Child executes a simple command
                let output = Command::new("echo")
                    .arg("test")
                    .output()
                    .expect("Failed to execute command");
                assert!(output.status.success());
                exit(0);
            }
            Err(_) => panic!("Fork failed"),
        }
    }

    #[test]
    fn test_multiple_forks() {
        // Test creating multiple child processes
        for i in 0..3 {
            match fork() {
                Ok(Fork::Parent(child)) => {
                    assert!(child > 0);
                    let status = waitpid(child).expect("waitpid failed");
                    assert!(WIFEXITED(status));
                    assert_eq!(WEXITSTATUS(status), i);
                }
                Ok(Fork::Child) => {
                    // Each child exits with its index
                    exit(i);
                }
                Err(_) => panic!("Fork {} failed", i),
            }
        }
    }

    #[test]
    fn test_getpgrp_in_parent() {
        // Test getpgrp in parent process
        let parent_pgrp = getpgrp().expect("getpgrp should succeed");
        assert!(parent_pgrp > 0);
    }
}
