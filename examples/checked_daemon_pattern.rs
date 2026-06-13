#![allow(clippy::expect_used)]
#![allow(clippy::indexing_slicing)]
#![allow(clippy::match_wild_err_arm)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::panic)]

//! Checked daemon startup pattern using only fork's low-level primitives.
//!
//! This is intentionally an example, not a library API. Real applications need
//! to define their own readiness point: after opening logs, binding sockets,
//! loading config, writing a PID file, or whatever "ready" means for them.
//!
//! Run with: `cargo run --example checked_daemon_pattern`

use std::{
    fs,
    io::{self, Write},
    os::unix::io::RawFd,
    path::Path,
};

use fork::{Fork, chdir, fork, getpid, redirect_stdio, setsid, waitpid};

const READY_OK: u8 = 0;
const READY_ERR: u8 = 1;
const READY_MESSAGE_LEN: usize = 5;

fn main() -> io::Result<()> {
    let marker = std::env::temp_dir().join(format!(
        "fork_checked_daemon_pattern_{}.marker",
        std::process::id()
    ));
    let _ = fs::remove_file(&marker);

    println!("failure demo: parent waits and receives setup error");
    match checked_daemon_pattern(true, &marker) {
        Err(err) => println!("parent observed expected setup failure: {err}"),
        Ok(Fork::Child) => unreachable!("failure demo should not create a daemon"),
        Ok(Fork::Parent(_)) => unreachable!("successful parent exits inside the pattern"),
    }

    println!("success demo: parent exits only after daemon reports ready");
    io::stdout().flush()?;

    match checked_daemon_pattern(false, &marker)? {
        Fork::Child => {
            // The daemon has already reported readiness. Keep demo daemons short-lived.
            unsafe { libc::_exit(0) };
        }
        Fork::Parent(_) => unreachable!("successful parent exits inside the pattern"),
    }
}

fn checked_daemon_pattern(simulate_failure: bool, marker: &Path) -> io::Result<Fork> {
    let [read_fd, write_fd] = create_pipe()?;

    match fork()? {
        Fork::Parent(child_pid) => {
            close_fd(write_fd);
            let readiness = read_ready(read_fd);
            close_fd(read_fd);

            let _ = waitpid(child_pid);

            match readiness {
                Ok(()) => {
                    let _ = fs::remove_file(marker);
                    unsafe { libc::_exit(0) };
                }
                Err(err) => Err(err),
            }
        }
        Fork::Child => {
            close_fd(read_fd);

            if simulate_failure {
                let err = io::Error::from_raw_os_error(libc::EACCES);
                report_error_and_exit(write_fd, &err);
            }

            run_checked_daemon_child(write_fd, marker)
        }
    }
}

fn run_checked_daemon_child(write_fd: RawFd, marker: &Path) -> io::Result<Fork> {
    if let Err(err) = setsid() {
        report_error_and_exit(write_fd, &err);
    }

    if let Err(err) = chdir() {
        report_error_and_exit(write_fd, &err);
    }

    if let Err(err) = redirect_stdio() {
        report_error_and_exit(write_fd, &err);
    }

    match fork() {
        Ok(Fork::Parent(_)) => {
            close_fd(write_fd);
            unsafe { libc::_exit(0) };
        }
        Ok(Fork::Child) => {
            if let Err(err) = fs::write(marker, format!("daemon pid={}\n", getpid())) {
                report_error_and_exit(write_fd, &err);
            }

            if let Err(err) = write_ready_ok(write_fd) {
                report_error_and_exit(write_fd, &err);
            }

            close_fd(write_fd);
            Ok(Fork::Child)
        }
        Err(err) => report_error_and_exit(write_fd, &err),
    }
}

fn create_pipe() -> io::Result<[RawFd; 2]> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(fds)
}

fn write_ready_ok(fd: RawFd) -> io::Result<()> {
    let msg = [READY_OK, 0, 0, 0, 0];
    write_all_fd(fd, &msg)
}

fn write_ready_err(fd: RawFd, err: &io::Error) -> io::Result<()> {
    let errno = err.raw_os_error().unwrap_or(libc::EIO);
    let mut msg = [0; READY_MESSAGE_LEN];
    msg[0] = READY_ERR;
    msg[1..].copy_from_slice(&errno.to_ne_bytes());
    write_all_fd(fd, &msg)
}

fn read_ready(fd: RawFd) -> io::Result<()> {
    let mut msg = [0; READY_MESSAGE_LEN];
    read_exact_fd(fd, &mut msg)?;

    match msg[0] {
        READY_OK => Ok(()),
        READY_ERR => {
            let errno = i32::from_ne_bytes([msg[1], msg[2], msg[3], msg[4]]);
            Err(io::Error::from_raw_os_error(errno))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown readiness message",
        )),
    }
}

fn write_all_fd(fd: RawFd, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        let written = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if written == -1 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "pipe write returned zero",
            ));
        }

        let written =
            usize::try_from(written).map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        buf = &buf[written..];
    }

    Ok(())
}

fn read_exact_fd(fd: RawFd, buf: &mut [u8]) -> io::Result<()> {
    let mut offset = 0;
    while offset < buf.len() {
        let read = unsafe { libc::read(fd, buf[offset..].as_mut_ptr().cast(), buf.len() - offset) };
        if read == -1 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "readiness pipe closed before a full message",
            ));
        }

        let read = usize::try_from(read).map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        offset += read;
    }

    Ok(())
}

fn report_error_and_exit(write_fd: RawFd, err: &io::Error) -> ! {
    let _ = write_ready_err(write_fd, err);
    close_fd(write_fd);
    unsafe { libc::_exit(1) };
}

fn close_fd(fd: RawFd) {
    let _ = unsafe { libc::close(fd) };
}
