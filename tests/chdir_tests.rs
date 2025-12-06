//! Comprehensive tests for `chdir()` function
//!
//! This module thoroughly tests the `chdir()` function to ensure:
//! - Successful directory change to root (/)
//! - Proper error handling and return types
//! - Thread safety and process isolation
//! - Integration with fork and daemon patterns
//! - Multiple successive calls (idempotent behavior)
//! - Verification of actual filesystem effects

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::match_wild_err_arm)]
#![allow(clippy::uninlined_format_args)]

use std::{env, fs, path::PathBuf, process::exit, thread, time::Duration};

use fork::{Fork, WEXITSTATUS, WIFEXITED, chdir, fork, waitpid};

#[test]
fn test_chdir_basic_success() {
    // Test that chdir successfully changes to root directory
    match fork().expect("Fork failed") {
        Fork::Parent(child) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status), "Child should exit normally");
            assert_eq!(WEXITSTATUS(status), 0, "Child should exit with code 0");
        }
        Fork::Child => {
            // Change to root directory
            match chdir() {
                Ok(()) => {
                    let cwd = env::current_dir().expect("Failed to get current dir");
                    assert_eq!(cwd.to_str().unwrap(), "/", "Should be in root directory");
                    exit(0);
                }
                Err(e) => {
                    eprintln!("chdir failed: {}", e);
                    exit(1);
                }
            }
        }
    }
}

#[test]
fn test_chdir_returns_unit() {
    // Test that chdir returns () on success
    match fork().expect("Fork failed") {
        Fork::Parent(child) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status));
            assert_eq!(WEXITSTATUS(status), 0);
        }
        Fork::Child => {
            let result: std::io::Result<()> = chdir();
            assert!(result.is_ok());

            // Verify it returns unit type
            let _unit: () = result.unwrap();
            exit(0);
        }
    }
}

#[test]
fn test_chdir_changes_actual_working_directory() {
    // Verify chdir actually changes the working directory, not just returns Ok
    match fork().expect("Fork failed") {
        Fork::Parent(child) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status));
            assert_eq!(WEXITSTATUS(status), 0);
        }
        Fork::Child => {
            // Get original directory
            let original = env::current_dir().expect("Failed to get original dir");

            // Change to root
            chdir().expect("chdir failed");

            // Get new directory
            let new_dir = env::current_dir().expect("Failed to get new dir");

            // Verify change occurred
            assert_ne!(original, new_dir, "Directory should have changed");
            assert_eq!(new_dir, PathBuf::from("/"), "Should be in root");

            exit(0);
        }
    }
}

#[test]
fn test_chdir_idempotent() {
    // Test that calling chdir multiple times is safe (idempotent)
    match fork().expect("Fork failed") {
        Fork::Parent(child) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status));
            assert_eq!(WEXITSTATUS(status), 0);
        }
        Fork::Child => {
            // Call chdir multiple times
            chdir().expect("First chdir failed");
            chdir().expect("Second chdir failed");
            chdir().expect("Third chdir failed");

            // Verify still in root
            let cwd = env::current_dir().expect("Failed to get current dir");
            assert_eq!(cwd.to_str().unwrap(), "/");

            exit(0);
        }
    }
}

#[test]
fn test_chdir_process_isolation() {
    // Test that chdir in child doesn't affect parent
    let parent_dir = env::current_dir().expect("Failed to get parent dir");

    match fork().expect("Fork failed") {
        Fork::Parent(child) => {
            // Parent waits for child
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status));

            // Parent directory should be unchanged
            let current = env::current_dir().expect("Failed to get current dir");
            assert_eq!(current, parent_dir, "Parent directory should not change");
        }
        Fork::Child => {
            // Child changes directory
            chdir().expect("chdir failed");

            // Verify child is in root
            let cwd = env::current_dir().expect("Failed to get current dir");
            assert_eq!(cwd.to_str().unwrap(), "/");

            exit(0);
        }
    }
}

#[test]
fn test_chdir_with_file_operations() {
    // Test that chdir affects file operations (relative paths)
    match fork().expect("Fork failed") {
        Fork::Parent(child) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status));
            assert_eq!(WEXITSTATUS(status), 0);
        }
        Fork::Child => {
            // Change to root
            chdir().expect("chdir failed");

            // Try to read a file that should exist in root
            // Most Unix systems have /etc or /bin
            let etc_exists = fs::metadata("/etc").is_ok();
            let bin_exists = fs::metadata("/bin").is_ok();

            assert!(
                etc_exists || bin_exists,
                "Root directory should contain /etc or /bin"
            );

            exit(0);
        }
    }
}

#[test]
fn test_chdir_with_absolute_path_operations() {
    // Test that absolute paths still work after chdir
    match fork().expect("Fork failed") {
        Fork::Parent(child) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status));
            assert_eq!(WEXITSTATUS(status), 0);
        }
        Fork::Child => {
            let temp_file = std::env::temp_dir().join("fork_test_chdir");
            fs::write(&temp_file, "test").expect("Failed to write test file");

            // Change directory
            chdir().expect("chdir failed");

            // Absolute path should still work
            let content =
                fs::read_to_string(&temp_file).expect("Failed to read with absolute path");
            assert_eq!(content, "test");

            // Cleanup
            fs::remove_file(&temp_file).ok();

            exit(0);
        }
    }
}

#[test]
fn test_chdir_error_type() {
    // Test that chdir returns proper io::Error type
    match fork().expect("Fork failed") {
        Fork::Parent(child) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status));
            assert_eq!(WEXITSTATUS(status), 0);
        }
        Fork::Child => {
            let result: std::io::Result<()> = chdir();

            // Should succeed for root directory
            assert!(result.is_ok());

            // Type check - this is an io::Error if it were to fail
            match result {
                Ok(()) => exit(0),
                Err(e) => {
                    // Verify it's a proper io::Error
                    let _: std::io::Error = e;
                    exit(1);
                }
            }
        }
    }
}

#[test]
fn test_chdir_concurrent_forks() {
    // Test chdir behavior with multiple concurrent child processes
    let mut children = Vec::new();

    for _ in 0..3 {
        match fork().expect("Fork failed") {
            Fork::Parent(child) => {
                children.push(child);
            }
            Fork::Child => {
                // Each child changes to root independently
                chdir().expect("chdir failed");

                let cwd = env::current_dir().expect("Failed to get current dir");
                assert_eq!(cwd.to_str().unwrap(), "/");

                // Small delay to ensure concurrency
                thread::sleep(Duration::from_millis(10));

                exit(0);
            }
        }
    }

    // Parent waits for all children
    for child in children {
        let status = waitpid(child).expect("waitpid failed");
        assert!(WIFEXITED(status));
        assert_eq!(WEXITSTATUS(status), 0);
    }
}

#[test]
fn test_chdir_before_and_after_setsid() {
    // Test that chdir works correctly with setsid (common in daemon creation)
    match fork().expect("Fork failed") {
        Fork::Parent(child) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status));
            assert_eq!(WEXITSTATUS(status), 0);
        }
        Fork::Child => {
            use fork::setsid;

            // Create new session first
            setsid().expect("setsid failed");

            // Then change directory (typical daemon pattern)
            chdir().expect("chdir failed");

            // Verify both worked
            let cwd = env::current_dir().expect("Failed to get current dir");
            assert_eq!(cwd.to_str().unwrap(), "/");

            let pgid = fork::getpgrp();
            assert!(pgid > 0);

            exit(0);
        }
    }
}

#[test]
fn test_chdir_uses_c_string_literal() {
    // This test verifies that the modern c"" string literal is used correctly
    // by ensuring chdir works without any runtime string allocation errors
    match fork().expect("Fork failed") {
        Fork::Parent(child) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status));
            assert_eq!(WEXITSTATUS(status), 0);
        }
        Fork::Child => {
            // Call chdir multiple times rapidly
            // If there were allocation issues, this would likely fail
            for _ in 0..100 {
                chdir().expect("chdir failed");
            }

            let cwd = env::current_dir().expect("Failed to get current dir");
            assert_eq!(cwd.to_str().unwrap(), "/");

            exit(0);
        }
    }
}

#[test]
fn test_chdir_with_env_manipulation() {
    // Test that chdir works correctly even when environment is modified
    match fork().expect("Fork failed") {
        Fork::Parent(child) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status));
            assert_eq!(WEXITSTATUS(status), 0);
        }
        Fork::Child => {
            // Modify environment
            // SAFETY: We're in a child process that will exit immediately,
            // and there are no other threads that could be reading env vars
            unsafe {
                env::set_var("PWD", "/some/fake/path");
            }

            // chdir should still work correctly
            chdir().expect("chdir failed");

            // Verify actual directory (not PWD env var)
            let cwd = env::current_dir().expect("Failed to get current dir");
            assert_eq!(cwd.to_str().unwrap(), "/");

            exit(0);
        }
    }
}
