use std::{
    collections::BTreeSet,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
};

const FIRST_NON_STDIO_FD: RawFd = 3;

#[derive(Debug)]
struct Mapping {
    source: OwnedFd,
    target: RawFd,
}

#[derive(Debug, Default)]
pub(crate) struct Actions {
    mappings: Vec<Mapping>,
    closed: BTreeSet<RawFd>,
}

impl Actions {
    pub(crate) fn map(&mut self, source: OwnedFd, target: RawFd) -> io::Result<()> {
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "descriptor target must not be negative",
            ));
        }
        if self.mappings.iter().any(|mapping| mapping.target == target) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "descriptor target must be unique",
            ));
        }
        if self.closed.contains(&target) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "descriptor target is explicitly closed",
            ));
        }
        self.mappings.push(Mapping { source, target });
        Ok(())
    }

    pub(crate) fn inherit(&mut self, descriptor: OwnedFd) -> io::Result<()> {
        let target = descriptor.as_raw_fd();
        self.map(descriptor, target)
    }

    pub(crate) fn close(&mut self, descriptor: RawFd) -> io::Result<()> {
        if descriptor < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "descriptor to close must not be negative",
            ));
        }
        if self
            .mappings
            .iter()
            .any(|mapping| mapping.target == descriptor)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mapped descriptor cannot also be explicitly closed",
            ));
        }
        self.closed.insert(descriptor);
        Ok(())
    }

    pub(crate) fn prepare(self, initial_status_writer: OwnedFd) -> io::Result<(Prepared, OwnedFd)> {
        let maximum_mapping_target = self
            .mappings
            .iter()
            .map(|mapping| mapping.target)
            .max()
            .unwrap_or(FIRST_NON_STDIO_FD - 1);
        let maximum_closed_descriptor = self
            .closed
            .iter()
            .copied()
            .max()
            .unwrap_or(FIRST_NON_STDIO_FD - 1);
        let duplicate_minimum = maximum_mapping_target
            .max(maximum_closed_descriptor)
            .checked_add(1)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "descriptor target is too large",
                )
            })?;

        let status_writer = duplicate_cloexec(&initial_status_writer, duplicate_minimum)?;
        drop(initial_status_writer);

        let mut mappings = Vec::with_capacity(self.mappings.len());
        for mapping in self.mappings {
            mappings.push(PreparedMapping {
                source: duplicate_cloexec(&mapping.source, duplicate_minimum)?,
                target: mapping.target,
            });
        }
        let mut allowed: Vec<_> = mappings
            .iter()
            .map(|mapping| mapping.target)
            .filter(|target| *target >= FIRST_NON_STDIO_FD)
            .collect();
        allowed.push(status_writer.as_raw_fd());
        allowed.sort_unstable();
        allowed.dedup();

        Ok((
            Prepared {
                mappings,
                closed: self.closed.into_iter().collect(),
                allowed,
                close_limit: descriptor_close_limit()?,
            },
            status_writer,
        ))
    }
}

#[derive(Debug)]
struct PreparedMapping {
    source: OwnedFd,
    target: RawFd,
}

#[derive(Debug)]
pub(crate) struct Prepared {
    mappings: Vec<PreparedMapping>,
    closed: Vec<RawFd>,
    allowed: Vec<RawFd>,
    close_limit: RawFd,
}

impl Prepared {
    /// Apply the precomputed child descriptor plan without allocation.
    pub(crate) fn apply_in_child(&self) -> Result<(), libc::c_int> {
        for mapping in &self.mappings {
            duplicate_to(mapping.source.as_raw_fd(), mapping.target)?;
        }
        for descriptor in &self.closed {
            close_raw(*descriptor);
        }
        for mapping in &self.mappings {
            close_raw(mapping.source.as_raw_fd());
        }
        close_unintended(&self.allowed, self.close_limit);
        Ok(())
    }

    /// Apply the plan in a pre-thread daemon child that will return to Rust.
    ///
    /// Unlike the exec path, this consumes and drops source ownership exactly
    /// once before closing the remaining unintended descriptors.
    pub(crate) fn apply_before_return(mut self) -> Result<(), libc::c_int> {
        for mapping in &self.mappings {
            duplicate_to(mapping.source.as_raw_fd(), mapping.target)?;
        }
        for descriptor in &self.closed {
            close_raw(*descriptor);
        }
        self.mappings.clear();
        close_unintended(&self.allowed, self.close_limit);
        Ok(())
    }
}

fn duplicate_cloexec(source: &OwnedFd, minimum: RawFd) -> io::Result<OwnedFd> {
    loop {
        // SAFETY: source is live and F_DUPFD_CLOEXEC returns a newly owned fd.
        let raw = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_DUPFD_CLOEXEC, minimum) };
        if raw >= 0 {
            // SAFETY: fcntl returned a new descriptor whose ownership transfers here.
            return Ok(unsafe { OwnedFd::from_raw_fd(raw) });
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn descriptor_close_limit() -> io::Result<RawFd> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: limit points to writable storage for getrlimit.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(limit.rlim_cur.min(libc::c_int::MAX as libc::rlim_t) as RawFd)
}

fn duplicate_to(source: RawFd, target: RawFd) -> Result<(), libc::c_int> {
    loop {
        // SAFETY: source is an owned prepared fd and target is validated nonnegative.
        if unsafe { libc::dup2(source, target) } >= 0 {
            return Ok(());
        }
        let errno = crate::raw::last_errno();
        if errno != libc::EINTR {
            return Err(errno);
        }
    }
}

fn close_unintended(allowed: &[RawFd], close_limit: RawFd) {
    if close_with_ranges(allowed) {
        return;
    }
    for descriptor in FIRST_NON_STDIO_FD..close_limit {
        if allowed.binary_search(&descriptor).is_err() {
            close_raw(descriptor);
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn close_with_ranges(allowed: &[RawFd]) -> bool {
    let mut first = FIRST_NON_STDIO_FD as libc::c_uint;
    for descriptor in allowed
        .iter()
        .copied()
        .filter(|fd| *fd >= FIRST_NON_STDIO_FD)
    {
        let descriptor = descriptor.cast_unsigned();
        if first < descriptor && close_range(first, descriptor - 1) == -1 {
            return false;
        }
        first = descriptor.saturating_add(1);
    }
    first > libc::c_uint::MAX - 1 || close_range(first, libc::c_uint::MAX) == 0
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
const fn close_with_ranges(_allowed: &[RawFd]) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn close_range(first: libc::c_uint, last: libc::c_uint) -> libc::c_int {
    // SAFETY: close_range is invoked with an ordered inclusive descriptor range.
    if unsafe { libc::syscall(libc::SYS_close_range, first, last, 0) } == -1 {
        -1
    } else {
        0
    }
}

#[cfg(target_os = "freebsd")]
fn close_range(first: libc::c_uint, last: libc::c_uint) -> libc::c_int {
    // SAFETY: close_range is invoked with an ordered inclusive descriptor range.
    unsafe { libc::close_range(first, last, 0) }
}

pub(crate) fn close_raw(descriptor: RawFd) {
    // Never retry close: after EINTR the descriptor may already be released.
    // SAFETY: closing an inherited raw descriptor is the intended child action.
    unsafe {
        libc::close(descriptor);
    }
}
