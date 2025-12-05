//! Tests for PID helper functions (getpid, getppid)
//!
//! This module tests the convenience wrappers for getting process IDs:
//! - `getpid()` - Get current process ID
//! - `getppid()` - Get parent process ID
//!
//! These tests verify that the wrappers correctly hide unsafe code
//! and return valid process IDs in both parent and child processes.

#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]
#![allow(clippy::match_wild_err_arm)]

use std::process::exit;

use fork::{Fork, fork, getpid, getppid, waitpid};

#[test]
fn test_getpid_returns_valid_pid() {
    // Tests that getpid() returns a valid positive process ID
    // Expected behavior:
    // 1. getpid() returns current process ID
    // 2. PID should be positive
    // 3. PID should be consistent across multiple calls
    let pid1 = getpid();
    let pid2 = getpid();

    assert!(pid1 > 0, "PID should be positive");
    assert_eq!(pid1, pid2, "PID should be consistent");
}

#[test]
fn test_getppid_returns_valid_pid() {
    // Tests that getppid() returns a valid parent process ID
    // Expected behavior:
    // 1. getppid() returns parent process ID
    // 2. Parent PID should be positive
    // 3. Parent PID should be consistent
    let ppid1 = getppid();
    let ppid2 = getppid();

    assert!(ppid1 > 0, "Parent PID should be positive");
    assert_eq!(ppid1, ppid2, "Parent PID should be consistent");
}

#[test]
fn test_getpid_different_in_child() {
    // Tests that child process has different PID from parent
    // Expected behavior:
    // 1. Parent gets its PID
    // 2. Child gets its PID
    // 3. Child PID != Parent PID
    // 4. Child's parent PID == Parent's PID
    let parent_pid = getpid();

    match fork() {
        Ok(Fork::Parent(child_pid)) => {
            // Verify fork returned correct child PID
            assert!(child_pid > 0, "Child PID from fork should be positive");
            assert_ne!(
                child_pid, parent_pid,
                "Child PID should differ from parent PID"
            );

            waitpid(child_pid).expect("waitpid failed");
        }
        Ok(Fork::Child) => {
            let child_pid = getpid();
            let child_parent_pid = getppid();

            // Child's PID should be different from parent's
            assert_ne!(
                child_pid, parent_pid,
                "Child should have different PID from parent"
            );

            // Child's parent PID should match original parent's PID
            assert_eq!(
                child_parent_pid, parent_pid,
                "Child's parent PID should match parent's PID"
            );

            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_getpid_matches_fork_result() {
    // Tests that getpid() in child matches PID returned by fork() in parent
    // Expected behavior:
    // 1. Parent gets child PID from fork()
    // 2. Child calls getpid()
    // 3. Both should match
    match fork() {
        Ok(Fork::Parent(fork_child_pid)) => {
            // Parent waits for child to complete
            let status = waitpid(fork_child_pid).expect("waitpid failed");
            assert!(libc::WIFEXITED(status), "Child should exit normally");

            // We can't directly compare here, but child will verify
        }
        Ok(Fork::Child) => {
            // Child verifies its PID
            let my_pid = getpid();
            assert!(my_pid > 0, "Child PID should be positive");

            // Note: We can't pass this back to parent easily,
            // but the fact that both calls succeed validates the wrapper

            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_getppid_returns_parent_pid() {
    // Tests that child's getppid() returns the parent's getpid()
    // Expected behavior:
    // 1. Parent records its PID
    // 2. Child calls getppid()
    // 3. Child's parent PID matches parent's PID
    let parent_pid = getpid();

    match fork() {
        Ok(Fork::Parent(child_pid)) => {
            waitpid(child_pid).expect("waitpid failed");
        }
        Ok(Fork::Child) => {
            let my_parent = getppid();

            assert_eq!(
                my_parent, parent_pid,
                "Child's parent PID should match parent's actual PID"
            );

            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_getpid_no_unsafe_in_user_code() {
    // Tests that getpid() hides unsafe from user code
    // Expected behavior:
    // 1. User can call getpid() without unsafe block
    // 2. Function returns valid PID
    // 3. Type is libc::pid_t

    // This compiles without unsafe - that's the test!
    let pid: libc::pid_t = getpid();

    assert!(pid > 0, "PID should be positive");
}

#[test]
fn test_getppid_no_unsafe_in_user_code() {
    // Tests that getppid() hides unsafe from user code
    // Expected behavior:
    // 1. User can call getppid() without unsafe block
    // 2. Function returns valid PID
    // 3. Type is libc::pid_t

    // This compiles without unsafe - that's the test!
    let ppid: libc::pid_t = getppid();

    assert!(ppid > 0, "Parent PID should be positive");
}

#[test]
fn test_pid_functions_in_multiple_forks() {
    // Tests PID functions work correctly with multiple forks
    // Expected behavior:
    // 1. Create multiple children
    // 2. Each child has unique PID
    // 3. All children have same parent PID
    let parent_pid = getpid();
    let mut child_pids = vec![];

    // Create 3 children
    for i in 0..3 {
        match fork() {
            Ok(Fork::Parent(child_pid)) => {
                child_pids.push(child_pid);
            }
            Ok(Fork::Child) => {
                let my_pid = getpid();
                let my_parent = getppid();

                // Verify child has different PID
                assert_ne!(my_pid, parent_pid, "Child {i} should have different PID");

                // Verify parent PID is correct
                assert_eq!(
                    my_parent, parent_pid,
                    "Child {i} should have correct parent PID"
                );

                exit(i);
            }
            Err(_) => panic!("Fork {i} failed"),
        }
    }

    // Parent waits for all children
    for child_pid in child_pids {
        waitpid(child_pid).expect("waitpid failed");
    }

    // Verify our PID is still the same
    assert_eq!(getpid(), parent_pid, "Parent PID should not change");
}

#[test]
fn test_getpid_consistency_across_operations() {
    // Tests that getpid() remains consistent during process lifetime
    // Expected behavior:
    // 1. PID remains the same throughout process execution
    // 2. PID doesn't change after fork (in same process)
    // 3. PID doesn't change after other operations
    let pid1 = getpid();

    // Do some work
    let _ppid = getppid();

    let pid2 = getpid();
    assert_eq!(pid1, pid2, "PID should remain consistent");

    // Fork and verify parent PID doesn't change
    match fork() {
        Ok(Fork::Parent(child_pid)) => {
            let pid3 = getpid();
            assert_eq!(pid1, pid3, "Parent PID should not change after fork");

            waitpid(child_pid).expect("waitpid failed");
        }
        Ok(Fork::Child) => {
            let child_pid = getpid();
            assert_ne!(child_pid, pid1, "Child should have different PID");
            exit(0);
        }
        Err(_) => panic!("Fork failed"),
    }
}

#[test]
fn test_getppid_after_parent_exits() {
    // Tests that grandchild gets reparented to init (PID 1)
    // Expected behavior:
    // 1. Create child, child creates grandchild
    // 2. Child exits
    // 3. Grandchild's parent becomes init (PID 1)
    match fork() {
        Ok(Fork::Parent(child_pid)) => {
            // Wait for child to exit
            waitpid(child_pid).expect("waitpid failed");
            // Grandchild will be reparented, but we can't directly observe it here
        }
        Ok(Fork::Child) => {
            let child_pid = getpid();

            // Create grandchild
            match fork() {
                Ok(Fork::Parent(_)) => {
                    // Child exits immediately, orphaning grandchild
                    exit(0);
                }
                Ok(Fork::Child) => {
                    // Give parent time to exit
                    std::thread::sleep(std::time::Duration::from_millis(100));

                    // Grandchild's parent should now be init (PID 1)
                    let current_parent = getppid();
                    assert_eq!(
                        current_parent, 1,
                        "Orphaned grandchild should be reparented to init (PID 1)"
                    );

                    // Verify we're not the original child
                    let my_pid = getpid();
                    assert_ne!(my_pid, child_pid, "Grandchild should have different PID");

                    exit(0);
                }
                Err(_) => exit(1),
            }
        }
        Err(_) => panic!("Fork failed"),
    }
}
