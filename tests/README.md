# Integration Tests

This directory contains integration tests for the `fork` library. These tests run in separate processes and can properly test functions like `daemon()` that call `exit()`.

## Overview

The integration contracts are organized by behavior:
- **`daemon_tests.rs`** - Daemon functionality
- **`fork_tests.rs`** - Fork/waitpid functionality
- **`integration_tests.rs`** - Advanced patterns
- **`stdio_redirect_tests.rs`** - Stdio redirection and fd safety
- **`waitpid_tests.rs`** - Exit codes, signals, error handling, and non-blocking waits
- **`error_handling_tests.rs`** - Error paths and type verification
- **`pid_tests.rs`** - PID helper functions (getpid, getppid)
- **`status_macro_tests.rs`** - Status macro re-exports
- **`chdir_tests.rs`** - Comprehensive chdir() function tests
- **`typed_process_tests.rs`** - Typed IDs, groups, signals, and child events
- **`fd_tests.rs`** - Owned close-on-exec pipe and socket-pair primitives
- **`prepared_command_tests.rs`** - Prepared fork/exec and descriptor contracts
- **`checked_daemon_tests.rs`** - Checked detachment and startup handshake
- **`group_guard_tests.rs`** - Process-group guard lifecycle and failure paths
- **`broker_owner_loss.rs`** - Harness-free forced broker-death containment
- **`api_compat_tests.rs`** - Published API behavior compatibility
- **`signal_constants_tests.rs`** - Portable signal-value contracts
- **`common/mod.rs`** - Shared test utilities

Comprehensive coverage of process management, daemon creation, stdio safety, fork patterns, exit status handling, non-blocking waits, PID helpers, status macros, directory operations, and error scenarios.

### `typed_process_tests.rs` - Typed Supervisor Primitives

Runs serially with bounded waits and cleanup guards. It covers checked fork
results, exact exit events, stop/continue observation, single-process and group
signal delivery, parent/child process-group setup, draining coalesced terminal
events, blocking wait retry after `EINTR`, and the process-group existence
probe: a live group, a group holding only an unreaped member, and refusal of
the broadcast group.

### `fd_tests.rs` - Broker IPC Primitives

Checks `FD_CLOEXEC`, one-way pipe data and EOF behavior, bidirectional socket
pair data flow, and ownership-based descriptor closure.

### `prepared_command_tests.rs` - Prepared Fork and Exec

Checks exact exits, typed and reaped exec failures, environment and working
directory materialization, stdout mapping, overlapping descriptor targets,
explicit inheritance and close-action conflicts, closure of unintended
non-CLOEXEC descriptors, numeric identity application, and new process-group
setup, descendant cleanup, and default signal-mask/disposition reset.

### `checked_daemon_tests.rs` - Checked Daemon Startup

Checks readiness only after daemon initialization, failures before and after
detachment, exact errno/stage reporting, dropped-notifier EOF, hard startup
timeouts, bounded group cleanup, repeated `EINTR`, directory changes, explicit
stdio redirection, and preserved descriptors. Every detached test process
exits through `_exit` and the original test process verifies group cleanup.

The private daemon regression `failed_intermediate_remains_unreaped_until_cleanup`
requires a nonzero-exit intermediate to remain waitable after the readiness
check fails, preserving its PID/group reservation until error cleanup signals.
The startup-timeout fixture also has an owner-held release pipe, so a missing
group kill still fails its assertion but fixture cleanup needs no stale-ID kill.

### `group_guard_tests.rs` - Fail-closed Process Groups

Runs the guard lifecycle serially with deadlines and cleanup ownership. It
covers running and stopped workloads, `TERM`/`STOP`/`CONT`/`KILL` isolation,
clean disarm, invalid timeouts, repeated activation, failed exec, empty-group
cleanup, unrelated and standard descriptor closure, dead and stopped helpers,
and deliberate session escape. It also proves that an armed guard keeps its
emptied group reserved through an anchor that is not the owner's child, that
disarm releases it, and that owner loss kills and then releases it. New signal
cases prove live delivery, no direct signal after externally reaped helper
loss, deadline expiry without delivery while the helper is stopped, skipping
a late reply after resume, timeout validation, and anchor-only `Ok`/`EPERM`.
Stopped-helper disarm resumes without touching the workload; stopped-helper
armed Drop resumes and kills it. Both also run after `waitpid(WUNTRACED)` has
consumed the stop event (not `WNOWAIT`), matching the broker's all-child drain.
Unit tests additionally cover partial reply framing across a timeout, stale
same-signal replies with older request IDs, request-ID exhaustion, EOF with a
half-closed peer that has not hung up, mutex deadline expiry, preserved auto
traits, a taken owner, and a full request socket that must not block disarm.

### `stopped_anchor.rs` - Pre-exit Anchor Stop

A GNU/Linux-only harness-free executable compiles its own temporary `LD_PRELOAD`
shim to stop the real anchor immediately after `setpgid(0, 0)`. Disarm must kill
and reap only that anchor. The test adopts and cleans any leaked stopped anchor
on failure and removes its temporary source and shared library. Requires `cc`.
Static targets (including default Linux/musl) cannot interpose `setpgid`, so this
fixture is a gated no-op there; the GNU run covers the scheduling regression.

### `descriptor_fallback.rs` - Lowered Descriptor Limits

A serial harness-free executable opens a non-CLOEXEC writer above a subsequently
lowered soft limit, then proves neither a guard helper nor a prepared exec child
retains it. Limits are restored before return. Linux repeats with a process-local
seccomp filter returning ENOSYS for `close_range`; macOS exercises the portable
fallback natively. Descriptor exhaustion proves guard/prepared spawn still
succeed without an inventory slot, using descriptor-free hard-limit probing.
A separate Linux sandbox denies directory opens and limits descriptors to a
finite hard ceiling; high descriptors above the soft limit must still close.
A unit test injects an iteration error so an incomplete directory cannot be
accepted as an inventory. A separate Linux child permits the no-op range probe but
rejects actual ranges; guard creation, prepared spawn and checked daemonization
must all fail closed.
Children are killed/reaped before assertions, including when
the regression fails.

### `sigchld_ignored.rs` - Waitable Owner Policy

A harness-free executable verifies that guard creation and prepared spawn
succeed under `SIGCHLD=SIG_IGN` and `SA_NOCLDWAIT`, without changing owner policy.
The helper's anchor stays pinned and `signal_group` actually ends a workload's
lifetime. Checked daemonization alone rejects that owner before fork. Cleanup
of a stopped helper refuses PID signals under auto-reaping. The fixture restores
policy and resumes/reaps that helper before asserting. A separate valid owner
handler would reap the helper's anchor if inherited there; the helper's reset
must preserve the pin until disarm. Dispositions are always restored.

### `waitable_children.rs` - Explicit Owner Policy Correction

This harness-free test covers default, ignored, and `SA_NOCLDWAIT` dispositions
(with default and custom handlers), plus a valid handler that must remain
unchanged. It checks read-only observation, both correction return values,
idempotence, handler masks/flags, daemon rejection before correction and successful
guard, prepared-command, and checked-daemon lifecycles afterwards. A unit test
covers the shared policy classifier independently of process-global state.

### `atfork_anchor.rs` - Fork Callbacks Outside the Helper

Runs without libtest, one fresh process per case, because `pthread_atfork`
registrations are permanent. Callbacks act only outside the owner: one reaps
the helper's children, the other restores SIGCHLD auto-reaping in the helper
and delays the anchor so it exits after startup. Guard creation must succeed,
the armed group must stay pinned past the delay, a helper-mediated `SIGKILL` or
owner loss must kill a real workload, and the owner's policy must be unchanged.
Each schedule ends once in disarm and once in owner loss. Creating the anchor
with `fork(3)` fails these cases: creation fails, or the pin
is released while the guard is armed. The cases use only portable APIs.

### `anchor_clone_policy.rs` - Sandboxed Anchor Creation

A Linux seccomp filter answers `EACCES` for every `clone` except the exact form
the linked C library uses for `fork`: glibc's `CHILD_SETTID | CHILD_CLEARTID |
SIGCHLD` with zero stack, parent-TID and TLS arguments (musl forks with
`SYS_fork`, so the filter denies `clone` entirely on x86-64). The fixture proves
that `fork` still works and deviating forms are refused, then requires guard
creation, a pinned group and disarm to succeed. A bare `clone(SIGCHLD)` anchor,
or one repeating the child TID pointer in the TLS slot, fails here with
`EACCES`, which the helper reports.

### `daemon_waitid_denied.rs` - Real Sandbox Denial

A Linux seccomp filter answers `EACCES` for `waitid` only. Checked
daemonization must still succeed through the exact-child `waitpid` fallback,
collect the finite daemon's exit zero, and drain to ECHILD. It needs no
dynamic interposition, so it also runs on musl.

### `owner_cleanup.rs` - Cleanup Policy and Lost Ownership

GNU/Linux-only preload schedules exercise stopped-helper cleanup under both
auto-reaping policies, a stopped pre-exec child reported rather than killed,
and a helper collected between the last idle wait and deadline delivery. The
oracle suppresses and fails any stale CONT/KILL attempt. Another schedule delays
a running helper's read while the owner has a SIGCONT callback: cleanup must not
invoke that inherited callback. A denied resume also must not fail a running
helper that finishes via its socket. These cases succeed on 0.10.0 and detect
the intermediate 0.11.0 regression before the helper's SIGCONT reset.
`inherited_reaper.rs` installs non-SIGCHLD callbacks (USR1 and URG) that collect
children only in the helper: delivery, disarm and owner loss must never signal
an unpinned ID, ignored USR2 stays ignored, and owner handlers remain unchanged.
`waitid_fallback.rs` denies waitid, preserves successful daemonization, rejects
an unexpectedly terminated intermediate without unsafe cleanup delivery, and
forces a terminal helper event into the fallback observation before CONT. Disarm
must receive the consumed exit rather than lose it and fail with ECHILD. Each
waitid schedule runs once per denial errno (ENOSYS, EPERM, EACCES, EINVAL and
EIO), because sandboxes and old kernels answer with different errnos;
`FORK_ONLY_CASE` and a numeric `FORK_ONLY_ERRNO` select one schedule and denial
for mutation verification. Commands, policy restoration, exact-child
cleanup and orphan adoption are bounded.

### `broker_owner_loss.rs` - Forced Broker Death

Runs without the multithreaded Rust test harness. It kills a single-threaded
broker with `SIGKILL` both before workload creation and after a guarded workload
starts. The parent proves that startup helpers disappear and that the workload
lifetime descriptor closes, so neither result can depend on broker destructors
or async cleanup.

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
- **test_daemon_never_returns_parent** - Verifies daemon() only returns `Fork::Child`, never `Fork::Parent`

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
- **test_close_once_ok_and_ebadf** - Unit-level check that close handles EBADF gracefully

### `waitpid_tests.rs` - Waitpid Comprehensive Tests

Tests all aspects of the `waitpid()`, `waitpid_nohang()`, `wait_any()`, and `wait_any_nohang()` functions including error handling, exit codes, signal termination, non-blocking waits, and any-child reaping.

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

**Any-child wait tests:**
- **test_wait_any_returns_reaped_pid_and_status** - Blocking wait for any child returns `(pid, status)`
- **test_wait_any_nohang_child_still_running** - Returns None when children exist but none has exited
- **test_wait_any_nohang_child_exited** - Returns the exited child's PID and status
- **test_wait_any_nohang_multiple_children_identifies_each_reaped_child** - Reaps multiple children and maps each result by PID
- **test_wait_any_nohang_echild_after_all_children_reaped** - ECHILD after all children are reaped

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

### `chdir_tests.rs` - Comprehensive chdir() Function Tests

Tests all aspects of the `chdir()` function including modern `c""` string literal implementation.

Tests include:
- **test_chdir_basic_success** - Verifies successful directory change to root
- **test_chdir_returns_unit** - Confirms return type is `io::Result<()>`
- **test_chdir_changes_actual_working_directory** - Validates real filesystem effects
- **test_chdir_idempotent** - Tests multiple successive calls are safe
- **test_chdir_process_isolation** - Verifies child chdir doesn't affect parent
- **test_chdir_with_file_operations** - Tests relative path operations after chdir
- **test_chdir_with_absolute_path_operations** - Confirms absolute paths still work
- **test_chdir_error_type** - Validates proper `io::Error` type on failure
- **test_chdir_concurrent_forks** - Tests with multiple concurrent child processes
- **test_chdir_before_and_after_setsid** - Integration with setsid (daemon pattern)
- **test_chdir_uses_c_string_literal** - Validates modern `c""` literal implementation (calls chdir 100 times)
- **test_chdir_with_env_manipulation** - Tests with modified environment variables

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
cargo test --test chdir_tests

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
3. **Use file-based communication** - Temporary files under the configured `TMPDIR` for parent-child verification
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

Filesystem tests use unique temporary directories. Native lifecycle CI also
sets `RUST_TEST_THREADS=1` because child ownership and all-child wait operations
must not race across tests:

```rust
// Daemon tests use atomic counter for uniqueness
let test_dir = setup_test_dir(get_unique_test_dir("daemon_creates_detached"));

// Fork tests use descriptive prefixes
let test_dir = setup_test_dir(get_test_dir("fork_communication"));

// Integration tests use specific names
let test_dir = setup_test_dir(get_test_dir("int_double_fork"));
```

The harness-free broker-death and subreaper contracts start no test-harness
threads before they fork.

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
- **Subreaper lifecycle** - State, fork/exec behavior, and orphan adoption


## Module Structure

```
tests/
├── common/
│   └── mod.rs               # Shared utilities
├── broker_owner_loss.rs     # Harness-free broker SIGKILL contract
├── subreaper.rs             # Harness-free orphan-adoption contract
├── group_guard_tests.rs     # Guard lifecycle and failure contracts
├── checked_daemon_tests.rs  # Checked daemon startup
├── prepared_command_tests.rs # Prepared fork/exec
├── typed_process_tests.rs   # Typed process lifecycle
├── fd_tests.rs              # Owned descriptor primitives
├── daemon_tests.rs          # Daemon behavior
├── fork_tests.rs            # Fork/waitpid behavior
├── integration_tests.rs     # Combined Unix patterns
├── stdio_redirect_tests.rs  # Stdio safety
├── waitpid_tests.rs         # Wait status behavior
├── error_handling_tests.rs  # Error contracts
├── pid_tests.rs             # PID helpers
├── status_macro_tests.rs    # Status re-exports
├── signal_constants_tests.rs # Portable signal constants
├── api_compat_tests.rs      # Published API behavior
├── chdir_tests.rs           # Working-directory behavior
└── README.md                # This file
```
