//! Socket framing tests without process creation or global signal state.

use std::{
    error::Error,
    io::{self, Read, Write},
    net::Shutdown,
    os::{fd::AsRawFd, unix::net::UnixStream},
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

use crate::Signal;

use super::{Exchange, REPLY_BYTES, REQUEST_BYTES, signal_group};

const TIMEOUT: Duration = Duration::from_secs(2);
const SHORT_TIMEOUT: Duration = Duration::from_millis(30);

fn reply_for(helper: &mut UnixStream, errno: i32) -> io::Result<[u8; REPLY_BYTES]> {
    let mut request = [0; REQUEST_BYTES];
    helper.read_exact(&mut request)?;
    let mut reply = [0; REPLY_BYTES];
    reply[..8].copy_from_slice(&request[1..]);
    reply[8..].copy_from_slice(&errno.to_ne_bytes());
    Ok(reply)
}

#[test]
fn partial_late_reply_preserves_framing() -> Result<(), Box<dyn Error>> {
    let (owner, mut helper) = UnixStream::pair()?;
    owner.set_nonblocking(true)?;
    let (resume, resumed) = mpsc::channel();
    helper.set_read_timeout(Some(TIMEOUT))?;
    let server = thread::spawn(move || -> io::Result<()> {
        let reply = reply_for(&mut helper, 0)?;
        helper.write_all(&reply[..4])?;
        resumed.recv_timeout(TIMEOUT).map_err(io::Error::other)?;
        helper.write_all(&reply[4..])?;
        let reply = reply_for(&mut helper, libc::EPERM)?;
        helper.write_all(&reply)
    });
    let exchange = Mutex::new(Exchange::default());
    let first = signal_group(
        owner.as_raw_fd(),
        &exchange,
        Signal::STOP,
        Instant::now() + SHORT_TIMEOUT,
    );
    resume.send(())?;
    let second = signal_group(
        owner.as_raw_fd(),
        &exchange,
        Signal::STOP,
        Instant::now() + TIMEOUT,
    );
    server
        .join()
        .map_err(|_| io::Error::other("reply thread panicked"))??;
    assert!(matches!(first, Err(error) if error.kind() == io::ErrorKind::TimedOut));
    assert!(matches!(second, Err(error) if error.raw_os_error() == Some(libc::EPERM)));
    Ok(())
}

#[test]
fn stale_same_signal_reply_cannot_acknowledge_a_new_request() -> Result<(), Box<dyn Error>> {
    let (owner, mut helper) = UnixStream::pair()?;
    owner.set_nonblocking(true)?;
    let (resume, resumed) = mpsc::channel();
    helper.set_read_timeout(Some(TIMEOUT))?;
    let server = thread::spawn(move || -> io::Result<()> {
        let stale = reply_for(&mut helper, 0)?;
        resumed.recv_timeout(TIMEOUT).map_err(io::Error::other)?;
        helper.write_all(&stale)?;
        let _ = reply_for(&mut helper, 0)?;
        // No reply for the second request until the owner reports its timeout.
        resumed.recv_timeout(TIMEOUT).map_err(io::Error::other)?;
        Ok(())
    });
    let exchange = Mutex::new(Exchange::default());
    let first = signal_group(
        owner.as_raw_fd(),
        &exchange,
        Signal::TERM,
        Instant::now() + SHORT_TIMEOUT,
    );
    resume.send(())?;
    let second = signal_group(
        owner.as_raw_fd(),
        &exchange,
        Signal::TERM,
        Instant::now() + SHORT_TIMEOUT,
    );
    resume.send(())?;
    server
        .join()
        .map_err(|_| io::Error::other("reply thread panicked"))??;
    assert!(matches!(first, Err(error) if error.kind() == io::ErrorKind::TimedOut));
    assert!(matches!(second, Err(error) if error.kind() == io::ErrorKind::TimedOut));
    Ok(())
}

#[test]
fn truncated_reply_reports_eof_without_peer_hangup() -> Result<(), Box<dyn Error>> {
    let (owner, mut helper) = UnixStream::pair()?;
    owner.set_nonblocking(true)?;
    let (release, released) = mpsc::channel();
    helper.set_read_timeout(Some(TIMEOUT))?;
    let server = thread::spawn(move || -> io::Result<()> {
        let reply = reply_for(&mut helper, 0)?;
        helper.write_all(&reply[..4])?;
        helper.shutdown(Shutdown::Write)?;
        // Keep the peer's read half open: on Linux this reports readable EOF
        // without POLLHUP. The owner must consume the prefix then observe EOF.
        released.recv_timeout(TIMEOUT).map_err(io::Error::other)?;
        Ok(())
    });
    let started = Instant::now();
    let result = signal_group(
        owner.as_raw_fd(),
        &Mutex::new(Exchange::default()),
        Signal::TERM,
        started + TIMEOUT,
    );
    release.send(())?;
    server
        .join()
        .map_err(|_| io::Error::other("reply thread panicked"))??;
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::UnexpectedEof));
    assert!(started.elapsed() < Duration::from_millis(500));
    Ok(())
}

#[test]
fn exchange_lock_obeys_deadline() -> Result<(), Box<dyn Error>> {
    let (owner, _helper) = UnixStream::pair()?;
    owner.set_nonblocking(true)?;
    let exchange = Mutex::new(Exchange::default());
    let _held = exchange
        .lock()
        .map_err(|_| io::Error::other("poisoned test lock"))?;
    let start = Instant::now();
    let result = signal_group(
        owner.as_raw_fd(),
        &exchange,
        Signal::TERM,
        start + SHORT_TIMEOUT,
    );
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::TimedOut));
    assert!(start.elapsed() < Duration::from_millis(500));
    Ok(())
}

#[test]
fn request_ids_never_wrap() -> io::Result<()> {
    let mut state = Exchange {
        last_id: u32::MAX - 1,
        ..Exchange::default()
    };
    assert_eq!(state.next_id()?, u32::MAX);
    assert!(matches!(state.next_id(), Err(error) if error.kind() == io::ErrorKind::InvalidInput));
    assert_eq!(state.last_id, u32::MAX);
    Ok(())
}
