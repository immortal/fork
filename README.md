# fork

[![Crates.io](https://img.shields.io/crates/v/fork.svg)](https://crates.io/crates/fork)
[![Documentation](https://docs.rs/fork/badge.svg)](https://docs.rs/fork)
[![Build](https://github.com/immortal/fork/actions/workflows/build.yml/badge.svg)](https://github.com/immortal/fork/actions/workflows/build.yml)
[![Coverage Status](https://coveralls.io/repos/github/immortal/fork/badge.svg?branch=main)](https://coveralls.io/github/immortal/fork?branch=main)
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
fork = "0.9.0"
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

### Typed Supervisor API

Supervisors should prefer the additive typed API. `ProcessId` and
`ProcessGroupId` accept only positive values, `Signal` cannot represent signal
zero, and process versus process-group delivery are separate operations.

```rust
use fork::{ChildEvent, wait_any_event_nohang};

while let Some(event) = wait_any_event_nohang()? {
    match event {
        ChildEvent::Exited { pid, code } => println!("{pid} exited with {code}"),
        ChildEvent::Signalled { pid, signal } => {
            println!("{pid} terminated by signal {signal}");
        }
        ChildEvent::Stopped { pid, signal } => {
            println!("{pid} stopped by signal {signal}");
        }
        ChildEvent::Continued { pid } => println!("{pid} continued"),
    }
}
# Ok::<(), std::io::Error>(())
```

The group helpers are explicit:

- `create_current_process_group()` creates a group in the child after fork.
- `create_process_group(process)` performs the matching parent-side operation
  to close the fork/exec race.
- `join_process_group(process, group)` joins another owned child.
- `signal_process(process, signal)` targets exactly one PID.
- `signal_process_group(group, signal)` targets every group member without
  exposing the raw negative-PID `kill(2)` convention.
- `wait_event*` and `wait_any_event*` report typed exit, signal, stop, and
  continue events and retry `waitpid(2)` after `EINTR`.

### Broker IPC

`pipe_cloexec()` and `socket_pair_cloexec()` return owned endpoints and prevent
them from leaking through a successful `exec`. A pipe is suitable for a
one-way startup-status handshake; a socket pair supports bidirectional framed
messages between a supervisor and its dedicated process broker.

On Linux and the supported BSDs, close-on-exec is set atomically when each
endpoint is created. macOS requires a `fcntl` fallback, so construct broker IPC
before starting threads that could concurrently execute another program.

### Prepared Fork and Exec

`PreparedCommand` is an additive API for a dedicated, single-threaded process
broker. It snapshots the environment and materializes the executable path,
arguments, working directory, pointer arrays, process-group intent, descriptor
mappings, descriptor close bounds, and numeric credentials before `fork`. The
child then performs only reviewed system calls before `execve` or `_exit`.

```rust
use std::time::Duration;
use fork::{PreparedCommand, ProcessGroup};

let mut command = PreparedCommand::new("/bin/sleep")?;
command.arg("5")?.process_group(ProcessGroup::New);
let child = command.spawn(Duration::from_secs(3))?;
println!("started {}", child.process());
# Ok::<(), Box<dyn std::error::Error>>(())
```

The executable is direct: the library performs neither shell parsing nor
`PATH` lookup. Descriptor sources are owned and duplicated above all targets
before fork, so overlapping mappings cannot destroy another mapping's source.
Descriptors can be mapped, inherited at their existing number, or explicitly
closed; everything else above standard error is closed before exec. Optional
identity transitions apply pre-resolved supplementary groups, primary GID, and
UID in that order—the library never performs account-database lookup after
fork. By default the child also clears its signal mask and restores portable
catchable dispositions so ignored or blocked supervisor signals do not leak
through exec. `ChildSignalState::Inherit` is an explicit opt-out.
An internal close-on-exec pipe distinguishes successful `execve` from a typed
pre-exec failure. The original low-level API remains unchanged.

### Checked Daemon Startup

`checked_daemon()` is the additive alternative when the invoking process must
remain alive and learn whether daemon initialization really succeeded. Call it
before creating threads. The intermediate child creates a session and performs
the second fork; the detached grandchild initializes resources and then uses
its `DaemonNotifier` to report ready or explicitly fail and exit.
The detached daemon receives the same default signal-state reset before
initialization; callers may explicitly request inheritance when necessary.

```rust
use std::time::Duration;
use fork::{CheckedDaemon, DaemonOptions, checked_daemon};

let mut options = DaemonOptions::new();
options
    .current_directory("/")?
    .redirect_standard_io_to_null()?;

match checked_daemon(options, Duration::from_secs(3))? {
    CheckedDaemon::Parent(process) => {
        println!("daemon {} is ready", process.process());
    }
    CheckedDaemon::Daemon(notifier) => {
        // Bind sockets, acquire locks, create PID files, and initialize logs.
        notifier.notify_ready()?;
        // Start the daemon event loop here.
    }
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

The deadline covers partial records and repeated `EINTR`, not only the first
pipe byte. Timeout and failure paths send `KILL`, reap the intermediate child,
and return any process or group that could not be confirmed clean through
`DaemonError::cleanup_pending()`. The existing `daemon()` behavior and
signature are unchanged.

### Main Functions

- **`fork_process()`** - Forks with a checked `ProcessId` in the parent
- **`fork()`** - Creates a new child process
- **`daemon(nochdir, noclose)`** - Creates a daemon using double-fork pattern
  - `nochdir`: if `false`, changes working directory to `/`
  - `noclose`: if `false`, redirects stdin/stdout/stderr to `/dev/null`
- **`setsid()`** - Creates a new session and sets the process group ID
- **`waitpid(pid)`** - Waits for child process to change state (blocking; returns raw status; retries on signals)
- **`waitpid_nohang(pid)`** - Checks child status without blocking (returns `Option<status>`; for supervisors/polling)
- **`wait_any()`** - Waits for any child process and returns `(pid, status)`
- **`wait_any_nohang()`** - Checks any child without blocking and returns `Option<(pid, status)>`
- **`getpgrp()`** - Returns the process group ID
- **`getpid()`** - Returns the current process ID
- **`getppid()`** - Returns the parent process ID
- **`chdir()`** - Changes current directory to `/`
- **`redirect_stdio()`** - Redirects stdin/stdout/stderr to `/dev/null` (recommended)
- **`close_fd()`** - Closes stdin, stdout, and stderr (legacy, use `redirect_stdio()` instead)
- **`pipe_cloexec()`** - Creates an owned close-on-exec unidirectional pipe
- **`socket_pair_cloexec()`** - Creates an owned close-on-exec Unix socket pair
- **`PreparedCommand`** - Materializes and directly executes a broker command
- **`checked_daemon()`** - Performs checked, bounded double-fork detachment

### Status Inspection Macros (re-exported from libc)

- **`WIFEXITED(status)`** - Check if child exited normally
- **`WEXITSTATUS(status)`** - Get exit code (if exited normally)
- **`WIFSIGNALED(status)`** - Check if child was terminated by signal
- **`WTERMSIG(status)`** - Get terminating signal (if signaled)

See the [documentation](https://docs.rs/fork) for detailed usage.

## Process Tree Example

When using `daemon(false, false)`, it will change directory to `/` and redirect stdin/stdout/stderr to `/dev/null`.

This matters for daemon startup code. Use absolute paths for PID files, logs,
sockets, and config files: after `chdir("/")`, `File::create("myapp.pid")`
tries to create `/myapp.pid`, not a file in the launch directory. With
`noclose = false`, any `println!`, `eprintln!`, or panic output from that
failure goes to `/dev/null`, which can make it look like the daemon block never
ran. Use `daemon(true, false)` if relative paths should stay relative to the
launch directory, and use `noclose = true` or a readiness pipe while debugging
startup failures.

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

### Checked Startup

`daemon()` intentionally follows the classic daemon pattern: after the first
fork succeeds, the original parent exits before later setup steps run. If the
launcher must remain alive and observe setup success or failure, use
`checked_daemon()` with an explicit timeout and `DaemonNotifier`. It preserves
the existing `daemon()` contract while providing checked detachment without
requiring each caller to implement its own wire protocol and cleanup path.

See `examples/checked_daemon_pattern.rs` for a complete checked-startup example.

## Safety Notes

- `daemon()` uses `_exit` in the forked parents to avoid running non-async-signal-safe destructors between fork/exec (POSIX-safe on Linux/macOS/BSD).
- `redirect_stdio()` retries `open()` and `dup2()` on `EINTR`; `close_fd()` calls `close()` once and treats `EINTR` as success to avoid closing a reused fd.
- Prefer `redirect_stdio()` over `close_fd()` so file descriptors 0,1,2 stay occupied (avoids accidental log/data corruption).
- For supervisors, prefer `ProcessId` as a checked live handle and remove it
  immediately after a terminal `ChildEvent`. Use your own monotonic identity for
  durable restart/history state because operating systems reuse PIDs.

## Testing

Run tests:

```bash
cargo test --all-targets --all-features -- --test-threads=1
```

The CI lifecycle suite runs natively on Linux, macOS, and FreeBSD.

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
- `checked_daemon_pattern.rs` - Bounded checked startup with explicit readiness
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
