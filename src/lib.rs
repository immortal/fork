//! Library for creating a new process detached from the controlling terminal (daemon).
//!
//! # Quick Start
//!
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
//!
//! # Common Patterns
//!
//! ## Process Supervisor
//!
//! Track multiple worker processes by durable worker id, with a PID lookup for
//! wait results:
//!
//! ```no_run
//! use fork::{fork, wait_any_nohang, Fork, WIFEXITED};
//! use std::collections::HashMap;
//!
//! # fn main() -> std::io::Result<()> {
//! #[derive(Clone, Copy, Eq, Hash, PartialEq)]
//! struct WorkerId(u64);
//!
//! struct Worker {
//!     id: WorkerId,
//!     pid: libc::pid_t,
//!     name: String,
//! }
//!
//! let mut workers = HashMap::new();
//! let mut by_pid = HashMap::new();
//!
//! // Spawn 3 workers
//! for i in 0..3 {
//!     let id = WorkerId(i);
//!     match fork()? {
//!         Fork::Parent(pid) => {
//!             workers.insert(
//!                 id,
//!                 Worker {
//!                     id,
//!                     pid,
//!                     name: format!("worker-{}", i),
//!                 },
//!             );
//!             by_pid.insert(pid, id);
//!         }
//!         Fork::Child => {
//!             // Do work...
//!             std::thread::sleep(std::time::Duration::from_secs(5));
//!             std::process::exit(0);
//!         }
//!     }
//! }
//!
//! // Monitor workers without blocking
//! while !workers.is_empty() {
//!     loop {
//!         match wait_any_nohang()? {
//!             Some((pid, status)) => {
//!                 if let Some(id) = by_pid.remove(&pid) {
//!                     let worker = workers.remove(&id).expect("pid map points to worker");
//!                     if WIFEXITED(status) {
//!                         println!(
//!                             "{} (id {}, pid {}) exited",
//!                             worker.name, worker.id.0, worker.pid
//!                         );
//!                     }
//!                 }
//!                 if workers.is_empty() {
//!                     break;
//!                 }
//!             }
//!             None => break,
//!         }
//!     }
//!     std::thread::sleep(std::time::Duration::from_millis(100));
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Inter-Process Communication (IPC) via Pipe
//!
//! ```no_run
//! use fork::{fork, Fork};
//! use std::io::{Read, Write};
//! use std::os::unix::io::FromRawFd;
//!
//! # fn main() -> std::io::Result<()> {
//! // Create pipe before forking
//! let mut pipe_fds = [0i32; 2];
//! unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
//!
//! match fork()? {
//!     Fork::Parent(_child) => {
//!         unsafe { libc::close(pipe_fds[1]) };  // Close write end
//!
//!         let mut reader = unsafe { std::fs::File::from_raw_fd(pipe_fds[0]) };
//!         let mut msg = String::new();
//!         reader.read_to_string(&mut msg)?;
//!         println!("Received: {}", msg);
//!     }
//!     Fork::Child => {
//!         unsafe { libc::close(pipe_fds[0]) };  // Close read end
//!
//!         let mut writer = unsafe { std::fs::File::from_raw_fd(pipe_fds[1]) };
//!         writer.write_all(b"Hello from child!")?;
//!         std::process::exit(0);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Daemon with PID File
//!
//! ```no_run
//! use fork::{daemon, Fork, getpid};
//! use std::fs::File;
//! use std::io::Write;
//!
//! # fn main() -> std::io::Result<()> {
//! if let Ok(Fork::Child) = daemon(false, false) {
//!     // Write PID file
//!     let pid = getpid();
//!     // Use an absolute path: daemon(false, false) changes cwd to `/`.
//!     let mut file = File::create("/var/run/myapp.pid")?;
//!     writeln!(file, "{}", pid)?;
//!
//!     // Run daemon logic...
//!     loop {
//!         // Do work
//!         std::thread::sleep(std::time::Duration::from_secs(60));
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Process Broker with Typed Events (Safe Supervisor)
//!
//! The safe broker API (`PreparedCommand` and `wait_any_event`) ensures that file descriptors are managed securely via close-on-exec, and provides rich typed events (`ChildEvent`) that clearly distinguish between termination and suspension without confusing integer logic.
//!
//! ```no_run
//! use fork::{PreparedCommand, wait_any_event, ChildEvent, ProcessGroup};
//!
//! # fn main() -> std::io::Result<()> {
//! // 1. Prepare a command that runs in its own process group
//! let mut cmd = PreparedCommand::new("/bin/sleep")?;
//! cmd.arg("3")?;
//! cmd.process_group(ProcessGroup::New);
//!
//! // 2. Spawn the child (this ensures all strings, arrays, and descriptors
//! //    are materialized safely before the fork)
//! match cmd.spawn(std::time::Duration::from_secs(3)) {
//!     Ok(child) => {
//!         println!("Spawned child with PID: {}", child.process().get());
//!         
//!         // 3. Monitor using typed events
//!         loop {
//!             // Block until an event occurs
//!             match wait_any_event() {
//!                 Ok(event) => {
//!                     match event {
//!                         ChildEvent::Exited { pid, code } => {
//!                             println!("PID {} exited with code {}", pid.get(), code);
//!                             break;
//!                         }
//!                         ChildEvent::Signalled { pid, signal } => {
//!                             println!("PID {} terminated by signal {}", pid.get(), signal);
//!                             break;
//!                         }
//!                         ChildEvent::Stopped { pid, signal } => {
//!                             println!("PID {} was stopped by {}", pid.get(), signal);
//!                         }
//!                         ChildEvent::Continued { pid } => {
//!                             println!("PID {} continued", pid.get());
//!                         }
//!                     }
//!                 }
//!                 Err(e) => {
//!                     eprintln!("Wait failed: {}", e);
//!                     break;
//!                 }
//!             }
//!         }
//!     }
//!     Err(e) => eprintln!("Failed to spawn child: {}", e),
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ## Checked Daemon Pattern
//!
//! Unlike a traditional "fire and forget" double-fork, the `checked_daemon` pattern allows the intermediate process to wait until the daemon has fully initialized its resources before returning success to the original caller.
//!
//! ```no_run
//! use fork::{checked_daemon, DaemonOptions};
//! use std::time::Duration;
//!
//! # fn main() -> std::io::Result<()> {
//! // Set a timeout to prevent the original caller from hanging indefinitely
//! // if the daemon gets stuck during initialization.
//! let timeout = Duration::from_secs(5);
//!
//! match checked_daemon(DaemonOptions::new(), timeout) {
//!     Ok(fork::CheckedDaemon::Parent(_)) => {
//!         // The original invoker returns successfully ONLY after the daemon
//!         // invokes `notifier.notify_ready()` below.
//!         println!("Daemon started and initialized successfully.");
//!     }
//!     Ok(fork::CheckedDaemon::Daemon(notifier)) => {
//!         // We are now the detached daemon process (session leader).
//!         // Perform initialization (bind ports, allocate memory, etc.)
//!         let initialized = true;
//!
//!         if initialized {
//!             // Notify the parent that we've started successfully
//!             let _ = notifier.notify_ready();
//!             
//!             // Run background service loop...
//!             loop {
//!                 std::thread::sleep(Duration::from_secs(60));
//!             }
//!         } else {
//!             // If initialization fails, bubble the error back to the parent and exit
//!             notifier.fail_and_exit(&std::io::Error::last_os_error());
//!         }
//!     }
//!     Err(e) => {
//!         eprintln!("Failed to launch daemon: {}", e);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Safety and Best Practices
//!
//! - **Always check fork result** - Functions marked `#[must_use]` prevent accidents
//! - **Use `waitpid()`** - Reap child processes to avoid zombies
//! - **Prefer `redirect_stdio()`** - Safer than `close_fd()` for daemons
//! - **Fork early** - Before creating threads, locks, or complex state
//! - **Close unused file descriptors** - Prevent resource leaks in children
//! - **Use durable supervisor ids** - Treat PIDs as live process handles, not
//!   historical identity, because operating systems reuse PIDs after reaping
//! - **Handle signals properly** - Consider what happens in both processes
//!
//! # Platform Compatibility
//!
//! This library uses POSIX system calls and is designed for Unix-like systems:
//! - Linux (all distributions)
//! - macOS (10.5+, replacement for deprecated `daemon(3)`)
//! - FreeBSD, OpenBSD, NetBSD
//! - Other POSIX-compliant systems
//!
//! Windows is **not supported** as it lacks `fork()` system call.

use std::io;

mod child_signal;
mod command;
mod daemon;
mod descriptor;
mod fd;
mod identity;
mod raw;
mod signal;
mod wait;

pub use child_signal::ChildSignalState;
pub use command::{
    PreparedCommand, ProcessCredentials, ProcessGroup, SpawnError, SpawnStage, SpawnedChild,
    SupplementaryGroups,
};
pub use daemon::{
    CheckedDaemon, DaemonCleanup, DaemonError, DaemonNotifier, DaemonOptions, DaemonProcess,
    DaemonStage, checked_daemon,
};
pub use fd::{Pipe, SocketPair, pipe_cloexec, socket_pair_cloexec};
pub use identity::{
    InvalidProcessGroupId, InvalidProcessId, ProcessGroupId, ProcessId,
    create_current_process_group, create_process_group, current_process_group_id,
    current_process_id, join_process_group, process_group,
};
pub use signal::{InvalidSignal, Signal, signal_process, signal_process_group};
pub use wait::{ChildEvent, wait_any_event, wait_any_event_nohang, wait_event, wait_event_nohang};

// Re-export libc status inspection macros for convenience
// This allows users to write `use fork::{waitpid, WIFEXITED, WEXITSTATUS}`
// instead of importing from libc separately
pub use libc::{WEXITSTATUS, WIFEXITED, WIFSIGNALED, WTERMSIG};

/// Fork result
///
/// # Using `Fork` as a map key
///
/// `Fork` derives `Hash`, `Eq`, and `Copy`, but equality is based solely on the
/// raw PID inside `Fork::Parent`. A `Fork::Parent(pid)` key is therefore only a
/// PID key. This is fine for short-lived tables of currently-running children
/// when entries are removed as soon as children are reaped.
///
/// Do not use `Fork::Parent(pid)` as a durable process identity across restarts
/// or historical supervisor state. **PIDs are recycled by the OS once a child
/// is reaped**, so a later child may receive the same PID and compare equal to
/// an old `Fork::Parent(pid)`. For long-lived supervisors, prefer a monotonic
/// logical id of your own as the durable key and keep the current PID as a
/// field or secondary lookup key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fork {
    Parent(libc::pid_t),
    Child,
}

/// Fork result with a checked process identifier in the parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessFork {
    Parent(ProcessId),
    Child,
}

/// Close a file descriptor without retrying on `EINTR`, treating `EBADF` as success.
///
/// On Linux, `close()` always releases the fd before returning `EINTR`, so retrying
/// would risk closing an unrelated fd opened by another thread. On FreeBSD, macOS,
/// and other Unixes the fd state after `EINTR` is unspecified (POSIX 2008+, Austin
/// Group defect 529), making retry equally unsafe. The safe portable behavior is to
/// call `close()` exactly once and treat `EINTR` as success — the same approach used
/// by Rust's stdlib, Go's runtime, and glibc internals.
#[inline]
fn close_once(fd: libc::c_int) -> io::Result<()> {
    let res = unsafe { libc::close(fd) };
    if res == 0 {
        return Ok(());
    }

    let err = io::Error::last_os_error();

    if err.kind() == io::ErrorKind::Interrupted || err.raw_os_error() == Some(libc::EBADF) {
        return Ok(());
    }

    Err(err)
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

    /// Returns the checked child process identifier in the parent.
    #[must_use]
    #[inline]
    pub const fn child_process_id(&self) -> Option<ProcessId> {
        match self {
            Self::Parent(pid) => ProcessId::new(*pid),
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
    // SAFETY: c"/" is a valid null-terminated C string literal
    let res = unsafe { libc::chdir(c"/".as_ptr()) };

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
/// Already-closed descriptors (`EBADF`) are treated as success so the
/// function is idempotent.
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
        close_once(fd)?;
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
    let null_fd = loop {
        let fd = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
        if fd == -1 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        break fd;
    };

    // Redirect stdin, stdout, stderr to /dev/null
    for fd in 0..=2 {
        loop {
            if unsafe { libc::dup2(null_fd, fd) } == -1 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                // Only close null_fd if it's > 2 (not one of the stdio fds we're duplicating to)
                // If null_fd was 0, 1, or 2, we're in the process of duping it, so don't close
                if null_fd > 2 {
                    let _ = close_once(null_fd);
                }
                return Err(err);
            }
            break;
        }
    }

    // Close the extra fd if it's > 2
    // (if null_fd was 0, 1, or 2, it's now dup'd to all three, so don't close)
    if null_fd > 2 {
        close_once(null_fd)?;
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

/// Create a child and return a checked process identifier to the parent.
///
/// This is the preferred fork entry point for supervisors. Like [`fork`], it
/// must be called before creating threads, and the child may perform only
/// async-signal-safe operations before `exec` or `_exit`.
///
/// # Errors
///
/// Returns the operating-system `fork(2)` error or `InvalidData` if the kernel
/// unexpectedly returns a nonpositive child PID to the parent.
#[must_use = "fork result must be checked to determine parent/child"]
pub fn fork_process() -> io::Result<ProcessFork> {
    match fork()? {
        Fork::Parent(raw) => ProcessId::try_from(raw)
            .map(ProcessFork::Parent)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Fork::Child => Ok(ProcessFork::Child),
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
    wait::waitpid_reaped(pid, 0).map(|(_, status)| status)
}

/// Wait for process to change status without blocking [see wait(2)](https://man.freebsd.org/cgi/man.cgi?waitpid)
///
/// This is the non-blocking variant of [`waitpid()`]. It checks if the child has
/// changed status and returns immediately without blocking.
///
/// # Return Value
/// - `Ok(Some(status))` - Child has terminated with the given status
/// - `Ok(None)` - Child is still running (no reportable state change)
/// - `Err(...)` - Error occurred (e.g., ECHILD if child doesn't exist)
///
/// # Behavior
/// - Returns immediately (does not block)
/// - Retries automatically on `EINTR` (interrupted by signal)
/// - Returns the raw status (use `libc::WIFEXITED`, `libc::WEXITSTATUS`, etc.)
/// - Only `WNOHANG` is passed: termination (exit or signal) is reported, but a
///   child that merely *stopped* or *continued* returns `Ok(None)`, since
///   `WUNTRACED`/`WCONTINUED` are not requested
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
    wait::waitpid_reaped_nohang(pid, libc::WNOHANG).map(|maybe_child| {
        maybe_child.map(|(_pid, status)| {
            // Child terminated (only WNOHANG is set, so stop/continue are not reported)
            status
        })
    })
}

/// Wait for any child process to terminate [see wait(2)](https://man.freebsd.org/cgi/man.cgi?waitpid)
///
/// This is equivalent to `waitpid(-1, ...)`: it blocks until any child process
/// terminates, then returns both the PID that was reaped and its raw status.
///
/// Use this when supervising multiple children. Unlike [`waitpid()`], which is
/// aimed at a specific child PID and returns only the status, this function
/// preserves the child PID returned by the underlying `waitpid` system call.
///
/// # Reaping behavior
/// Because this calls `waitpid(-1, ...)`, it reaps **any** child of the current
/// process — including children spawned by other parts of the program, such as
/// [`std::process::Command`]. If you reap such a child here, the standard
/// library's [`std::process::Child::wait`]/`try_wait` will later fail with
/// `ECHILD` because the child no longer exists. Only use `wait_any()` in
/// programs that manage all of their children directly through this crate.
///
/// # Behavior
/// - Blocks until any child terminates
/// - Retries automatically on `EINTR` (interrupted by signal)
/// - Returns `(pid, status)` where `pid` identifies the child that was reaped
/// - Returns the raw status (use `libc::WIFEXITED`, `libc::WEXITSTATUS`, etc.)
///
/// # Errors
/// Returns an [`io::Error`] if the waitpid system call fails. Common errors include:
/// - No unwaited-for child processes exist (ECHILD)
/// - Invalid options
///
/// # Example
///
/// ```
/// use fork::{fork, wait_any, Fork, WEXITSTATUS, WIFEXITED};
///
/// match fork::fork() {
///     Ok(Fork::Parent(child)) => {
///         let (pid, status) = wait_any()?;
///         assert_eq!(pid, child);
///         assert!(WIFEXITED(status));
///         assert_eq!(WEXITSTATUS(status), 0);
///     }
///     Ok(Fork::Child) => std::process::exit(0),
///     Err(e) => eprintln!("Fork failed: {e}"),
/// }
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn wait_any() -> io::Result<(libc::pid_t, libc::c_int)> {
    wait::waitpid_reaped(-1, 0)
}

/// Wait for any child process to terminate without blocking [see wait(2)](https://man.freebsd.org/cgi/man.cgi?waitpid)
///
/// This is the non-blocking variant of [`wait_any()`]. It is equivalent to
/// `waitpid(-1, ..., WNOHANG)`: it checks whether any child has terminated and
/// returns immediately.
///
/// # Reaping behavior
/// Like [`wait_any()`], this reaps **any** child of the current process,
/// including ones spawned by [`std::process::Command`]. Reaping such a child
/// here makes the standard library's [`std::process::Child::wait`]/`try_wait`
/// fail with `ECHILD`. Only use it in programs that manage all of their
/// children directly through this crate.
///
/// # Return Value
/// - `Ok(Some((pid, status)))` - A child terminated; `pid` identifies the reaped child
/// - `Ok(None)` - Child processes exist, but none has terminated yet
/// - `Err(...)` - Error occurred (e.g., ECHILD if there are no unwaited-for children)
///
/// # Behavior
/// - Returns immediately (does not block)
/// - Retries automatically on `EINTR` (interrupted by signal)
/// - Returns the raw status (use `libc::WIFEXITED`, `libc::WEXITSTATUS`, etc.)
/// - Only `WNOHANG` is passed: termination (exit or signal) is reported, but a
///   child that merely *stopped* or *continued* returns `Ok(None)`, since
///   `WUNTRACED`/`WCONTINUED` are not requested
///
/// # Use Cases
/// - **Process supervisors** - Reap whichever child exited and identify it
/// - **Event loops** - Drain exited children after SIGCHLD without blocking
/// - **Polling patterns** - Avoid checking every known child PID individually
///
/// # Errors
/// Returns an [`io::Error`] if the waitpid system call fails. Common errors include:
/// - No unwaited-for child processes exist (ECHILD)
/// - Invalid options
///
/// # Example
///
/// ```
/// use fork::{fork, wait_any_nohang, waitpid, Fork, WIFEXITED};
/// use std::time::Duration;
///
/// match fork::fork() {
///     Ok(Fork::Parent(child)) => {
///         match wait_any_nohang()? {
///             Some((_pid, _status)) => {
///                 // Child exited before the first poll.
///             }
///             None => {
///                 std::thread::sleep(Duration::from_millis(50));
///                 let status = waitpid(child)?;
///                 assert!(WIFEXITED(status));
///             }
///         }
///     }
///     Ok(Fork::Child) => {
///         std::thread::sleep(Duration::from_millis(10));
///         std::process::exit(0);
///     }
///     Err(e) => eprintln!("Fork failed: {e}"),
/// }
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn wait_any_nohang() -> io::Result<Option<(libc::pid_t, libc::c_int)>> {
    wait::waitpid_reaped_nohang(-1, libc::WNOHANG)
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

/// Get the process group ID of the current process [see getpgrp(2)](https://www.freebsd.org/cgi/man.cgi?query=getpgrp)
///
/// Returns the process group ID of the calling process. This function is always successful
/// and cannot fail according to POSIX specification.
///
/// # Example
///
/// ```
/// use fork::getpgrp;
///
/// let pgid = getpgrp();
/// println!("Current process group ID: {}", pgid);
/// ```
#[inline]
#[must_use = "process group ID should be used"]
pub fn getpgrp() -> libc::pid_t {
    // SAFETY: getpgrp() has no preconditions and always succeeds per POSIX
    unsafe { libc::getpgrp() }
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
/// # Common pitfall: relative paths and hidden diagnostics
///
/// With `daemon(false, false)`, code after `daemon()` runs with cwd `/` and
/// stdio attached to `/dev/null`. A relative path such as
/// `File::create("myapp.pid")` is therefore resolved as `/myapp.pid`, not as a
/// file in the directory that launched the program. If creating that file fails,
/// `println!`, `eprintln!`, and panic output are also discarded because stderr
/// points at `/dev/null`.
///
/// Use absolute paths for PID files, logs, sockets, and config files. If your
/// daemon intentionally depends on the launch directory, pass `nochdir = true`.
/// While debugging startup, consider `noclose = true` or a readiness pipe so
/// errors can be observed by the launcher.
///
/// # Not performed by this function
///
/// `daemon()` is intentionally minimal. It does **not** perform several hardening
/// steps that some daemons want; do them yourself if you need them:
///
/// - **`umask`** — the parent's file-mode creation mask is inherited unchanged.
///   Call `unsafe { libc::umask(0) }` (or your preferred mask) if file
///   permissions matter.
/// - **Closing inherited file descriptors > 2** — only stdin/stdout/stderr are
///   handled (and only when `noclose = false`). Any other descriptor the parent
///   left open is inherited by the daemon; close them before or after forking.
/// - **Resetting signal state** — inherited signal dispositions and the signal
///   mask are left as-is. Reset them with `sigaction`/`sigprocmask` if the parent
///   may have customized them.
///
/// # Return Value
///
/// This function only ever returns in the **daemon (grandchild) process**:
///
/// - `Ok(Fork::Child)` — You are the daemon. The original process and the
///   intermediate child have already exited via `_exit(0)`.
/// - `Err(...)` — A system call failed before the daemon could be created.
///
/// **`Ok(Fork::Parent(_))` is never returned** because both parent processes
/// call `_exit(0)` internally. You do not need to match on it:
///
/// # Error observability
///
/// The original (launching) process calls `_exit(0)` at the **first** fork,
/// before `setsid()`, `chdir()`, and `redirect_stdio()` run. As a result, an
/// `Err(...)` from any of those steps is returned only inside the detached
/// first child — a background process with no controlling terminal, whose
/// stderr may already point at `/dev/null` (when `noclose == false`). The
/// launching shell, meanwhile, has already observed exit code `0`. In other
/// words, only a failure of the *first* `fork()` is reportable to the caller;
/// later failures cannot be surfaced to the original process. If you need the
/// launcher to confirm the daemon actually started, implement a readiness
/// handshake (e.g. a pipe the parent reads before exiting) rather than relying
/// on this return value. See `examples/checked_daemon_pattern.rs` for a
/// low-level pattern built from this crate's primitives.
///
/// ```no_run
/// use fork::{daemon, Fork};
///
/// // Recommended: use `if let` — no dead Parent arm needed
/// if let Ok(Fork::Child) = daemon(false, false) {
///     // Only the daemon reaches here
///     loop {
///         // daemon work…
///         std::thread::sleep(std::time::Duration::from_secs(60));
///     }
/// }
/// ```
///
/// If you prefer `match` for explicit error handling, mark the parent arm
/// unreachable:
///
/// ```no_run
/// use fork::{daemon, Fork};
///
/// match daemon(false, false) {
///     Ok(Fork::Child) => {
///         // daemon work…
///     }
///     Ok(Fork::Parent(_)) => unreachable!("daemon() exits both parent processes"),
///     Err(err) => eprintln!("daemon failed: {err}"),
/// }
/// ```
///
/// # Implementation (double-fork)
///
/// 1. **First fork** — Parent calls `_exit(0)` immediately.
/// 2. **Session setup** — Child calls `setsid()`, optionally `chdir("/")`, and optionally redirects stdio.
/// 3. **Second (double) fork** — Session-leader child calls `_exit(0)` immediately.
/// 4. **Daemon continues** — Grandchild (daemon) runs with no controlling terminal.
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
#[must_use = "daemon() only returns Ok(Fork::Child) in the daemon process; check the result"]
pub fn daemon(nochdir: bool, noclose: bool) -> io::Result<Fork> {
    // 1. First fork: detach from original parent; parent exits immediately
    match fork()? {
        // SAFETY: _exit is async-signal-safe and avoids running any Rust/CRT destructors
        Fork::Parent(_) => unsafe { libc::_exit(0) },
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
                // SAFETY: _exit avoids invoking non-async-signal-safe destructors in the forked process
                Fork::Parent(_) => unsafe { libc::_exit(0) },
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
    use std::{
        env,
        os::unix::io::FromRawFd,
        process::{Command, exit},
    };

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
                let pgrp = getpgrp();
                assert!(pgrp > 0);
                exit(0);
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
                        let pgrp = getpgrp();
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

                        let pgrp = getpgrp();
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
    fn test_close_fd_idempotent() {
        match fork() {
            Ok(Fork::Parent(child)) => {
                let status = waitpid(child).expect("waitpid failed");
                assert!(WIFEXITED(status));
                assert_eq!(WEXITSTATUS(status), 0);
            }
            Ok(Fork::Child) => {
                // First close should succeed
                close_fd().expect("first close_fd failed");
                // Second close should treat EBADF as success
                close_fd().expect("second close_fd failed");
                exit(0);
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
                        let pgrp = getpgrp();
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
        let parent_pgrp = getpgrp();
        assert!(parent_pgrp > 0);
    }

    #[test]
    fn test_close_once_ok_and_ebadf() {
        // Create a pipe to obtain valid fds
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(&raw mut fds[0]) }, 0);

        // Close write end via close_once (should succeed)
        close_once(fds[1]).expect("close_once should close valid fd");

        // Wrap read end in File to close it once; drop immediately
        let read_fd = fds[0];
        unsafe { std::fs::File::from_raw_fd(read_fd) };

        // Second close should be treated as success (EBADF path)
        close_once(read_fd).expect("EBADF should be treated as success");
    }
}
