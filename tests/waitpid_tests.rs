//! Comprehensive `waitpid()` tests
//!
//! This module tests all aspects of the `waitpid()` function including:
//! - Error handling (ECHILD for invalid/already-waited PIDs)
//! - Exit status codes (various exit codes)
//! - Signal termination (WIFSIGNALED)
//! - Status code inspection (WIFEXITED, WEXITSTATUS, WTERMSIG)
//! - Double-wait scenarios
//!
//! These tests ensure waitpid correctly handles both success and error cases.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::match_wild_err_arm)]
#![allow(clippy::similar_names)]
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::indexing_slicing)]

use std::process::exit;

use fork::{Fork, fork, waitpid, waitpid_nohang};
use libc::{WEXITSTATUS, WIFEXITED, WIFSIGNALED, WTERMSIG};
use std::time::Duration;

#[test]
fn test_waitpid_invalid_pid() {
    // Tests that waitpid returns error for non-existent PID
    // Expected behavior:
    // 1. Try to wait on a PID that doesn't exist
    // 2. waitpid should fail with ECHILD error
    // 3. Verifies error handling for invalid PIDs
    match fork() {
        Ok(Fork::Parent(_)) => {
            // Try to wait on non-existent PID (very high number unlikely to exist)
            let result = waitpid(999_999);
            assert!(result.is_err(), "waitpid on invalid PID should fail");

            // Should be ECHILD (no child process)
            let err = result.unwrap_err();
            assert_eq!(
                err.raw_os_error(),
                Some(libc::ECHILD),
                "Should return ECHILD for non-existent child"
            );
        }
        Ok(Fork::Child) => exit(0),
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_waitpid_double_wait() {
    // Tests that waitpid fails when called twice on same child
    // Expected behavior:
    // 1. First waitpid succeeds and reaps the child
    // 2. Second waitpid on same PID fails with ECHILD
    // 3. Demonstrates that child can only be waited once
    match fork() {
        Ok(Fork::Parent(child)) => {
            // First wait succeeds
            let result = waitpid(child);
            assert!(result.is_ok(), "First waitpid should succeed");

            // Give child time to fully exit
            std::thread::sleep(std::time::Duration::from_millis(10));

            // Second wait should fail with ECHILD (child already reaped)
            let result = waitpid(child);
            assert!(result.is_err(), "Second waitpid should fail");
            assert_eq!(
                result.unwrap_err().raw_os_error(),
                Some(libc::ECHILD),
                "Should return ECHILD for already-waited child"
            );
        }
        Ok(Fork::Child) => exit(0),
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_waitpid_exit_code_zero() {
    // Tests that waitpid correctly reports exit code 0 (success)
    // Expected behavior:
    // 1. Child exits with code 0
    // 2. Parent calls waitpid and gets status
    // 3. WIFEXITED(status) is true
    // 4. WEXITSTATUS(status) returns 0
    match fork() {
        Ok(Fork::Parent(child)) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status), "Child should have exited normally");
            assert_eq!(WEXITSTATUS(status), 0, "Exit code should be 0");
        }
        Ok(Fork::Child) => exit(0),
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_waitpid_exit_code_one() {
    // Tests that waitpid correctly reports exit code 1 (error)
    match fork() {
        Ok(Fork::Parent(child)) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status), "Child should have exited normally");
            assert_eq!(WEXITSTATUS(status), 1, "Exit code should be 1");
        }
        Ok(Fork::Child) => exit(1),
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_waitpid_exit_code_42() {
    // Tests that waitpid correctly reports arbitrary exit code 42
    match fork() {
        Ok(Fork::Parent(child)) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status), "Child should have exited normally");
            assert_eq!(WEXITSTATUS(status), 42, "Exit code should be 42");
        }
        Ok(Fork::Child) => exit(42),
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_waitpid_exit_code_127() {
    // Tests that waitpid correctly reports exit code 127
    // (commonly used for "command not found")
    match fork() {
        Ok(Fork::Parent(child)) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status), "Child should have exited normally");
            assert_eq!(WEXITSTATUS(status), 127, "Exit code should be 127");
        }
        Ok(Fork::Child) => exit(127),
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_waitpid_multiple_exit_codes() {
    // Tests that waitpid correctly reports various exit codes
    // Tests multiple children with different exit codes sequentially
    for exit_code in [0, 1, 2, 42, 100, 127, 255] {
        match fork() {
            Ok(Fork::Parent(child)) => {
                let status = waitpid(child).expect("waitpid failed");
                assert!(WIFEXITED(status), "Child should have exited normally");
                assert_eq!(
                    WEXITSTATUS(status),
                    exit_code,
                    "Exit code should be {}",
                    exit_code
                );
            }
            Ok(Fork::Child) => exit(exit_code),
            Err(_) => panic!("Fork failed for exit code {}", exit_code),
        }
    }
}

#[test]
fn test_waitpid_signal_termination_sigkill() {
    // Tests that waitpid correctly reports signal termination
    // Expected behavior:
    // 1. Child kills itself with SIGKILL
    // 2. waitpid succeeds and returns status
    // 3. WIFSIGNALED(status) is true
    // 4. WTERMSIG(status) returns SIGKILL
    match fork() {
        Ok(Fork::Parent(child)) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(
                WIFSIGNALED(status),
                "Child should have been terminated by signal"
            );
            assert_eq!(WTERMSIG(status), libc::SIGKILL, "Signal should be SIGKILL");
        }
        Ok(Fork::Child) => {
            // Kill ourselves with SIGKILL
            unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
            }
            // Never reached
            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_waitpid_signal_termination_sigterm() {
    // Tests that waitpid correctly reports SIGTERM termination
    match fork() {
        Ok(Fork::Parent(child)) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(
                WIFSIGNALED(status),
                "Child should have been terminated by signal"
            );
            assert_eq!(WTERMSIG(status), libc::SIGTERM, "Signal should be SIGTERM");
        }
        Ok(Fork::Child) => {
            // Kill ourselves with SIGTERM
            unsafe {
                libc::kill(libc::getpid(), libc::SIGTERM);
            }
            // Never reached
            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_waitpid_signal_termination_sigabrt() {
    // Tests that waitpid correctly reports SIGABRT termination (abort)
    match fork() {
        Ok(Fork::Parent(child)) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(
                WIFSIGNALED(status),
                "Child should have been terminated by signal"
            );
            assert_eq!(WTERMSIG(status), libc::SIGABRT, "Signal should be SIGABRT");
        }
        Ok(Fork::Child) => {
            // Abort
            unsafe {
                libc::abort();
            }
            // Never reached
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_waitpid_distinguishes_exit_vs_signal() {
    // Tests that waitpid can distinguish between normal exit and signal termination
    // Expected behavior:
    // 1. First child exits normally with code 9
    // 2. Second child is killed with signal 9 (SIGKILL)
    // 3. waitpid should correctly identify each case

    // Normal exit with code 9
    match fork() {
        Ok(Fork::Parent(child)) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status), "First child should have exited normally");
            assert!(!WIFSIGNALED(status), "First child should not be signaled");
            assert_eq!(WEXITSTATUS(status), 9, "Exit code should be 9");
        }
        Ok(Fork::Child) => exit(9),
        Err(_) => panic!("Fork failed"),
    }

    // Signal termination with signal 9 (SIGKILL)
    match fork() {
        Ok(Fork::Parent(child)) => {
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFSIGNALED(status), "Second child should be signaled");
            assert!(
                !WIFEXITED(status),
                "Second child should not have exited normally"
            );
            assert_eq!(
                WTERMSIG(status),
                libc::SIGKILL,
                "Signal should be SIGKILL (9)"
            );
        }
        Ok(Fork::Child) => {
            unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
            }
            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_waitpid_returns_raw_status() {
    // Tests that waitpid returns the raw status code that can be inspected
    // Expected behavior:
    // 1. waitpid returns io::Result<c_int> (raw status)
    // 2. Status can be examined with WIFEXITED, WEXITSTATUS, etc.
    // 3. Verifies the function signature change from v0.5.0
    match fork() {
        Ok(Fork::Parent(child)) => {
            let status: libc::c_int = waitpid(child).expect("waitpid failed");

            // Verify we can use the raw status with libc macros
            assert!(WIFEXITED(status));
            let exit_code = WEXITSTATUS(status);
            assert_eq!(exit_code, 123);
        }
        Ok(Fork::Child) => exit(123),
        Err(_) => panic!("Fork failed"),
    }
}

// ============================================================================
// waitpid_nohang() tests
// ============================================================================

#[test]
fn test_waitpid_nohang_child_still_running() {
    // Tests that waitpid_nohang returns None when child is still running
    // Expected behavior:
    // 1. Child sleeps for a while
    // 2. Parent checks immediately with waitpid_nohang
    // 3. Should return Ok(None) because child hasn't exited yet
    match fork() {
        Ok(Fork::Parent(child)) => {
            // Check immediately - child should still be running
            match waitpid_nohang(child) {
                Ok(None) => {
                    // Expected: child still running
                }
                Ok(Some(status)) => {
                    panic!("Child exited too quickly with status: {}", status);
                }
                Err(e) => {
                    panic!("waitpid_nohang failed: {}", e);
                }
            }

            // Now wait for child to finish
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status));
        }
        Ok(Fork::Child) => {
            // Child sleeps to ensure parent's check happens while we're running
            std::thread::sleep(Duration::from_millis(100));
            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_waitpid_nohang_child_exited() {
    // Tests that waitpid_nohang returns Some(status) when child has exited
    // Expected behavior:
    // 1. Child exits immediately
    // 2. Parent waits a bit to ensure child exits
    // 3. waitpid_nohang should return Ok(Some(status))
    match fork() {
        Ok(Fork::Parent(child)) => {
            // Give child time to exit
            std::thread::sleep(Duration::from_millis(50));

            // Check if child exited
            match waitpid_nohang(child) {
                Ok(Some(status)) => {
                    assert!(WIFEXITED(status), "Child should have exited normally");
                    assert_eq!(WEXITSTATUS(status), 42);
                }
                Ok(None) => {
                    panic!("Child should have exited by now");
                }
                Err(e) => {
                    panic!("waitpid_nohang failed: {}", e);
                }
            }
        }
        Ok(Fork::Child) => {
            // Child exits immediately
            exit(42);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_waitpid_nohang_poll_until_exit() {
    // Tests polling pattern with waitpid_nohang
    // Expected behavior:
    // 1. Parent polls child status in a loop
    // 2. Returns None while child is running
    // 3. Eventually returns Some(status) when child exits
    match fork() {
        Ok(Fork::Parent(child)) => {
            let mut iterations = 0;
            let mut child_exited = false;

            // Poll for child exit
            for _ in 0..20 {
                iterations += 1;

                match waitpid_nohang(child) {
                    Ok(Some(status)) => {
                        assert!(WIFEXITED(status));
                        assert_eq!(WEXITSTATUS(status), 0);
                        child_exited = true;
                        break;
                    }
                    Ok(None) => {
                        // Child still running, continue polling
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(e) => {
                        panic!("waitpid_nohang failed: {}", e);
                    }
                }
            }

            assert!(child_exited, "Child should have exited");
            assert!(
                iterations > 1,
                "Should have polled at least twice (child was running)"
            );
        }
        Ok(Fork::Child) => {
            // Child runs for a bit then exits
            std::thread::sleep(Duration::from_millis(150));
            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_waitpid_nohang_invalid_pid() {
    // Tests that waitpid_nohang returns error for invalid PID
    // Expected behavior:
    // 1. Call waitpid_nohang with non-existent PID
    // 2. Should return Err with ECHILD
    match fork() {
        Ok(Fork::Parent(_)) => {
            let result = waitpid_nohang(999_999);
            assert!(result.is_err(), "Should fail for invalid PID");

            let err = result.unwrap_err();
            assert_eq!(
                err.raw_os_error(),
                Some(libc::ECHILD),
                "Should return ECHILD for non-existent child"
            );
        }
        Ok(Fork::Child) => exit(0),
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_waitpid_nohang_multiple_children() {
    // Tests checking multiple children with waitpid_nohang
    // Expected behavior:
    // 1. Create 3 children that exit at different times
    // 2. Poll all children without blocking
    // 3. Should be able to detect each child's exit independently
    let mut children = vec![];

    // Create 3 children
    for i in 0_u32..3 {
        match fork() {
            Ok(Fork::Parent(child)) => {
                children.push(child);
            }
            Ok(Fork::Child) => {
                // Each child sleeps for a different duration
                std::thread::sleep(Duration::from_millis(50 * u64::from(i + 1)));
                exit(i.try_into().unwrap());
            }
            Err(_) => panic!("Fork {} failed", i),
        }
    }

    // Poll children
    let mut exited = [false; 3];
    let mut all_exited = false;

    for _ in 0..30 {
        let mut count = 0;

        for (idx, &pid) in children.iter().enumerate() {
            if exited[idx] {
                count += 1;
                continue;
            }

            match waitpid_nohang(pid) {
                Ok(Some(status)) => {
                    assert!(WIFEXITED(status));
                    exited[idx] = true;
                    count += 1;
                }
                Ok(None) => {
                    // Still running
                }
                Err(e) => {
                    panic!("waitpid_nohang failed for child {}: {}", pid, e);
                }
            }
        }

        if count == 3 {
            all_exited = true;
            break;
        }

        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(all_exited, "All children should have exited");
}

#[test]
fn test_waitpid_nohang_returns_option() {
    // Tests the return type of waitpid_nohang is Option<c_int>
    // Expected behavior:
    // 1. Verify type signature
    // 2. Verify we can pattern match on Option
    match fork() {
        Ok(Fork::Parent(child)) => {
            // Type annotation to verify signature
            let result: std::io::Result<Option<libc::c_int>> = waitpid_nohang(child);

            match result {
                Ok(Some(_status)) => {
                    // Child exited quickly
                }
                Ok(None) => {
                    // Child still running - wait for it
                    waitpid(child).expect("waitpid failed");
                }
                Err(e) => {
                    panic!("Unexpected error: {}", e);
                }
            }
        }
        Ok(Fork::Child) => exit(0),
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_waitpid_nohang_vs_blocking() {
    // Tests the difference between waitpid and waitpid_nohang
    // Expected behavior:
    // 1. waitpid_nohang returns immediately
    // 2. waitpid blocks until child exits
    match fork() {
        Ok(Fork::Parent(child)) => {
            use std::time::Instant;

            // Non-blocking check should return immediately
            let start = Instant::now();
            match waitpid_nohang(child) {
                Ok(None) => {
                    // Expected: child still running
                    let elapsed = start.elapsed();
                    assert!(
                        elapsed < Duration::from_millis(50),
                        "waitpid_nohang should return immediately, took {:?}",
                        elapsed
                    );
                }
                Ok(Some(_)) => {
                    panic!("Child exited too quickly");
                }
                Err(e) => {
                    panic!("waitpid_nohang failed: {}", e);
                }
            }

            // Now wait for child to finish (blocking)
            let status = waitpid(child).expect("waitpid failed");
            assert!(WIFEXITED(status));
        }
        Ok(Fork::Child) => {
            // Child sleeps to ensure parent's nohang check happens first
            std::thread::sleep(Duration::from_millis(100));
            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}
