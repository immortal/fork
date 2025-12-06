# Integration Tests

This directory contains integration tests for the `fork` library. These tests run in separate processes and can properly test functions like `daemon()` that call `exit()`.

## Overview

The integration tests are organized into eight files:
- **`daemon_tests.rs`** - Daemon functionality
- **`fork_tests.rs`** - Fork/waitpid functionality
- **`integration_tests.rs`** - Advanced patterns
- **`stdio_redirect_tests.rs`** - Stdio redirection and fd safety
- **`waitpid_tests.rs`** - Exit codes, signals, error handling, and non-blocking waits
- **`error_handling_tests.rs`** - Error paths and type verification
- **`pid_tests.rs`** - PID helper functions (getpid, getppid)
- **`status_macro_tests.rs`** - Status macro re-exports
- **`common/mod.rs`** - Shared test utilities

Comprehensive coverage of process management, daemon creation, stdio safety, fork patterns, exit status handling, non-blocking waits, PID helpers, status macros, and error scenarios.

## Test Files

### `daemon_tests.rs` - Daemon Functionality Tests

Tests the `daemon()` function with real process daemonization. Each test documents:
- What is being tested
- Expected behavior (numbered steps)
- Why it matters for daemon creation

Tests include:
- **test_daemon_creates_detached_process** - Verifies daemon process creation and PID management
- **test_daemon_with_nochdir** - Tests `nochdir` option preserves current directory
- **test_daemon_process_group** - Verifies daemon process group structure (double-fork pattern)
- **test_daemon_with_command_execution** - Tests command execution in daemon context
- **test_daemon_no_controlling_terminal** - Verifies daemon has no controlling terminal

### `fork_tests.rs` - Fork Functionality Tests

Tests the core `fork()` and `waitpid()` functions. Each test explains the expected parent-child behavior.

Tests include:
- **test_fork_basic** - Basic fork/waitpid functionality and cleanup
- **test_fork_parent_child_communication** - File-based parent-child IPC pattern
- **test_fork_multiple_children** - Creating and managing multiple child processes
- **test_fork_child_inherits_environment** - Environment variable inheritance across fork
- **test_fork_child_can_execute_commands** - Command execution in child processes
- **test_fork_child_has_different_pid** - PID uniqueness between parent and child
- **test_waitpid_waits_for_child** - Proper parent-child synchronization

### `integration_tests.rs` - Advanced Pattern Tests

Tests complex usage patterns combining multiple operations. Documents real-world daemon scenarios.

Tests include:
- **test_double_fork_daemon_pattern** - Classic double-fork daemon creation (standard pattern)
- **test_setsid_creates_new_session** - Session management and session leader verification
- **test_chdir_changes_directory** - Directory changes in child processes
- **test_process_isolation** - File system isolation between parent/child (separate memory)
- **test_chdir_error_handling** - Ensures chdir propagates errors correctly
- **test_chdir_returns_io_error** - Verifies error types returned from chdir
- **test_getpgrp_returns_process_group** - Process group queries and verification

### `stdio_redirect_tests.rs` - Stdio Redirection Tests

Tests stdin/stdout/stderr safety and fd reuse protection.

Tests include:
- **test_redirect_stdio_prevents_fd_reuse** - Ensures `/dev/null` redirection blocks fd reuse
- **test_redirect_stdio_idempotent** - Multiple calls are safe
- **test_redirect_stdio_println_safety** - `println!` goes to `/dev/null` after redirect
- **test_daemon_uses_redirect_stdio** - Confirms `daemon()` uses redirect_stdio
- **test_redirect_stdio_error_handling** - Propagates errors from failed redirection
- **test_fd_reuse_corruption_scenario** - Demonstrates corruption risk when closing stdio
- **test_close_fd_allows_fd_reuse** - Shows fd reuse when stdio is closed (expected panic)

### `waitpid_tests.rs` - Waitpid Comprehensive Tests

Tests all aspects of the `waitpid()` and `waitpid_nohang()` functions including error handling, exit codes, signal termination, and non-blocking waits.

**Blocking waitpid() tests:**
- **test_waitpid_invalid_pid** - ECHILD error for non-existent PID
- **test_waitpid_double_wait** - ECHILD error for already-waited child
- **test_waitpid_exit_code_zero** - Exit code 0 handling
- **test_waitpid_exit_code_one** - Exit code 1 handling
- **test_waitpid_exit_code_42** - Arbitrary exit code handling
- **test_waitpid_exit_code_127** - Command not found exit code
- **test_waitpid_multiple_exit_codes** - Tests codes 0,1,2,42,100,127,255
- **test_waitpid_signal_termination_sigkill** - SIGKILL detection
- **test_waitpid_signal_termination_sigterm** - SIGTERM detection
- **test_waitpid_signal_termination_sigabrt** - SIGABRT (abort) detection
- **test_waitpid_distinguishes_exit_vs_signal** - WIFEXITED vs WIFSIGNALED
- **test_waitpid_returns_raw_status** - Raw status code return verification

**Non-blocking waitpid_nohang() tests:**
- **test_waitpid_nohang_child_still_running** - Returns None when child running
- **test_waitpid_nohang_child_exited** - Returns Some(status) when child exited
- **test_waitpid_nohang_poll_until_exit** - Polling pattern until child exits
- **test_waitpid_nohang_invalid_pid** - ECHILD error for non-existent PID
- **test_waitpid_nohang_multiple_children** - Poll multiple children without blocking
- **test_waitpid_nohang_returns_option** - Verify Option<c_int> return type
- **test_waitpid_nohang_vs_blocking** - Compare blocking vs non-blocking behavior

### `error_handling_tests.rs` - Error Path Tests

Tests error scenarios and type verification for all library functions.

Tests include:
- **test_setsid_error_when_already_session_leader** - EPERM when calling setsid twice
- **test_setsid_returns_io_error_type** - io::Error type and EPERM verification
- **test_fork_returns_io_error_type** - io::Result<Fork> type verification
- **test_waitpid_returns_io_error_type** - io::Result<c_int> type verification
- **test_getpgrp_returns_pid_type** - pid_t type verification (getpgrp always succeeds per POSIX)
- **test_close_fd_error_handling** - close_fd error scenarios
- **test_error_kind_matching** - io::ErrorKind pattern matching
- **test_fork_child_pid_method** - Fork::child_pid() correctness
- **test_fork_is_parent_is_child_methods** - Fork::is_parent() and is_child()

### `common/mod.rs` - Shared Test Utilities

Provides reusable helper functions to reduce code duplication:
- `get_unique_test_dir()` - Creates unique test directories with atomic counter
- `get_test_dir()` - Creates simple test directories
- `setup_test_dir()` - Sets up and cleans test directory
- `wait_for_file()` - Waits for file creation with timeout
- `cleanup_test_dir()` - Removes test directory

### `pid_tests.rs` - PID Helper Function Tests

Tests the convenience wrapper functions for getting process IDs without requiring unsafe code.

Tests include:
- **test_getpid_returns_valid_pid** - Verifies `getpid()` returns valid positive PID
- **test_getppid_returns_valid_pid** - Verifies `getppid()` returns valid parent PID
- **test_getpid_different_in_child** - Confirms child has different PID from parent
- **test_getpid_matches_fork_result** - Verifies `getpid()` matches fork's returned child PID
- **test_getppid_returns_parent_pid** - Confirms child's parent PID matches parent's PID
- **test_getpid_no_unsafe_in_user_code** - Proves user can call without unsafe block
- **test_getppid_no_unsafe_in_user_code** - Proves parent PID getter hides unsafe
- **test_pid_functions_in_multiple_forks** - Tests PID functions with multiple children
- **test_getpid_consistency_across_operations** - Verifies PID stability during lifetime
- **test_getppid_after_parent_exits** - Tests orphan reparenting to init (PID 1)

### `status_macro_tests.rs` - Status Macro Re-export Tests

Tests that status inspection macros can be imported from `fork` crate instead of requiring `libc`.

Tests include:
- **test_wifexited_macro_works** - Verifies `WIFEXITED` can be imported from fork
- **test_wexitstatus_macro_works** - Verifies `WEXITSTATUS` can be imported from fork
- **test_wifsignaled_macro_works** - Verifies `WIFSIGNALED` can be imported from fork
- **test_wtermsig_macro_works** - Verifies `WTERMSIG` can be imported from fork
- **test_all_macros_together** - Tests using all macros together for status inspection
- **test_macros_with_multiple_exit_codes** - Tests macros work with various exit codes (0, 1, 42, 127, 255)
- **test_macros_distinguish_exit_vs_signal** - Confirms macros correctly identify exit vs signal termination
- **test_no_libc_import_needed** - Proves users don't need `libc` import for status macros

## Running Tests

```bash
# Run all tests (unit + integration + doc)
cargo test

# For more stable process-based tests (avoid rare flakiness), run serially
RUST_TEST_THREADS=1 cargo test

# Run only integration tests
cargo test --tests

# Run specific test file
cargo test --test daemon_tests
cargo test --test fork_tests
cargo test --test integration_tests

# Run specific test
cargo test --test daemon_tests test_daemon_creates_detached_process

# Run with output
cargo test --test daemon_tests -- --nocapture

# Run with verbose output
cargo test --test fork_tests -- --nocapture --test-threads=1
```

## How Integration Tests Work

Unlike unit tests in `src/lib.rs`, integration tests:

1. **Run in separate processes** - Each test file is compiled as its own binary
2. **Can call `daemon()`** - The parent process in tests doesn't terminate the test runner
3. **Use file-based communication** - Temporary files in `/tmp` for parent-child verification
4. **Have proper isolation** - Each test uses unique temporary directories to avoid conflicts
5. **Clean up after themselves** - Temporary files are removed after test completion
6. **Document expected behavior** - Each test has detailed comments explaining what happens

## Test Documentation

Every test includes comprehensive documentation:

```rust
#[test]
fn test_name() {
    // Clear description of what is being tested
    // Expected behavior:
    // 1. First step
    // 2. Second step
    // 3. Third step
    // 4. Fourth step
    // 5. Final verification

    [test implementation]
}
```

This makes it easy to:
- Understand test purpose at a glance
- Debug failures quickly
- Use tests as usage examples
- Onboard new contributors

## Test Isolation

Each test uses a unique temporary directory to prevent conflicts when running in parallel:

```rust
// Daemon tests use atomic counter for uniqueness
let test_dir = setup_test_dir(get_unique_test_dir("daemon_creates_detached"));

// Fork tests use descriptive prefixes
let test_dir = setup_test_dir(get_test_dir("fork_communication"));

// Integration tests use specific names
let test_dir = setup_test_dir(get_test_dir("int_double_fork"));
```

This allows tests to run in parallel without interfering with each other.

## Coverage

Integration tests provide coverage for:

- **Daemon creation** - Real process daemonization (not mocked)
- **Process groups** - Session management and process group creation
- **File descriptors** - Proper handling of stdin/stdout/stderr
- **IPC patterns** - Parent-child communication via files
- **Command execution** - Running commands in forked/daemon processes
- **Environment inheritance** - Variable passing across fork
- **Process isolation** - Memory separation and filesystem sharing
- **Double-fork pattern** - Standard daemon creation technique
- **PID management** - Process ID tracking and verification
- **Exit status handling** - Exit codes (0-255) and status inspection
- **Signal termination** - SIGKILL, SIGTERM, SIGABRT detection
- **Error scenarios** - ECHILD, EPERM, and invalid input handling
- **Type safety** - io::Error verification and error kind matching
- **Fork helper methods** - is_parent(), is_child(), child_pid()


## Module Structure

```
tests/
├── common/
│   └── mod.rs               # Shared utilities (51 lines)
├── daemon_tests.rs          # Daemon tests (271 lines, 5 tests)
├── fork_tests.rs            # Fork tests (301 lines, 7 tests)
├── integration_tests.rs     # Advanced tests (284 lines, 7 tests)
├── stdio_redirect_tests.rs  # Stdio safety tests (313 lines, 7 tests)
├── waitpid_tests.rs         # Waitpid tests (591 lines, 19 tests)
├── error_handling_tests.rs  # Error tests (260 lines, 9 tests)
└── README.md                # This file
```
