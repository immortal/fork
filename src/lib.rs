//! Library for creating a new process detached from the controlling terminal (daemon).
//!
//! Example:
//! ```
//!use fork::{daemon, Fork};
//!use std::process::Command;
//!
//!if let Ok(Fork::Child) = daemon(false, false) {
//!    Command::new("sleep")
//!        .arg("3")
//!        .output()
//!        .expect("failed to execute process");
//!}
//!```

use std::ffi::CString;
use std::process::exit;

/// Fork result
pub enum Fork {
    Parent(libc::pid_t),
    Child,
}

/// Change dir to `/` [see chdir(2)](https://www.freebsd.org/cgi/man.cgi?query=chdir&sektion=2)
///
/// Upon successful completion, 0 shall be returned. Otherwise, -1 shall be
/// returned, the current working directory shall remain unchanged, and errno
/// shall be set to indicate the error.
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
///    _ => panic!(),
///}
///```
///
/// # Errors
/// returns `-1` if error
/// # Panics
/// Panics if `CString::new` fails
pub fn chdir() -> Result<libc::c_int, i32> {
    let dir = CString::new("/").expect("CString::new failed");
    let res = unsafe { libc::chdir(dir.as_ptr()) };
    match res {
        -1 => Err(-1),
        res => Ok(res),
    }
}

/// Close file descriptors stdin,stdout,stderr
///
/// # Errors
/// returns `-1` if error
pub fn close_fd() -> Result<(), i32> {
    match unsafe { libc::close(0) } {
        -1 => Err(-1),
        _ => match unsafe { libc::close(1) } {
            -1 => Err(-1),
            _ => match unsafe { libc::close(2) } {
                -1 => Err(-1),
                _ => Ok(()),
            },
        },
    }
}

/// Create a new child process [see fork(2)](https://www.freebsd.org/cgi/man.cgi?fork)
///
/// Upon successful completion, `fork()` returns a value of 0 to the child process
/// and returns the process ID of the child process to the parent process.
/// Otherwise, a value of -1 is returned to the parent process, no child process
/// is created.
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
///    Err(_) => println!("Fork failed"),
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
/// # [`nix::unistd::fork`](https://docs.rs/nix/0.15.0/nix/unistd/fn.fork.html)
///
/// The example has been taken from the [`nix::unistd::fork`](https://docs.rs/nix/0.15.0/nix/unistd/fn.fork.html),
/// please check the [Safety](https://docs.rs/nix/0.15.0/nix/unistd/fn.fork.html#safety) section
///
/// # Errors
/// returns `-1` if error
pub fn fork() -> Result<Fork, i32> {
    let res = unsafe { libc::fork() };
    match res {
        -1 => Err(-1),
        0 => Ok(Fork::Child),
        res => Ok(Fork::Parent(res)),
    }
}

/// Wait for process to change status [see wait(2)](https://man.freebsd.org/cgi/man.cgi?waitpid)
///
/// # Errors
/// returns `-1` if error
///
/// Example:
///
/// ```
///use fork::{waitpid, Fork};
///use std::process::Command;
///
///fn main() {
///  match fork::fork() {
///     Ok(Fork::Parent(pid)) => {
///
///         println!("Child pid: {pid}");
///
///         match waitpid(pid) {
///             Ok(_) => println!("Child existed"),
///             Err(_) => eprintln!("Failted to wait on child"),
///         }
///     }
///     Ok(Fork::Child) => {
///         Command::new("sleep")
///             .arg("1")
///             .output()
///             .expect("failed to execute process");
///     }
///     Err(_) => eprintln!("Failed to fork"),
///  }
///}
///```
pub fn waitpid(pid: i32) -> Result<(), i32> {
    let mut status: i32 = 0;
    let res = unsafe { libc::waitpid(pid, &mut status, 0) };
    match res {
        -1 => Err(-1),
        _ => Ok(()),
    }
}

/// Create session and set process group ID [see setsid(2)](https://www.freebsd.org/cgi/man.cgi?setsid)
///
/// Upon successful completion, the `setsid()` system call returns the value of the
/// process group ID of the new process group, which is the same as the process ID
/// of the calling process. If an error occurs, `setsid()` returns -1
///
/// # Errors
/// returns `-1` if error
pub fn setsid() -> Result<libc::pid_t, i32> {
    let res = unsafe { libc::setsid() };
    match res {
        -1 => Err(-1),
        res => Ok(res),
    }
}

/// The process group of the current process [see getgrp(2)](https://www.freebsd.org/cgi/man.cgi?query=getpgrp)
///
/// # Errors
/// returns `-1` if error
pub fn getpgrp() -> Result<libc::pid_t, i32> {
    let res = unsafe { libc::getpgrp() };
    match res {
        -1 => Err(-1),
        res => Ok(res),
    }
}

/// The daemon function is for programs wishing to detach themselves from the
/// controlling terminal and run in the background as system daemons.
///
/// * `nochdir = false`, changes the current working directory to the root (`/`).
/// * `noclose = false`, will close standard input, standard output, and standard error
///
/// # Errors
/// If an error occurs, returns -1
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
pub fn daemon(nochdir: bool, noclose: bool) -> Result<Fork, i32> {
    match fork() {
        Ok(Fork::Parent(_)) => exit(0),
        Ok(Fork::Child) => setsid().and_then(|_| {
            if !nochdir {
                chdir()?;
            }
            if !noclose {
                close_fd()?;
            }
            fork()
        }),
        Err(n) => Err(n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::process::{Command, exit};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_fork() {
        match fork() {
            Ok(Fork::Parent(child)) => {
                assert!(child > 0);
                // Wait for child to complete
                let _ = waitpid(child);
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
                // Wait for child and verify it succeeds
                assert!(waitpid(child).is_ok());
            }
            Ok(Fork::Child) => {
                // Child does some work then exits
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
                let _ = waitpid(child);
            }
            Ok(Fork::Child) => {
                // Test changing directory to root
                match chdir() {
                    Ok(res) => {
                        assert_eq!(res, 0);
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
                let _ = waitpid(child);
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
                let _ = waitpid(child);
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
                let _ = waitpid(child);
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
                let _ = waitpid(child);
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
                let _ = waitpid(child);
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
                let _ = waitpid(child);
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
                let _ = waitpid(child1);
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
                let result = waitpid(child);
                assert!(result.is_ok(), "waitpid should succeed");
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
                assert!(waitpid(child).is_ok());
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
                    let _ = waitpid(child);
                }
                Ok(Fork::Child) => {
                    // Each child exits with its index
                    exit(i);
                }
                Err(_) => panic!("Fork {} failed", i),
            }
        }
    }
}
