## 0.6.0

### Breaking Changes
* **`getpgrp()` signature changed** - Now returns `libc::pid_t` directly instead of `io::Result<libc::pid_t>`
  - `getpgrp()` always succeeds per POSIX specification and cannot fail
  - **Migration guide:**
    - Change `getpgrp()?` to `getpgrp()`
    - Change `getpgrp().expect("...")` to `getpgrp()`
    - Change `match getpgrp() { Ok(pgid) => ... }` to `let pgid = getpgrp();`
  - Rationale: Aligns with POSIX.1 specification and matches `getpid()`/`getppid()` patterns
  - Verified on Linux, macOS, FreeBSD, OpenBSD per POSIX.1 specification
  - Updated all tests and documentation to reflect this guarantee

### Improved
* **Enhanced documentation** - Comprehensive improvements to library documentation
  - Added "Common Patterns" section with practical examples:
    - Process supervisor using HashMap with Fork
    - Inter-process communication via pipes
    - Daemon with PID file creation
  - Added "Safety and Best Practices" guidelines
  - Added detailed "Common Pitfalls and Safety Considerations" to `fork()`:
    - Mutexes and locks (deadlock risks)
    - File descriptors (shared state issues)
    - Signal handlers (inheritance behavior)
    - Async-signal-safety between fork and exec
    - Memory usage (copy-on-write behavior)
  - Enhanced `Fork` enum documentation with helper method examples
  - Added "Platform Compatibility" information
* **Test quality improvements**
  - Replaced deprecated `signal()` with `sigaction()` in EINTR tests
  - More portable signal handling for cross-platform compatibility
  - Renamed `test_getpgrp_returns_io_error_type` to `test_getpgrp_returns_pid_type`
  - Updated test README to reflect current test descriptions

### Fixed
* **Documentation warnings** - Resolved doctest warnings about main function wrapping
* **EINTR resilience** - `close_fd` and `redirect_stdio` now retry on `EINTR` for `close/open/dup2`, preventing spurious failures under signal-heavy conditions on Linux, macOS, and BSD
* **Daemon exit safety** - Replaced `std::process::exit` in post-fork parents with `libc::_exit` to avoid running non-async-signal-safe destructors, preventing undefined behavior between `fork()` and `exec()`

### Code Quality
* **Modernized C string handling** - Replaced runtime `CString::new()` allocations with compile-time `c""` string literals (Rust 2024 feature)
  - `chdir()` now uses `c"/"` instead of `CString::new("/")`
  - `redirect_stdio()` now uses `c"/dev/null"` instead of `CString::new("/dev/null")`
  - Benefits: Eliminated dead error handling code, zero runtime overhead, compile-time validation
  - No API changes, fully backward compatible
* **Enhanced code clarity** - Added clarifying comments to `redirect_stdio()` error handling logic explaining conditional cleanup of file descriptors
* **Comprehensive test coverage** - Added 12 dedicated tests for `chdir()` function (346 lines)
  - Tests idempotent behavior, process isolation, concurrent usage
  - Validates modern `c""` string literal implementation
  - Tests integration with `setsid()` (daemon pattern)
  - Total test count increased from 107 to 119 tests

## 0.5.0

### Breaking Changes
* **`waitpid()` return type changed** - Now returns `io::Result<libc::c_int>` instead of `io::Result<()>`
  - Returns the raw status code for inspection with `WIFEXITED`, `WEXITSTATUS`, `WIFSIGNALED`, `WTERMSIG`, etc.
  - Migration: Change `waitpid(pid)?` to `let status = waitpid(pid)?; assert!(WIFEXITED(status));`
  - Enables proper exit code checking and signal detection
  - See updated examples in documentation

### Added
* **Fork helper methods** - Added convenience methods to `Fork` enum
  - `is_parent()` - Check if this is the parent process
  - `is_child()` - Check if this is the child process
  - `child_pid()` - Get child PID if parent, otherwise None
* **Hash trait** - `Fork` now derives `Hash`, enabling use in `HashMap` and `HashSet`
  - Useful for process supervisors and tracking multiple children
  - Examples: `supervisor.rs` and `supervisor_advanced.rs`
* **must_use attributes** - Added `#[must_use]` to critical functions to prevent accidental misuse
  - `fork()` - Must check if parent or child
  - `daemon()` - Must check daemon result
  - `setsid()` - Must use session ID
  - `getpgrp()` - Must use process group ID
* **`waitpid_nohang()` function** - Non-blocking variant of `waitpid()`
  - Returns `Ok(Some(status))` if child has exited
  - Returns `Ok(None)` if child is still running
  - Essential for process supervisors and event loops
  - Enables polling patterns without blocking
  - Includes 7 comprehensive tests
* **PID helper functions** - Convenience wrappers for getting process IDs
  - `getpid()` - Get current process ID (always succeeds, hides unsafe)
  - `getppid()` - Get parent process ID (always succeeds, hides unsafe)
* **Status macro re-exports** - Convenient access to status inspection macros
  - Re-export `WIFEXITED`, `WEXITSTATUS`, `WIFSIGNALED`, `WTERMSIG` from libc
  - Users can now `use fork::{waitpid, WIFEXITED, WEXITSTATUS}` instead of separate libc import
* **Comprehensive test suite** - Added extensive tests covering critical edge cases
  - `tests/waitpid_tests.rs` - Exit codes, signals, error handling, and non-blocking waits
  - `tests/error_handling_tests.rs` - Error paths and type verification
  - `tests/pid_tests.rs` - PID helper functions (getpid, getppid)
  - `tests/status_macro_tests.rs` - Status macro re-exports

### Improved
* **Performance** - Added `#[inline]` hints to thin wrapper functions (`chdir`, `setsid`, `getpgrp`, `getpid`, `getppid`)
* **Documentation** - Enhanced with comprehensive examples and safety considerations
  - Added doc test for `setsid()` - Session creation example
  - Added doc test for `getpgrp()` - Process group query example
  - Added doc test for `getpid()` - Current PID example
  - Added doc test for `getppid()` - Parent PID example
  - Enhanced `fork()` with safety considerations (file descriptors, mutexes, async-signal-safety, signals, memory)
  - Enhanced `waitpid()` with status inspection examples
  - Added `waitpid_nohang()` with polling patterns and process supervisor examples
* **Daemon correctness** - `daemon()` now performs the full double-fork, exiting the intermediate session leader so only the daemon continues
  - Docs clarified the numbered double-fork stages
  - Examples updated (`example_daemon.rs`, `example_touch_pid.rs`) to reflect that only the daemon process returns `Fork::Child`
* **waitpid robustness** - Automatic retry on `EINTR` (signal interruption)
  - Takes `pid_t` instead of `i32` for better type safety
  - Returns raw status code enabling exit code inspection and signal detection
* **Code quality** - Simplified `daemon()` implementation using `?` operator consistently
* **Test coverage** - Comprehensive coverage of all error paths and edge cases
  - Error handling: Invalid PID (ECHILD), double-wait, session leader errors (EPERM)
  - Exit codes: 0, 1, 42, 127, 255, and multiple code variations
  - Signal termination: SIGKILL, SIGTERM, SIGABRT detection
  - Status inspection: WIFEXITED vs WIFSIGNALED distinction
  - Fork helper methods: `is_parent()`, `is_child()`, `child_pid()`
  - io::Error type verification for all functions
* **CI** - GitHub Actions now run tests serially (`RUST_TEST_THREADS=1`) and use the latest checkout action

### Examples
* Added `supervisor.rs` - Basic process supervisor example
* Added `supervisor_advanced.rs` - Production-ready supervisor with restart policies

### Migration Guide (0.4.x → 0.5.0)

#### Before (0.4.x):
```rust
match fork() {
    Ok(Fork::Parent(child)) => {
        waitpid(child)?; // Just waits, no status
    }
    Ok(Fork::Child) => exit(0),
    Err(e) => eprintln!("Fork failed: {}", e),
}
```

#### After (0.5.0):
```rust
use libc::{WIFEXITED, WEXITSTATUS};

match fork() {
    Ok(Fork::Parent(child)) => {
        let status = waitpid(child)?; // Returns status code
        assert!(WIFEXITED(status), "Child should exit normally");
        let exit_code = WEXITSTATUS(status);
        println!("Child exited with code: {}", exit_code);
    }
    Ok(Fork::Child) => exit(0),
    Err(e) => eprintln!("Fork failed: {}", e),
}
```

## 0.4.0

### Breaking Changes
* **Improved error handling** - All functions now return `io::Result` instead of `Result<T, i32>`
  - `fork()` now returns `io::Result<Fork>` (was `Result<Fork, i32>`)
  - `daemon()` now returns `io::Result<Fork>` (was `Result<Fork, i32>`)
  - `setsid()` now returns `io::Result<libc::pid_t>` (was `Result<libc::pid_t, i32>`)
  - `getpgrp()` now returns `io::Result<libc::pid_t>` (was `Result<libc::pid_t, i32>`)
  - `waitpid()` now returns `io::Result<()>` (was `Result<(), i32>`)
  - `chdir()` now returns `io::Result<()>` (was `Result<libc::c_int, i32>`)
  - `close_fd()` now returns `io::Result<()>` (was `Result<(), i32>`)

### Major Improvements
* **Fixed file descriptor reuse bug** (Issue #2)
  - Added `redirect_stdio()` function that redirects stdio to `/dev/null` instead of closing
  - Prevents silent file corruption when daemon opens files after stdio is closed
  - `daemon()` now uses `redirect_stdio()` instead of `close_fd()`
  - Matches industry standard implementations (libuv, systemd, BSD daemon(3))
  
### Benefits
* **Better error diagnostics** - Errors now capture and preserve `errno` values
* **Rich error messages** - Error display shows descriptive text (e.g., "Permission denied") instead of `-1`
* **Rust idioms** - Integrates seamlessly with `?` operator, `anyhow`, `thiserror`, and other error handling crates
* **Type safety** - Can match on `ErrorKind` variants for specific error handling
* **Debugging** - `.raw_os_error()` provides access to underlying errno when needed
* **Correctness** - No more file descriptor reuse bugs that could corrupt data files

### Added
* `Fork` enum now derives `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq` for better usability
* `redirect_stdio()` function - Safer alternative to `close_fd()`
* Comprehensive tests for stdio redirection (`tests/stdio_redirect_tests.rs`)
  - Test demonstrating the fd reuse bug with `close_fd()`
  - Tests verifying `redirect_stdio()` prevents fd reuse
  - Tests confirming `daemon()` uses correct behavior

### Improved
* Simplified `close_fd()` implementation using iterator pattern
* Enhanced documentation with detailed error descriptions for all functions
* Updated all examples to use proper error handling patterns
* Added warnings to `close_fd()` documentation about fd reuse risks

### Security
* **CRITICAL FIX**: `daemon()` no longer vulnerable to file descriptor reuse bugs
  - Previously, files opened after `daemon(false, false)` could get fd 0, 1, or 2
  - Any `println!`, `eprintln!`, or panic would write to those files, corrupting them
  - Now stdio is redirected to `/dev/null`, keeping fd 0,1,2 occupied
  - New files always get fd >= 3

## 0.3.1
* Added comprehensive test coverage for `getpgrp()` function
  - Unit tests in `src/lib.rs` (`test_getpgrp`, `test_getpgrp_in_parent`)
  - Integration test `test_getpgrp_returns_process_group` in `tests/integration_tests.rs`
* Added `coverage` recipe to `.justfile` for generating coverage reports with grcov

## 0.3.0

### Changed
* Updated Rust edition from 2021 to 2024
* Applied edition 2024 formatting standards (alphabetical import ordering)

### Added
* **Integration tests directory** - Added `tests/` directory with comprehensive integration tests
  - `daemon_tests.rs` - 5 tests for daemon functionality (detached process, nochdir, process groups, command execution, no controlling terminal)
  - `fork_tests.rs` - 7 tests for fork functionality (basic fork, parent-child communication, multiple children, environment inheritance, command execution, different PIDs, waitpid)
  - `integration_tests.rs` - 5 tests for advanced patterns (double-fork daemon, setsid, chdir, process isolation, getpgrp)

### Improved
* Significantly expanded test coverage from 1 to 13 comprehensive unit tests
* Added tests for all public API functions:
  - `fork()` - Multiple test scenarios including child execution
  - `daemon()` - Daemon pattern tested (double-fork with setsid)
  - `waitpid()` - Proper parent-child synchronization
  - `setsid()` - Session management and verification
  - `getpgrp()` - Process group queries
  - `chdir()` - Directory changes with verification
  - `close_fd()` - File descriptor management
* Added real-world usage pattern tests:
  - Classic double-fork daemon pattern
  - Multiple sequential forks
  - Command execution in child processes
* Improved test quality with proper cleanup and zombie process prevention
* Enhanced CI/CD integration with LLVM coverage instrumentation
* **Total test count: 35 tests** (13 unit + 17 integration + 5 doc tests)

### Fixed
* Daemon tests now properly test the daemon pattern without calling `daemon()` directly
  (which would call `exit(0)` and terminate the test runner)

### Updated
* GitHub Actions: codecov/codecov-action from v4 to v5

## 0.2.0
* Added waitpid(pid: i32)
