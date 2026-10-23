//! GNU/Linux ownership-policy and last-observation cleanup schedules.
//!
//! The preload oracle suppresses any stale delivery. Each scenario runs in a
//! fresh process, restores signal state, and collects its exact children and
//! adopted orphans before returning. Static/non-Linux targets skip interposition.

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[path = "owner_cleanup/linux.rs"]
mod linux;

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    linux::run()
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn main() {}
