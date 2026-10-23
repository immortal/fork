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
                close_plan: descriptor_close_plan()?,
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
    close_plan: ClosePlan,
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
        close_unintended(&self.allowed, self.close_plan)
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
        close_unintended(&self.allowed, self.close_plan)
    }
}

#[derive(Debug)]
pub(crate) struct GuardPrepared {
    allowed: RawFd,
    close_plan: ClosePlan,
}

impl GuardPrepared {
    pub(crate) fn new(allowed: &OwnedFd) -> io::Result<Self> {
        Ok(Self {
            allowed: allowed.as_raw_fd(),
            close_plan: descriptor_close_plan()?,
        })
    }

    /// Close every descriptor except the guard's owner-lifetime reader.
    pub(crate) fn apply_in_child(&self) -> Result<(), libc::c_int> {
        close_unintended_from(&[self.allowed], 0, self.close_plan)
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

/// A scan bound comes from a pre-fork inventory or descriptor-free hard-limit
/// probe. A successful no-op probe selects Ranges, not a scan fallback.
#[derive(Clone, Copy, Debug)]
enum ClosePlan {
    Ranges,
    Scan(RawFd),
}

fn descriptor_close_plan() -> io::Result<ClosePlan> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: limit points to writable storage for getrlimit.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let soft_limit = bounded_descriptor_limit(limit.rlim_cur)?;
    if range_closing_available() {
        return Ok(ClosePlan::Ranges);
    }
    // FreeBSD's /dev/fd may list only 0..2 without fdescfs. Elsewhere the
    // inventory is the fast path and includes fds above even a lowered hard
    // limit. Neither an open nor an iteration failure makes it authoritative.
    if !cfg!(target_os = "freebsd") {
        let inventory = std::fs::read_dir("/dev/fd").and_then(|entries| {
            inventory_limit(
                soft_limit,
                entries.map(|entry| entry.map(|entry| entry.file_name())),
            )
        });
        if let Ok(bound) = inventory {
            return Ok(ClosePlan::Scan(bound));
        }
    }
    // No descriptor is opened by this fallback, so EMFILE and denied directory
    // access still permit safe preparation. All probing remains before fork.
    Ok(ClosePlan::Scan(probe_limit(
        soft_limit,
        finite_hard_limit(limit.rlim_max)?,
    )))
}

/// Establish a finite ceiling without opening anything. On Linux a finite
/// hard NOFILE cannot exceed `nr_open`; on macOS the kernel ceiling also bounds
/// an unlimited hard NOFILE. Descriptors opened before lowering the *hard*
/// limit can exceed this ceiling: without inventory/ranges that is unknowable.
fn finite_hard_limit(hard: libc::rlim_t) -> io::Result<RawFd> {
    #[cfg(target_os = "macos")]
    let hard = {
        let mut maximum: libc::c_int = 0;
        let mut length = size_of::<libc::c_int>();
        // SAFETY: NUL-terminated name and correctly sized writable output;
        // sysctlbyname queries the kernel without opening a descriptor.
        if unsafe {
            libc::sysctlbyname(
                c"kern.maxfilesperproc".as_ptr(),
                (&raw mut maximum).cast(),
                &raw mut length,
                std::ptr::null_mut(),
                0,
            )
        } != 0
        {
            // A finite hard limit is still a complete ceiling if the optional
            // kernel cap cannot be queried. Only infinity needs that cap.
            if hard != libc::RLIM_INFINITY {
                return bounded_descriptor_limit(hard);
            }
            return Err(io::Error::last_os_error());
        }
        if length != size_of::<libc::c_int>() || maximum <= 0 {
            return Err(io::Error::other("invalid kern.maxfilesperproc"));
        }
        let maximum = libc::rlim_t::try_from(maximum).map_err(io::Error::other)?;
        if hard == libc::RLIM_INFINITY {
            maximum
        } else {
            hard.min(maximum)
        }
    };
    if hard == libc::RLIM_INFINITY {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "descriptor inventory unavailable and no finite hard descriptor limit",
        ));
    }
    bounded_descriptor_limit(hard)
}

/// Include live descriptors above the soft limit without allocating or opening.
fn probe_limit(soft: RawFd, hard: RawFd) -> RawFd {
    let mut bound = soft;
    for descriptor in soft..hard {
        loop {
            // SAFETY: F_GETFD observes an integer descriptor, changing nothing.
            if unsafe { libc::fcntl(descriptor, libc::F_GETFD) } >= 0 {
                bound = descriptor + 1;
                break;
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EINTR) => {}
                Some(libc::EBADF) => break,
                // An unavailable observation cannot shrink the known ceiling.
                // Close through that whole finite range instead of rejecting a
                // previously successful preparation or assuming the fd absent.
                _ => return soft.max(hard),
            }
        }
    }
    bound
}

/// Include every listed numeric descriptor, propagating incomplete iteration.
fn inventory_limit(
    soft_limit: RawFd,
    entries: impl Iterator<Item = io::Result<std::ffi::OsString>>,
) -> io::Result<RawFd> {
    let mut bound = soft_limit;
    for entry in entries {
        let name = entry?;
        if let Some(descriptor) = name.to_str().and_then(|name| name.parse::<RawFd>().ok()) {
            bound = bound.max(descriptor.saturating_add(1));
        }
    }
    Ok(bound)
}

/// Probe on every preparation, so a subsequently installed seccomp filter is
/// honored. No valid descriptor can occupy this harmless unsigned-max range.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn range_closing_available() -> bool {
    close_range(libc::c_uint::MAX, libc::c_uint::MAX) == 0
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
const fn range_closing_available() -> bool {
    false
}

fn bounded_descriptor_limit(limit: libc::rlim_t) -> io::Result<RawFd> {
    let bounded = i128::from(limit).min(i128::from(libc::c_int::MAX));
    RawFd::try_from(bounded).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the bounded descriptor limit does not fit in RawFd",
        )
    })
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

fn close_unintended(allowed: &[RawFd], plan: ClosePlan) -> Result<(), libc::c_int> {
    close_unintended_from(allowed, FIRST_NON_STDIO_FD, plan)
}

fn close_unintended_from(
    allowed: &[RawFd],
    first: RawFd,
    plan: ClosePlan,
) -> Result<(), libc::c_int> {
    match plan {
        // A real range failure is fatal: the probe did not inventory high fds.
        ClosePlan::Ranges => close_with_ranges(allowed, first),
        ClosePlan::Scan(limit) => {
            for descriptor in first..limit {
                if allowed.binary_search(&descriptor).is_err() {
                    close_raw(descriptor);
                }
            }
            Ok(())
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn close_with_ranges(allowed: &[RawFd], first: RawFd) -> Result<(), libc::c_int> {
    let mut range_start = first.cast_unsigned();
    for descriptor in allowed.iter().copied().filter(|fd| *fd >= first) {
        let descriptor = descriptor.cast_unsigned();
        if range_start < descriptor && close_range(range_start, descriptor - 1) == -1 {
            return Err(crate::raw::last_errno());
        }
        range_start = descriptor.saturating_add(1);
    }
    if range_start < libc::c_uint::MAX && close_range(range_start, libc::c_uint::MAX) == -1 {
        return Err(crate::raw::last_errno());
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
const fn close_with_ranges(_allowed: &[RawFd], _first: RawFd) -> Result<(), libc::c_int> {
    Err(libc::ENOSYS)
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

#[cfg(test)]
mod tests {
    use super::{bounded_descriptor_limit, inventory_limit};

    #[test]
    fn inventory_limit_propagates_iteration_failure() {
        let entries = [
            Ok("128".into()),
            Err(std::io::Error::from_raw_os_error(libc::EIO)),
        ];
        assert!(
            matches!(inventory_limit(64, entries.into_iter()), Err(error) if error.raw_os_error() == Some(libc::EIO))
        );
    }

    #[test]
    fn inventory_limit_includes_high_descriptors_and_preserves_soft_bound() -> std::io::Result<()> {
        let entries = [".", "-1", "2", "128", "not-a-descriptor"].map(|name| Ok(name.into()));
        assert_eq!(inventory_limit(64, entries.into_iter())?, 129);
        assert_eq!(inventory_limit(64, std::iter::empty())?, 64);
        Ok(())
    }

    #[test]
    fn bounded_descriptor_limit_saturates_at_raw_fd_max() {
        assert!(matches!(bounded_descriptor_limit(0), Ok(0)));
        assert!(matches!(
            bounded_descriptor_limit(libc::rlim_t::MAX),
            Ok(value) if value == libc::c_int::MAX
        ));
    }
}
