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
fork = "0.11.0"
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
  exposing the raw negative-PID `kill(2)` convention. Group 1 is refused,
  because `kill(-1, ..)` is a broadcast rather than a group selector.
- `process_group_exists(group)` reports whether any member, including an
  unreaped zombie, remains, without delivering a signal. Use it with a
  deadline to confirm that a killed group has drained; never signal a group
  because it was reported present, since an emptied group's ID can be reused.
- `wait_event*` and `wait_any_event*` report typed exit, signal, stop, and
  continue events and retry `waitpid(2)` after `EINTR`.

### Adopt orphaned descendants

On Linux and FreeBSD, a supervisor that is not PID 1 can opt into the OS
subreaper role before starting its workload:

```rust,no_run
use fork::{acquire_subreaper, wait_any_event};

acquire_subreaper()?;

// Start the supervised workload. If an intermediate parent exits, its
// orphaned descendants are adopted here and become visible to wait_any_*.
let event = wait_any_event()?;
println!("observed child event: {event:?}");
# Ok::<(), std::io::Error>(())
```

Acquisition is process-wide, is not inherited across `fork`, and is preserved
across `exec`. Adopted processes may have PIDs the supervisor did not spawn
directly, so unknown PIDs from `wait_any_*` are expected. Subreaping does not
enumerate live descendants or signal processes that escape an owned process
group. The API returns `ErrorKind::Unsupported` outside Linux and FreeBSD.

### Fail-closed Process Groups

#### The problem it solves

A supervisor normally stops a service by sending a signal and waiting for it.
That works only while the process responsible for cleanup is alive. If a
process broker is killed with `SIGKILL`, crashes, or is terminated by the
out-of-memory killer, it cannot run a destructor or an async cleanup task. The
service may therefore continue running without its owner.

Waiting for another process to notice a closed IPC connection is not
equivalent: the broker can die before publishing the new process-group ID, and
a numeric ID may be reused before delayed cleanup reaches it.
`ProcessGroupGuard` makes the cleanup authority exist before the workload
starts and keeps it beside—not inside—the workload group.

```text
 Tokio supervisor
        │ broker IPC
        ▼
 single-threaded broker ─── owner socket ─── guard helper
        │                                      │ │
        │ spawns into reserved group           │ │ unexpected EOF
        ▼                                      │ │ sends SIGKILL
 process group ◄───────────────────────────────┘ │
   ├── anchor zombie (the helper's child) ◄──────┘ reaped on disarm
   └── workload process(es)
```

The broker owns the socket endpoint and the `ProcessGroupGuard` value. The
helper owns the other endpoint and deliberately remains outside the workload
group. It also owns the group's anchor: a child which created the group and
exited at once, and which the helper leaves unreaped while the guard is armed.
That zombie keeps the group, and its numeric ID, reserved even after every
workload member has exited or left, while the helper holds the anchor.
Armed-group signals go through `guard.signal_group(signal, timeout)`: the helper
performs the kill itself before releasing the anchor. If the helper dies
unexpectedly, requests fail instead of reaching a potentially reused group ID. The kernel closes the broker endpoint even after `SIGKILL`; the
helper observes EOF and kills the complete group, which the anchor still pins.
During a clean shutdown, `disarm` sends an explicit record instead, so the
helper reaps the anchor and exits without signaling the workload.

#### Typical lifecycle

```rust,no_run
use std::time::Duration;

use fork::{PreparedCommand, ProcessGroup, ProcessGroupGuard, wait_event};

let timeout = Duration::from_secs(3);
let mut guard = ProcessGroupGuard::new()?;

let mut command = PreparedCommand::new("/bin/sleep")?;
command
    .arg("5")?
    .process_group(ProcessGroup::Join(guard.process_group()));
let child = command.spawn(timeout)?;

// The workload has joined the group the guard reserved and pins.
guard.activate(timeout)?;
let event = wait_event(child.process())?;

// Normal completion: the helper reaps its anchor, releasing the pin, and
// exits without signaling the group.
guard.disarm(timeout)?;
println!("workload finished: {event:?}");
# Ok::<(), Box<dyn std::error::Error>>(())
```

The repository includes this lifecycle as the runnable
`process_group_guard` example.

Internally, `new` starts the out-of-group helper, which creates the anchor
without running fork callbacks, makes it the leader of a new group, and reports
that group's ID once it exists. The
caller starts the workload in that reserved group and calls `activate`, which
records that the workload joined. One guard helper, its anchor zombie, and one
close-on-exec socket remain for the active group. Dropping an armed guard is
fail-closed: it closes the endpoint, lets the helper kill the group and reap
the anchor, and waits for helper cleanup under the deadline described below.

#### Ownership and limits

- Use this API in a dedicated single-threaded process broker, before starting a
  multithreaded runtime. It has no Tokio dependency or `Arc`; an owner-side
  mutex serializes `&self` signal exchanges without changing the guard's auto
  traits. Lock acquisition and socket I/O share the request's deadline.
- The broker must exclusively own and serialize collection of its direct-child
  events. `guard_process` exists so an all-child wait loop can classify an
  unexpected helper event; such an event means containment has failed. Never
  directly signal its numeric group unless an independently verified unreaped
  zombie member or an owned unreaped child whose PID equals the PGID still
  pins that ID. Neither a helper liveness check nor a live nonleader's
  `getpgid` observation is a pin: that member can leave immediately afterwards.
- `new` and `signal_group` work even when the owner ignores `SIGCHLD` or sets
  `SA_NOCLDWAIT`: the helper establishes its own waitable anchor policy. For
  owner-side cleanup to resume or kill a helper, keep SIGCHLD non-ignored and
  SA_NOCLDWAIT unset and serialize policy changes and collection. Each cleanup
  PID signal checks policy and an unreaped child pin first. Otherwise cleanup
  closes the socket and reports an error, but cannot resume or kill a stopped
  helper. A running helper can still act on disarm/EOF and release the anchor.
  The library never silently changes the owner's process-global policy.
- Startup creates two processes: the helper, with an ordinary `fork` that runs
  registered `pthread_atfork` callbacks as any fork does, and its anchor, which
  runs none (see below). Each active group then holds one sleeping helper
  process, one anchor zombie, and one socket endpoint.
  The helper blocks in `read` and consumes no scheduled CPU while healthy, and
  a zombie holds only a process-table entry, but two entries per group are
  still a real capacity cost for high-cardinality supervisors.
- Helper children allocate no shared state and retain only their guard
  descriptor. Linux and FreeBSD close inherited descriptors with
  `close_range`, probed in the owner before every preparation without closing a
  valid fd. A sandbox must answer that probe with an errno: a filter that kills
  on `close_range` now terminates the owner, where 0.10.0 called it only in the
  child and that child died instead. Both fail the operation.
  When supported, no descriptor inventory is needed, but an actual range-close
  failure then aborts the child operation: there is no incomplete soft-limit
  fallback. Otherwise `/dev/fd` inventories the highest open fd before fork.
  If opening or iterating that inventory fails, `fcntl(F_GETFD)` probes from
  the soft limit through the finite hard `RLIMIT_NOFILE` ceiling, without opening
  anything; macOS caps that ceiling with descriptor-free `kern.maxfilesperproc`.
  Linux's hard limit cannot exceed `nr_open`. FreeBSD skips its potentially
  incomplete `/dev/fd` inventory and uses this probe when ranges are unavailable.
  If `F_GETFD` cannot classify a descriptor, closing uses the whole finite
  ceiling instead. Preparation fails closed if no finite ceiling can be established. Residual limitation: a descriptor opened before the **hard** limit was
  lowered can exceed that ceiling; retain inventory/range access in that case.
  Concurrent descriptor creation/limit changes must be excluded during preparation.
  Parent cleanup briefly yields, then uses bounded exponential sleeps capped at
  five milliseconds rather than busy polling.
- `activate` is bookkeeping only; it validates its nonzero timeout for source
  compatibility. `disarm` uses the supplied deadline; armed `Drop` uses three
  seconds. Both verify child ownership and resume initially and on each stop,
  even if a broker drain already consumed the helper's stop event, so it can
  process disarm or owner EOF. Terminal reaps and ECHILD never authorize CONT.
  After the deadline they kill the helper, then reap it: that final wait depends
  on kernel progress and has no absolute bound. A helper unable to run cannot
  guarantee workload cleanup. A full owner socket makes disarm return a write
  error and close the owner, rather than block before resuming helper cleanup.
- While armed, use `signal_group`, never `signal_process_group`. Helper loss
  returns `BrokenPipe`/`UnexpectedEof` and forbids a direct retry. A timeout
  does not cancel an already queued signal; it may be delivered later. `Ok`
  acknowledges this exact request: non-reused u32 request IDs distinguish even
  repeated signals. Stale replies are consumed and discarded; partial reply
  bytes are retained across deadlines, and a zero-length read detects EOF
  without relying on `POLLHUP`. Zero/oversized timeouts and request-ID exhaustion
  are rejected.
- While the helper holds its anchor, `process_group_exists` reports the group
  present even after every workload member has gone. A group whose only member is the
  anchor has nothing live to signal; macOS, which skips zombie members,
  answers a signal to it with `EPERM`. Disarming releases the pin, so a
  disarmed group's ID may be reused and must never be signalled again. The helper
  gives every catchable signal exec-like dispositions before forking the anchor:
  application handlers become `SIG_DFL`, and ignored signals stay ignored except
  `SIGCHLD` (default to retain the zombie) and `SIGCONT` (default for cleanup).
  The helper, a post-fork child limited to async-signal-safe calls, then never
  calls `fork(3)`, which would run the application's `pthread_atfork` handlers
  inside it after the anchor exists. Linux and Android on architectures whose
  raw `clone` returns 0 in the child (x86, x86-64, Arm, AArch64, RISC-V,
  PowerPC, MIPS, LoongArch, s390x) fork the anchor with the system call, flags
  and argument layout the C library's own `fork` uses (glibc and bionic:
  `clone` with `CHILD_SETTID | CHILD_CLEARTID | SIGCHLD` and zero unused
  arguments; musl: `SYS_fork`, or `clone(SIGCHLD)` where none exists), so a
  sandbox that filters these permits the anchor. Only the child TID address
  differs from glibc's; it is a per-process address under address
  randomization, so only a filter built inside the running process could
  refuse it. FreeBSD uses `rfork`; the anchor only calls `setpgid` and `_exit`. Other
  Linux architectures (SPARC returns the parent's PID in the child; m68k, C-SKY
  and Hexagon are unverified) and every other target, including macOS, NetBSD,
  OpenBSD and DragonFly, use `posix_spawn` of `/usr/bin/true` into a new group,
  with default dispositions and an empty mask (and close-on-exec by default on
  macOS); the owner prepares the attributes before the helper fork, and the
  exited `true` is the zombie. The helper's own creation remains an ordinary
  fork whose child callbacks run before it sanitizes itself, exactly as for
  the public fork APIs. Thus neither an inherited signal handler nor a
  registered fork callback can collect the helper's anchor, and nothing but the
  helper can reap it. On the `posix_spawn` targets guard creation requires that
  `/usr/bin/true` can be executed, and on Linux that the C library's spawn
  process creation (`clone3` or a vfork-style `clone`) is permitted; a sandbox
  that denies either makes creation fail.
  The owner's handlers and mask are untouched.
  A signal whose default terminates the helper can still cause helper loss; that
  disables delivery, never authorizes an unpinned numeric fallback.
- After the final helper-mediated kill, disarm to release the anchor before
  waiting with `process_group_exists` and a deadline: the armed anchor itself
  otherwise keeps the probe true. Never signal the group after disarming.
  Do not assume the group is empty: a member can still be exiting, or be an
  unreaped zombie whose parent is outside the group. Stop at the first absent
  answer and never signal the group again.
- The portable boundary is the process group, not every possible descendant.
  A workload which calls `setsid` or joins another group has escaped. Use an
  operating-system-specific facility such as a FreeBSD process reaper or Linux
  cgroup when stronger containment is required.

### Broker IPC

`pipe_cloexec()` and `socket_pair_cloexec()` return owned endpoints and prevent
them from leaking through a successful `exec`. A pipe is suitable for a
one-way startup-status handshake; a socket pair supports bidirectional framed
messages between a supervisor and its dedicated process broker.

On Linux and the supported BSDs, close-on-exec is set atomically when each
endpoint is created. macOS requires a `fcntl` fallback, so construct broker IPC
before starting threads that could concurrently execute another program.

### Explicit Owner Policy at Startup

`children_are_waitable()` reads the current process's `SIGCHLD` disposition.
It returns false for explicit ignoring or `SA_NOCLDWAIT`, and does not change
state. This snapshot is not child ownership or synchronization.

A supervisor can deliberately call `ensure_waitable_children()` while still
single-threaded, before children or signal handlers depending on that state
exist. An ignored `SIGCHLD` survives `exec`, so launch environments can otherwise
make checked daemonization reject the owner and prevent numeric-PID cleanup of
spawned children or guard helpers. Guard/spawn creation and helper-mediated group
delivery remain supported without correction. The function returns true if it restored
`SIG_DFL` with an empty handler mask and no flags; otherwise it returns false
and leaves an existing valid handler untouched. It does not alter the calling
thread's signal mask or recover already auto-reaped children. Both functions
propagate OS errors and retry interrupted `sigaction` calls.

This is an explicit process-wide policy choice, never an implicit side effect
of checked creation. Afterwards the owner must keep children waitable and
serialize policy changes and child collection throughout every checked
operation and guard lifetime. A handler must not steal these child events.

### Prepared Fork and Exec

Prepared spawn's pipe handshake works with `SIGCHLD=SIG_IGN` and `SA_NOCLDWAIT`,
as in 0.10.0. Failure cleanup only signals a child PID if owner policy and an
unreaped child pin permit it; a stopped or unfinished child may instead be
reported in `cleanup_pending` without being killed. Returned IDs are not signal
capabilities. Serialize policy changes and collection across cleanup.

Checked daemonization requires collection of its intermediate, so it still
rejects auto-reaping before fork at `SignalState` with `InvalidInput`. In 0.10.0
this policy already made the original invoker fail after creating a daemon;
the earlier rejection removes unsafe cleanup, not a successful outcome. No
handler or concurrent waiter may reap its intermediate. The checked
daemon prefers `waitid(WNOWAIT)` to leave a failed intermediate unreaped until
cleanup has sent every signal. If `waitid` fails with any errno (sandboxes
answer `ENOSYS`, `EPERM` or `EACCES`; a kernel without `WNOWAIT` answers
`EINVAL`), an exact-child consuming wait preserves the old successful daemon path. Consuming an
unexpected exit removes the pin, so cleanup sends neither PID nor group signals
and reports any remaining group as an observation-only obligation. Other cleanup
paths also retain and handle consumed terminal outcomes instead of losing them.
Returned pending groups never authorize signals to an ID whose pin was released.

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
