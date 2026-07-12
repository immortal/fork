use std::{
    io,
    os::fd::{FromRawFd, OwnedFd},
};

/// The owned endpoints of a unidirectional pipe.
#[derive(Debug)]
pub struct Pipe {
    reader: OwnedFd,
    writer: OwnedFd,
}

impl Pipe {
    /// Borrow the read endpoint.
    #[must_use]
    pub const fn reader(&self) -> &OwnedFd {
        &self.reader
    }

    /// Borrow the write endpoint.
    #[must_use]
    pub const fn writer(&self) -> &OwnedFd {
        &self.writer
    }

    /// Consume the pipe and return its owned endpoints.
    #[must_use]
    pub fn into_parts(self) -> (OwnedFd, OwnedFd) {
        (self.reader, self.writer)
    }
}

/// The owned endpoints of a bidirectional Unix socket pair.
#[derive(Debug)]
pub struct SocketPair {
    first: OwnedFd,
    second: OwnedFd,
}

impl SocketPair {
    /// Borrow the first endpoint.
    #[must_use]
    pub const fn first(&self) -> &OwnedFd {
        &self.first
    }

    /// Borrow the second endpoint.
    #[must_use]
    pub const fn second(&self) -> &OwnedFd {
        &self.second
    }

    /// Consume the pair and return its owned endpoints.
    #[must_use]
    pub fn into_parts(self) -> (OwnedFd, OwnedFd) {
        (self.first, self.second)
    }
}

/// Create an owned pipe whose endpoints are closed by `exec`.
///
/// Linux and the supported BSDs set `FD_CLOEXEC` atomically at creation. On
/// platforms without `pipe2`, notably macOS, this uses `pipe` followed by
/// `fcntl`; callers must therefore create the broker pipe before starting
/// threads that could concurrently call `exec`.
///
/// # Errors
///
/// Returns the operating-system error from `pipe2`, `pipe`, or `fcntl`.
pub fn pipe_cloexec() -> io::Result<Pipe> {
    let (reader, writer) = create_pipe()?;
    Ok(Pipe { reader, writer })
}

/// Create an owned bidirectional Unix socket pair closed by `exec`.
///
/// Linux and the supported BSDs set `FD_CLOEXEC` atomically at creation. On
/// platforms without `SOCK_CLOEXEC`, notably macOS, this uses `socketpair`
/// followed by `fcntl`; callers must therefore create the pair before starting
/// threads that could concurrently call `exec`.
///
/// # Errors
///
/// Returns the operating-system error from `socketpair` or `fcntl`.
pub fn socket_pair_cloexec() -> io::Result<SocketPair> {
    let mut raw = [-1; 2];
    let socket_type = socket_type_cloexec();
    retry_syscall(|| {
        // SAFETY: `raw` has room for both descriptors written by socketpair.
        unsafe { libc::socketpair(libc::AF_UNIX, socket_type, 0, raw.as_mut_ptr()) }
    })?;

    // SAFETY: a successful socketpair call initialized two distinct owned fds.
    let first = unsafe { OwnedFd::from_raw_fd(raw[0]) };
    // SAFETY: ownership of the other initialized fd is transferred separately.
    let second = unsafe { OwnedFd::from_raw_fd(raw[1]) };

    #[cfg(target_vendor = "apple")]
    {
        set_cloexec(&first)?;
        set_cloexec(&second)?;
    }

    Ok(SocketPair { first, second })
}

#[cfg(any(
    target_os = "android",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "linux",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn create_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut raw = [-1; 2];
    retry_syscall(|| {
        // SAFETY: `raw` has room for both descriptors written by pipe2.
        unsafe { libc::pipe2(raw.as_mut_ptr(), libc::O_CLOEXEC) }
    })?;

    // SAFETY: a successful pipe2 call initialized two distinct owned fds.
    let reader = unsafe { OwnedFd::from_raw_fd(raw[0]) };
    // SAFETY: ownership of the other initialized fd is transferred separately.
    let writer = unsafe { OwnedFd::from_raw_fd(raw[1]) };
    Ok((reader, writer))
}

#[cfg(not(any(
    target_os = "android",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "linux",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
fn create_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut raw = [-1; 2];
    retry_syscall(|| {
        // SAFETY: `raw` has room for both descriptors written by pipe.
        unsafe { libc::pipe(raw.as_mut_ptr()) }
    })?;

    // SAFETY: a successful pipe call initialized two distinct owned fds.
    let reader = unsafe { OwnedFd::from_raw_fd(raw[0]) };
    // SAFETY: ownership of the other initialized fd is transferred separately.
    let writer = unsafe { OwnedFd::from_raw_fd(raw[1]) };
    set_cloexec(&reader)?;
    set_cloexec(&writer)?;
    Ok((reader, writer))
}

#[cfg(not(target_vendor = "apple"))]
const fn socket_type_cloexec() -> libc::c_int {
    libc::SOCK_STREAM | libc::SOCK_CLOEXEC
}

#[cfg(target_vendor = "apple")]
const fn socket_type_cloexec() -> libc::c_int {
    libc::SOCK_STREAM
}

#[cfg(not(any(
    target_os = "android",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "linux",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
fn set_cloexec(fd: &OwnedFd) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let raw = fd.as_raw_fd();
    let current = retry_fcntl(raw, libc::F_GETFD, 0)?;
    retry_fcntl(raw, libc::F_SETFD, current | libc::FD_CLOEXEC).map(|_| ())
}

#[cfg(not(any(
    target_os = "android",
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "linux",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
fn retry_fcntl(
    fd: std::os::fd::RawFd,
    command: libc::c_int,
    argument: libc::c_int,
) -> io::Result<libc::c_int> {
    loop {
        // SAFETY: the command and argument are valid for fcntl and fd is owned.
        let result = unsafe { libc::fcntl(fd, command, argument) };
        if result != -1 {
            return Ok(result);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn retry_syscall(mut operation: impl FnMut() -> libc::c_int) -> io::Result<()> {
    loop {
        if operation() == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
