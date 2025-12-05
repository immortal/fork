//! Tests for status macro re-exports
//!
//! This module tests that users can import status inspection macros
//! directly from the fork crate instead of requiring libc:
//! - `WIFEXITED` - Check if child exited normally
//! - `WEXITSTATUS` - Get exit code
//! - `WIFSIGNALED` - Check if child was terminated by signal
//! - `WTERMSIG` - Get terminating signal
//!
//! These tests verify the convenience re-exports work correctly.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::match_wild_err_arm)]

use std::process::exit;

// Import macros from fork crate (not libc)
use fork::{Fork, WEXITSTATUS, WIFEXITED, WIFSIGNALED, WTERMSIG, fork, waitpid};

#[test]
fn test_wifexited_macro_works() {
    // Tests that WIFEXITED macro can be imported from fork crate
    // Expected behavior:
    // 1. Child exits normally with code 0
    // 2. Parent uses WIFEXITED to check normal exit
    // 3. WIFEXITED returns true
    match fork() {
        Ok(Fork::Parent(child_pid)) => {
            let status = waitpid(child_pid).expect("waitpid failed");

            // This macro is re-exported from fork, not libc
            assert!(WIFEXITED(status), "Child should exit normally");
        }
        Ok(Fork::Child) => {
            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_wexitstatus_macro_works() {
    // Tests that WEXITSTATUS macro can be imported from fork crate
    // Expected behavior:
    // 1. Child exits with code 42
    // 2. Parent uses WEXITSTATUS to get exit code
    // 3. WEXITSTATUS returns 42
    match fork() {
        Ok(Fork::Parent(child_pid)) => {
            let status = waitpid(child_pid).expect("waitpid failed");

            assert!(WIFEXITED(status), "Child should exit normally");

            // This macro is re-exported from fork, not libc
            let exit_code = WEXITSTATUS(status);
            assert_eq!(exit_code, 42, "Exit code should be 42");
        }
        Ok(Fork::Child) => {
            exit(42);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_wifsignaled_macro_works() {
    // Tests that WIFSIGNALED macro can be imported from fork crate
    // Expected behavior:
    // 1. Child kills itself with SIGKILL
    // 2. Parent uses WIFSIGNALED to check signal termination
    // 3. WIFSIGNALED returns true
    match fork() {
        Ok(Fork::Parent(child_pid)) => {
            let status = waitpid(child_pid).expect("waitpid failed");

            // This macro is re-exported from fork, not libc
            assert!(WIFSIGNALED(status), "Child should be terminated by signal");
        }
        Ok(Fork::Child) => {
            unsafe {
                libc::raise(libc::SIGKILL);
            }
            exit(0); // Never reached
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_wtermsig_macro_works() {
    // Tests that WTERMSIG macro can be imported from fork crate
    // Expected behavior:
    // 1. Child kills itself with SIGTERM
    // 2. Parent uses WTERMSIG to get signal number
    // 3. WTERMSIG returns SIGTERM
    match fork() {
        Ok(Fork::Parent(child_pid)) => {
            let status = waitpid(child_pid).expect("waitpid failed");

            assert!(WIFSIGNALED(status), "Child should be terminated by signal");

            // This macro is re-exported from fork, not libc
            let signal = WTERMSIG(status);
            assert_eq!(signal, libc::SIGTERM, "Signal should be SIGTERM");
        }
        Ok(Fork::Child) => {
            unsafe {
                libc::raise(libc::SIGTERM);
            }
            exit(0); // Never reached
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_all_macros_together() {
    // Tests using all re-exported macros together
    // Expected behavior:
    // 1. Child exits with code 7
    // 2. Parent uses all macros to inspect status
    // 3. WIFEXITED true, WIFSIGNALED false
    // 4. WEXITSTATUS returns 7
    match fork() {
        Ok(Fork::Parent(child_pid)) => {
            let status = waitpid(child_pid).expect("waitpid failed");

            // All these macros are re-exported from fork
            if WIFEXITED(status) {
                let code = WEXITSTATUS(status);
                assert_eq!(code, 7, "Exit code should be 7");
            } else if WIFSIGNALED(status) {
                let signal = WTERMSIG(status);
                panic!("Child unexpectedly terminated by signal {signal}");
            } else {
                panic!("Child in unexpected state");
            }
        }
        Ok(Fork::Child) => {
            exit(7);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_macros_with_multiple_exit_codes() {
    // Tests macros work with various exit codes
    // Expected behavior:
    // 1. Create children with different exit codes
    // 2. Parent uses macros to verify each code
    // 3. All codes match expected values
    let exit_codes = [0, 1, 42, 127, 255];

    for expected_code in exit_codes {
        match fork() {
            Ok(Fork::Parent(child_pid)) => {
                let status = waitpid(child_pid).expect("waitpid failed");

                assert!(
                    WIFEXITED(status),
                    "Child should exit normally with code {expected_code}"
                );

                let actual_code = WEXITSTATUS(status);
                assert_eq!(
                    actual_code, expected_code,
                    "Exit code should be {expected_code}"
                );
            }
            Ok(Fork::Child) => {
                exit(expected_code);
            }
            Err(_) => panic!("Fork failed"),
        }
    }
}

#[test]
fn test_macros_distinguish_exit_vs_signal() {
    // Tests macros can distinguish normal exit from signal termination
    // Expected behavior:
    // 1. First child exits normally
    // 2. Second child is killed by signal
    // 3. Macros correctly identify each case

    // Test normal exit
    match fork() {
        Ok(Fork::Parent(child_pid)) => {
            let status = waitpid(child_pid).expect("waitpid failed");

            assert!(WIFEXITED(status), "First child should exit normally");
            assert!(!WIFSIGNALED(status), "First child should not be signaled");
            assert_eq!(WEXITSTATUS(status), 0, "Exit code should be 0");
        }
        Ok(Fork::Child) => {
            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }

    // Test signal termination
    match fork() {
        Ok(Fork::Parent(child_pid)) => {
            let status = waitpid(child_pid).expect("waitpid failed");

            assert!(
                WIFSIGNALED(status),
                "Second child should be terminated by signal"
            );
            assert!(!WIFEXITED(status), "Second child should not exit normally");
            assert_eq!(WTERMSIG(status), libc::SIGABRT, "Signal should be SIGABRT");
        }
        Ok(Fork::Child) => {
            unsafe {
                libc::raise(libc::SIGABRT);
            }
            exit(0); // Never reached
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_no_libc_import_needed() {
    // Tests that users don't need to import libc for status macros
    // Expected behavior:
    // 1. This test file only imports from fork, not libc
    // 2. All status macros work correctly
    // 3. Code compiles without libc import (except for signals)

    // This is a compile-time test - if it compiles, it passes!
    // The fact that we can use WIFEXITED, WEXITSTATUS, WIFSIGNALED, WTERMSIG
    // without importing libc proves the re-exports work.

    match fork() {
        Ok(Fork::Parent(child_pid)) => {
            let status = waitpid(child_pid).expect("waitpid failed");

            // No libc:: prefix needed!
            assert!(WIFEXITED(status));
            assert_eq!(WEXITSTATUS(status), 13);
        }
        Ok(Fork::Child) => {
            exit(13);
        }
        Err(_) => panic!("Fork failed"),
    }
}
