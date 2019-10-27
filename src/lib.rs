//! Library for creating a new process detached from the controling terminal (daemon)
//!
//! Example:
//! ```
//!use fork::{daemon, Fork};
//!use std::process::Command;
//!
//!fn main() {
//!    if let Ok(Fork::Child) = daemon(false, false) {
//!        Command::new("sleep")
//!            .arg("300")
//!            .output()
//!            .expect("failed to execute process");
//!    }
//!}
//!```

use libc;
use std::ffi::CString;
use std::process::exit;

/// Fork result
pub enum Fork {
    Parent(libc::pid_t),
    Child,
}

/// Upon successful completion, fork() returns a value of 0 to the child process
/// and returns the process ID of the child process to the parent process.
/// Otherwise, a value of -1 is returned to the parent process, no child process
/// is created.
pub fn fork() -> Result<Fork, ()> {
    let res = unsafe { libc::fork() };
    match res {
        -1 => Err(()),
        0 => Ok(Fork::Child),
        res => Ok(Fork::Parent(res)),
    }
}

/// Upon successful completion, the setsid() system call returns the value of the
/// process group ID of the new process group, which is the same as the process ID
/// of the calling process. If an error occurs, setsid() returns -1
pub fn setsid() -> Result<libc::pid_t, ()> {
    let res = unsafe { libc::setsid() };
    match res {
        -1 => Err(()),
        res => Ok(res),
    }
}

/// Upon successful completion, 0 shall be returned. Otherwise, -1 shall be
/// returned, the current working directory shall remain unchanged, and errno
/// shall be set to indicate the error.
pub fn chdir() -> Result<libc::c_int, ()> {
    let dir = CString::new("/").expect("CString::new failed");
    let res = unsafe { libc::chdir(dir.as_ptr()) };
    match res {
        -1 => Err(()),
        res => Ok(res),
    }
}

/// close file descriptors stdin,stdout,stderr
pub fn close_fd() -> Result<(), ()> {
    match unsafe { libc::close(0) } {
        -1 => Err(()),
        _ => match unsafe { libc::close(1) } {
            -1 => Err(()),
            _ => match unsafe { libc::close(2) } {
                -1 => Err(()),
                _ => Ok(()),
            },
        },
    }
}

/// The daemon function is for programs wishing to detach themselves from the
/// controlling terminal and run in the background as system daemons.
///
/// * `nochdir = false`, changes the current working directory to the root (`/`).
/// * `noclose = false`, will close standard input, standard output, and standard error
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
///fn main() {
///    if let Ok(Fork::Child) = daemon(false, false) {
///        Command::new("sleep")
///            .arg("300")
///            .output()
///            .expect("failed to execute process");
///    }
///}
///```
pub fn daemon(nochdir: bool, noclose: bool) -> Result<Fork, ()> {
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

    #[test]
    fn test_fork() {
        if let Ok(Fork::Parent(child)) = fork() {
            assert!(child > 0);
        }
    }
}
