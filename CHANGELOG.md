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
