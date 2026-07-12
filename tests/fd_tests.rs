use std::{
    error::Error,
    fs::File,
    io::{Read, Write},
    os::fd::{AsFd, AsRawFd},
};

use fork::{pipe_cloexec, socket_pair_cloexec};

#[test]
fn pipe_is_owned_cloexec_and_unidirectional() -> Result<(), Box<dyn Error>> {
    let pipe = pipe_cloexec()?;
    assert_cloexec(pipe.reader().as_fd())?;
    assert_cloexec(pipe.writer().as_fd())?;

    let (reader, writer) = pipe.into_parts();
    let mut reader = File::from(reader);
    let mut writer = File::from(writer);
    writer.write_all(b"ready")?;
    drop(writer);

    let mut received = Vec::new();
    reader.read_to_end(&mut received)?;
    assert_eq!(received, b"ready");
    Ok(())
}

#[test]
fn socket_pair_is_owned_cloexec_and_bidirectional() -> Result<(), Box<dyn Error>> {
    let pair = socket_pair_cloexec()?;
    assert_cloexec(pair.first().as_fd())?;
    assert_cloexec(pair.second().as_fd())?;

    let (first, second) = pair.into_parts();
    let mut first = File::from(first);
    let mut second = File::from(second);
    first.write_all(b"request")?;
    let mut request = [0; 7];
    second.read_exact(&mut request)?;
    assert_eq!(&request, b"request");

    second.write_all(b"response")?;
    let mut response = [0; 8];
    first.read_exact(&mut response)?;
    assert_eq!(&response, b"response");
    Ok(())
}

fn assert_cloexec(fd: std::os::fd::BorrowedFd<'_>) -> Result<(), Box<dyn Error>> {
    // SAFETY: F_GETFD only reads flags from the borrowed, live descriptor.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    if flags == -1 {
        return Err(std::io::Error::last_os_error().into());
    }
    assert_ne!(flags & libc::FD_CLOEXEC, 0);
    Ok(())
}
