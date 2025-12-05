//! Error handling and edge case tests
//!
//! This module tests error scenarios and edge cases for fork library functions:
//! - `setsid()` called when already a session leader (EPERM)
//! - `close_fd()` error handling
//! - Error type verification (`io::Error`)
//!
//! These tests ensure proper error propagation and handling.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::match_wild_err_arm)]
#![allow(clippy::similar_names)]
#![allow(clippy::uninlined_format_args)]

use std::process::exit;

use fork::{Fork, close_fd, fork, getpgrp, setsid, waitpid};

#[test]
fn test_setsid_error_when_already_session_leader() {
    // Tests that setsid returns error when called by a session leader
    // Expected behavior:
    // 1. Child calls setsid() to become session leader (succeeds)
    // 2. Child immediately calls setsid() again (should fail with EPERM)
    // 3. Verifies proper error handling
    match fork() {
        Ok(Fork::Parent(child)) => {
            waitpid(child).expect("waitpid failed");
        }
        Ok(Fork::Child) => {
            // First setsid should succeed
            let sid = setsid().expect("First setsid should succeed");
            assert!(sid > 0, "SID should be positive");

            // Verify we're a session leader (PID == PGID)
            let pid = unsafe { libc::getpid() };
            let pgid = getpgrp().expect("getpgrp failed");
            assert_eq!(pid, pgid, "Should be session leader");

            // Second setsid should fail with EPERM
            let result = setsid();
            assert!(result.is_err(), "Second setsid should fail");

            let err = result.unwrap_err();
            assert_eq!(
                err.raw_os_error(),
                Some(libc::EPERM),
                "Should return EPERM when already session leader"
            );

            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_fork_returns_io_error_type() {
    // Tests that fork returns proper io::Error type
    // Expected behavior:
    // 1. fork() returns io::Result<Fork>
    // 2. Error type can be inspected with raw_os_error()
    // 3. Verifies type signature from v0.4.0
    match fork() {
        Ok(Fork::Parent(child)) => {
            waitpid(child).expect("waitpid failed");
        }
        Ok(Fork::Child) => exit(0),
        Err(e) => {
            // If fork somehow fails, verify it's a proper io::Error
            let _errno = e.raw_os_error();
            let _error_msg = format!("{}", e);
            panic!("Fork failed: {}", e);
        }
    }
}

#[test]
fn test_waitpid_returns_io_error_type() {
    // Tests that waitpid returns proper io::Error on failure
    // Expected behavior:
    // 1. waitpid on invalid PID returns Err(io::Error)
    // 2. Error has raw_os_error() method
    // 3. Error can be formatted as string
    match fork() {
        Ok(Fork::Parent(_)) => {
            // Try to wait on invalid PID
            let result = waitpid(999_999);
            assert!(result.is_err(), "Should fail on invalid PID");

            let err = result.unwrap_err();
            // Verify io::Error properties
            assert!(err.raw_os_error().is_some(), "Should have errno");
            let error_string = format!("{}", err);
            assert!(!error_string.is_empty(), "Should have error message");
        }
        Ok(Fork::Child) => exit(0),
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_setsid_returns_io_error_type() {
    // Tests that setsid returns proper io::Error on failure
    match fork() {
        Ok(Fork::Parent(child)) => {
            waitpid(child).expect("waitpid failed");
        }
        Ok(Fork::Child) => {
            // First call succeeds
            setsid().expect("First setsid should succeed");

            // Second call fails
            let result = setsid();
            assert!(result.is_err(), "Second setsid should fail");

            let err = result.unwrap_err();
            // Verify io::Error properties
            assert_eq!(err.raw_os_error(), Some(libc::EPERM));
            let error_string = format!("{}", err);
            assert!(
                error_string.contains("Operation not permitted") || error_string.contains("EPERM"),
                "Error message should indicate permission error: {}",
                error_string
            );

            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_getpgrp_returns_io_error_type() {
    // Tests that getpgrp returns proper io::Result type
    // (getpgrp shouldn't fail normally, but verify type)
    match fork() {
        Ok(Fork::Parent(child)) => {
            waitpid(child).expect("waitpid failed");
        }
        Ok(Fork::Child) => {
            let result: std::io::Result<libc::pid_t> = getpgrp();
            assert!(result.is_ok(), "getpgrp should succeed");

            let pgid = result.unwrap();
            assert!(pgid > 0, "PGID should be positive");

            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_close_fd_error_handling() {
    // Tests that close_fd handles errors properly
    // Note: This is tricky because closing stdin/stdout/stderr
    // should normally succeed. This test just verifies the function
    // returns io::Result and can be error-checked.
    match fork() {
        Ok(Fork::Parent(child)) => {
            waitpid(child).expect("waitpid failed");
        }
        Ok(Fork::Child) => {
            // Close fds - should succeed normally
            let result = close_fd();
            assert!(result.is_ok(), "close_fd should succeed normally");

            // Second call might fail since fds are already closed
            // but this is implementation-dependent
            let result2 = close_fd();

            // Either succeeds (idempotent) or fails - both are acceptable
            match result2 {
                Ok(()) => {
                    // Idempotent - fine
                }
                Err(e) => {
                    // Failed - verify it's a proper io::Error
                    assert!(e.raw_os_error().is_some(), "Should have errno");
                }
            }

            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_error_kind_matching() {
    // Tests that io::Error kinds can be matched for specific handling
    // Expected behavior:
    // 1. Errors return standard io::ErrorKind variants
    // 2. Can pattern match on error kinds
    // 3. Demonstrates error handling patterns
    match fork() {
        Ok(Fork::Parent(_)) => {
            // Try to wait on invalid PID
            let result = waitpid(999_999);

            if let Err(e) = result {
                // Can match on error kind for different handling
                if e.kind() == std::io::ErrorKind::NotFound {
                    // ECHILD can map to NotFound on some systems
                }

                // Verify we can access errno
                assert!(e.raw_os_error().is_some(), "Should have raw OS error code");
            }
        }
        Ok(Fork::Child) => exit(0),
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_fork_child_pid_method() {
    // Tests Fork::child_pid() method returns correct PID
    match fork() {
        Ok(Fork::Parent(child_pid)) => {
            let fork_result = Fork::Parent(child_pid);

            // Test child_pid() method
            let extracted_pid = fork_result.child_pid();
            assert!(extracted_pid.is_some(), "Parent should have child PID");
            assert_eq!(
                extracted_pid.unwrap(),
                child_pid,
                "child_pid() should match fork result"
            );

            waitpid(child_pid).expect("waitpid failed");
        }
        Ok(Fork::Child) => {
            let fork_result = Fork::Child;

            // Test child_pid() method
            let extracted_pid = fork_result.child_pid();
            assert!(extracted_pid.is_none(), "Child should not have child PID");

            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_fork_is_parent_is_child_methods() {
    // Tests Fork::is_parent() and Fork::is_child() methods
    match fork() {
        Ok(result) => {
            if result.is_parent() {
                // In parent
                assert!(!result.is_child(), "Parent should not be child");
                assert!(result.child_pid().is_some(), "Parent should have child PID");

                let child_pid = result.child_pid().unwrap();
                waitpid(child_pid).expect("waitpid failed");
            } else if result.is_child() {
                // In child
                assert!(!result.is_parent(), "Child should not be parent");
                assert!(
                    result.child_pid().is_none(),
                    "Child should not have child PID"
                );
                exit(0);
            }
        }
        Err(_) => panic!("Fork failed"),
    }
}
