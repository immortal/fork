# fork

[![Crates.io](https://img.shields.io/crates/v/fork.svg)](https://crates.io/crates/fork)
[![Documentation](https://docs.rs/fork/badge.svg)](https://docs.rs/fork)
[![Build](https://github.com/immortal/fork/actions/workflows/build.yml/badge.svg)](https://github.com/immortal/fork/actions/workflows/build.yml)
[![codecov](https://codecov.io/gh/immortal/fork/graph/badge.svg?token=LHZW56OC10)](https://codecov.io/gh/immortal/fork)
[![License](https://img.shields.io/crates/l/fork.svg)](https://github.com/immortal/fork/blob/main/LICENSE)

Library for creating a new process detached from the controlling terminal (daemon) on Unix-like systems.

## Features

- ✅ **Minimal** - Small, focused library for process forking and daemonization
- ✅ **Safe** - Comprehensive test coverage across all APIs and edge cases
- ✅ **Well-documented** - Extensive documentation with real-world examples
- ✅ **Unix-first** - Built specifically for Unix-like systems (Linux, macOS, BSD)
- ✅ **Edition 2024** - Uses latest Rust edition features

## Why?

- Minimal library to daemonize, fork, double-fork a process
- [daemon(3)](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man3/daemon.3.html) has been
deprecated in macOS 10.5. By using `fork` and `setsid` syscalls, new methods
can be created to achieve the same goal
- Provides the building blocks for creating proper Unix daemons

## Installation

Add `fork` to your `Cargo.toml`:

```toml
[dependencies]
fork = "0.6.0"
```

Or use cargo-add:

```bash
cargo add fork
```

## Quick Start

### Basic Daemon Example

```rust
use fork::{daemon, Fork};
use std::process::Command;

fn main() {
    if let Ok(Fork::Child) = daemon(false, false) {
        // This code runs in the daemon process
        Command::new("sleep")
            .arg("300")
            .output()
            .expect("failed to execute process");
    }
}
```

### Simple Fork Example

```rust
use fork::{fork, Fork, waitpid, WIFEXITED, WEXITSTATUS};

match fork() {
    Ok(Fork::Parent(child)) => {
        println!("Parent process, child PID: {}", child);

        // Wait for child and check exit status
        match waitpid(child) {
            Ok(status) => {
                if WIFEXITED(status) {
                    println!("Child exited with code: {}", WEXITSTATUS(status));
                }
            }
            Err(e) => eprintln!("waitpid failed: {}", e),
        }
    }
    Ok(Fork::Child) => {
        println!("Child process");
        std::process::exit(0);
    }
    Err(e) => eprintln!("Fork failed: {}", e),
}
```

### Error Handling with Rich Diagnostics

```rust
use fork::{fork, Fork};

match fork() {
    Ok(Fork::Parent(child)) => {
        println!("Spawned child with PID: {}", child);
    }
    Ok(Fork::Child) => {
        println!("I'm the child!");
        std::process::exit(0);
    }
    Err(err) => {
        eprintln!("Fork failed: {}", err);
        // Access the underlying errno if needed
        if let Some(code) = err.raw_os_error() {
            eprintln!("OS error code: {}", code);
        }
    }
}
```

## API Overview

### Main Functions

- **`fork()`** - Creates a new child process
- **`daemon(nochdir, noclose)`** - Creates a daemon using double-fork pattern
  - `nochdir`: if `false`, changes working directory to `/`
  - `noclose`: if `false`, redirects stdin/stdout/stderr to `/dev/null`
- **`setsid()`** - Creates a new session and sets the process group ID
- **`waitpid(pid)`** - Waits for child process to change state (blocking; returns raw status; retries on signals)
- **`waitpid_nohang(pid)`** - Checks child status without blocking (returns `Option<status>`; for supervisors/polling)
- **`getpgrp()`** - Returns the process group ID
- **`getpid()`** - Returns the current process ID
- **`getppid()`** - Returns the parent process ID
- **`chdir()`** - Changes current directory to `/`
- **`redirect_stdio()`** - Redirects stdin/stdout/stderr to `/dev/null` (recommended)
- **`close_fd()`** - Closes stdin, stdout, and stderr (legacy, use `redirect_stdio()` instead)

### Status Inspection Macros (re-exported from libc)

- **`WIFEXITED(status)`** - Check if child exited normally
- **`WEXITSTATUS(status)`** - Get exit code (if exited normally)
- **`WIFSIGNALED(status)`** - Check if child was terminated by signal
- **`WTERMSIG(status)`** - Get terminating signal (if signaled)

See the [documentation](https://docs.rs/fork) for detailed usage.

## Process Tree Example

When using `daemon(false, false)`, it will change directory to `/` and redirect stdin/stdout/stderr to `/dev/null`.

Test running:

```bash
$ cargo run
```

Use `ps` to check the process:

```bash
$ ps -axo ppid,pid,pgid,sess,tty,tpgid,stat,uid,%mem,%cpu,command | egrep "myapp|sleep|PID"
```

Output:

```
 PPID   PID  PGID   SESS TTY      TPGID STAT   UID       %MEM  %CPU COMMAND
    1 48738 48737      0 ??           0 S      501        0.0   0.0 target/debug/myapp
48738 48753 48737      0 ??           0 S      501        0.0   0.0 sleep 300
```

Key points:
- `PPID == 1` - Parent is init/systemd (orphaned process)
- `TTY = ??` - No controlling terminal
- New `PGID = 48737` - Own process group

Process hierarchy:

```
1 - root (init/systemd)
 └── 48738 myapp        PGID - 48737
      └── 48753 sleep   PGID - 48737
```

## Double-Fork Daemon Pattern

The `daemon()` function implements the classic double-fork pattern:

1. **First fork** - Creates child process
2. **setsid()** - Child becomes session leader
3. **Second fork** - Grandchild is created (not a session leader)
4. **First child exits** - Leaves grandchild orphaned
5. **Grandchild continues** - As daemon (no controlling terminal)

This prevents the daemon from ever acquiring a controlling terminal.

## Safety Notes

- `daemon()` uses `_exit` in the forked parents to avoid running non-async-signal-safe destructors between fork/exec (POSIX-safe on Linux/macOS/BSD).
- `redirect_stdio()` and `close_fd()` retry on `EINTR` for `open/dup2/close` to prevent spurious failures under signal-heavy workloads.
- Prefer `redirect_stdio()` over `close_fd()` so file descriptors 0,1,2 stay occupied (avoids accidental log/data corruption).

## Testing

Run tests:

```bash
cargo test
```

See [`tests/README.md`](tests/README.md) for detailed information about integration tests.

## Platform Support

This library is designed for Unix-like operating systems:

- ✅ Linux
- ✅ macOS
- ✅ FreeBSD
- ✅ NetBSD
- ✅ OpenBSD
- ❌ Windows (not supported)

## Documentation

- [API Documentation](https://docs.rs/fork)
- [Integration Tests Documentation](tests/README.md)
- [Changelog](CHANGELOG.md)

## Examples

See the [`examples/`](examples/) directory for more usage examples:

- `example_daemon.rs` - Daemon creation
- `example_pipe.rs` - Fork with pipe communication
- `example_touch_pid.rs` - PID file creation

Run an example:

```bash
cargo run --example example_daemon
```

## Contributing

Contributions are welcome! Please ensure:

- All tests pass: `cargo test`
- Code is formatted: `cargo fmt`
- No clippy warnings: `cargo clippy -- -D warnings`
- Documentation is updated

## License

BSD 3-Clause License - see [LICENSE](LICENSE) file for details.
