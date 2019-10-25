use libc;
use std::ffi::CString;
use std::process::exit;

pub enum Fork {
    Parent(libc::pid_t),
    Child,
}

// Upon successful completion, fork() returns a value of 0 to the child process
// and returns the process ID of the child process to the parent process.
// Otherwise, a value of -1 is returned to the parent process, no child process
// is created.
pub fn fork() -> Result<Fork, ()> {
    let res = unsafe { libc::fork() };
    match res {
        -1 => Err(()),
        0 => Ok(Fork::Child),
        res => Ok(Fork::Parent(res)),
    }
}

// Upon successful completion, the setsid() system call returns the value of the
// process group ID of the new process group, which is the same as the process ID
// of the calling process. If an error occurs, setsid() returns -1
pub fn setsid() -> Result<libc::pid_t, ()> {
    let res = unsafe { libc::setsid() };
    match res {
        -1 => Err(()),
        res => Ok(res),
    }
}

// Upon successful completion, 0 shall be returned. Otherwise, -1 shall be
// returned, the current working directory shall remain unchanged, and errno
// shall be set to indicate the error.
pub fn chdir() -> Result<libc::c_int, ()> {
    let dir = CString::new("/").expect("CString::new failed");
    let res = unsafe { libc::chdir(dir.as_ptr()) };
    match res {
        -1 => Err(()),
        res => Ok(res),
    }
}

// The parent forks the child
// The parent exits
// The child calls setsid() to start a new session with no controlling terminals
// The child forks a grandchild
// The child exits
// The grandchild is now the daemon
// nochdir = false, changes the current working directory to the root (/).
pub fn daemon(nochdir: bool) -> Result<Fork, ()> {
    match fork() {
        Ok(Fork::Parent(_)) => exit(0),
        Ok(Fork::Child) => setsid().and_then(|_| {
            if nochdir {
                fork()
            } else {
                chdir().and_then(|_| fork())
            }
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
