## 0.3.0

### Changed
* Updated Rust edition from 2021 to 2024
* Applied edition 2024 formatting standards (alphabetical import ordering)

### Improved
* Significantly expanded test coverage from 1 to 13 comprehensive tests
* Added tests for all public API functions:
  - `fork()` - Multiple test scenarios including child execution
  - `daemon()` - Three configurations (with/without chdir, with/without close_fd)
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

### Updated
* GitHub Actions: codecov/codecov-action from v4 to v5

## 0.2.0
* Added waitpid(pid: i32)
