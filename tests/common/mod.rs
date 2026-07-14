//! Common test utilities for fork integration tests
//!
//! This module provides shared helper functions for integration tests,
//! reducing code duplication across test files.

#![allow(dead_code)]

use std::{
    env, fs,
    ops::Deref,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A test directory removed when its parent-side owner leaves scope.
pub struct TestDir(PathBuf);

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Deref for TestDir {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Get a unique test directory with counter to avoid conflicts
pub fn get_unique_test_dir(test_name: &str) -> PathBuf {
    let counter = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    env::temp_dir().join(format!("fork_test_{test_name}_{counter}"))
}

/// Get a simple test directory without counter
pub fn get_test_dir(prefix: &str) -> PathBuf {
    env::temp_dir().join(format!("fork_test_{prefix}"))
}

/// Setup a test directory (creates and cleans if exists)
pub fn setup_test_dir(path: PathBuf) -> TestDir {
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("Failed to create test directory");
    TestDir(path)
}

/// Wait for a file to exist with timeout
pub fn wait_for_file(path: &Path, timeout_ms: u64) -> bool {
    let start = Instant::now();
    while start.elapsed().as_millis() < u128::from(timeout_ms) {
        if path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

/// Cleanup a test directory
pub fn cleanup_test_dir(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

/// Prevent an intentionally aborting child from writing a core file.
pub fn disable_core_dumps() {
    let limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let result = unsafe { libc::setrlimit(libc::RLIMIT_CORE, &raw const limit) };
    assert_eq!(
        result,
        0,
        "failed to disable core dumps: {}",
        std::io::Error::last_os_error()
    );
}
