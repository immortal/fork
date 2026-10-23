## 0.11.0

### Added
* `children_are_waitable()` observes owner SIGCHLD policy without changing it.
  `ensure_waitable_children()` explicitly corrects inherited auto-reaping to
  the default disposition at single-threaded startup, returning whether it
  changed state and preserving valid installed handlers. Guard creation and
  prepared spawn continue accepting auto-reaping owners; checked daemonization
  and numeric-PID cleanup require waitability. No existing API implicitly resets
  the owner's policy.
* `ProcessGroupGuard::signal_group(signal, timeout)` delivers through the helper
  while it owns the unreaped anchor. Helper loss fails rather than signalling a
  potentially reused numeric group; concurrent shared calls are serialized.
  Non-reused u32 request IDs ensure success acknowledges this exact request;
  stale replies are discarded and partial replies retained across deadlines.
  Consuming reads detect EOF without platform-specific `POLLHUP` assumptions.
  A timeout does not cancel a queued signal. macOS reports `EPERM` for an
  anchor-only group.
* `process_group_exists(group)` reports whether any process, including an
  unreaped zombie, is still a member of a process group. It performs the POSIX
  null-signal check `kill(-pgid, 0)` and delivers nothing: success and `EPERM`
  mean present, `ESRCH` means absent, `EINTR` is retried, and any other error is
  returned. A supervisor can use it to wait, with its own deadline, until a
  group it killed has drained before reporting cleanup complete.
* An armed `ProcessGroupGuard` now pins its group. The out-of-group helper creates
  the group's anchor and leaves it unreaped while the guard is armed, so the
  zombie keeps the group, and its numeric ID, reserved after every workload
  member has exited or left. The helper's owner-loss `SIGKILL` therefore always
  reaches the group the guard reserved, never an unrelated group that reused
  an emptied ID. `disarm` reaps the anchor and releases the pin. On macOS,
  which skips zombie members, a signal to a group holding only the anchor
  answers `EPERM`; the helper and `process_group_exists` treat that group as
  present.
* Typed contracts covering a live group, a group that holds only an unreaped
  member, refusal of the broadcast group, an armed guard that keeps its emptied
  group reserved until disarm, and owner loss that releases it. Guard contracts
  also cover `signal_group` delivery, failure after external helper loss, and
  timeout while the helper is stopped; stopped-helper disarm and armed drop,
  including an already consumed stop; and request framing. Harness-free
  contracts cover an anchor stopped before it exits, descriptors above a
  lowered soft limit, including forced fallback and descriptor exhaustion, and
  successful guard/spawn creation and helper-mediated delivery under an
  auto-reaping owner (`SIGCHLD=SIG_IGN` or `SA_NOCLDWAIT`), daemon rejection,
  skipped unpinned cleanup signals, and isolation from every inherited application
  handler, including non-SIGCHLD handlers that reap children, and from
  registered `pthread_atfork` callbacks that reap the helper's children or
  restore its auto-reaping. Denied inventory and denied `waitid` (`ENOSYS`,
  `EPERM`, `EACCES`, `EINVAL` and `EIO`, plus a real seccomp `EACCES` filter)
  contracts preserve successful sandboxed calls, and a seccomp filter that
  permits only the C library's own `fork` form of `clone`, unused arguments
  included, still permits guard creation.

### Safety
* Guard creation and prepared spawn retain their 0.10.0 success under ignored
  SIGCHLD or SA_NOCLDWAIT. The helper resets its own policy and holds the anchor;
  `signal_group` still works. Every owner-side cleanup PID signal checks policy
  and an unreaped child pin first. Otherwise cleanup skips delivery: a stopped
  helper cannot be resumed/killed, and a failed spawn may report an unfinished
  child in `cleanup_pending` rather than kill it. Serialize policy and collection.
* Checked daemonization alone rejects auto-reaping before fork, at `SignalState`
  with `InvalidInput`. In 0.10.0 its required intermediate wait already failed
  under that policy after starting a daemon, then attempted unsafe cleanup.
  Earlier rejection removes no successful original-invoker outcome.
* Checked-daemon error cleanup prefers retaining an unexpectedly terminated
  intermediate with `waitid(WNOWAIT)` until every signal is finished. When
  `waitid` fails with any errno, exact-child consuming waits preserve the 0.10.0
  success path: sandboxes deny it with `ENOSYS`, `EPERM` or `EACCES`, and a
  kernel without `WNOWAIT` answers `EINVAL`. A consumed unexpected exit forbids PID/group delivery;
  any surviving group is reported as an observation-only cleanup obligation.
  Every cleanup observation propagates consumed events instead of waiting again.
* Before creating the anchor, the helper resets every catchable application
  handler to default, preserves ignored signals except SIGCHLD and SIGCONT,
  and restores its inherited mask. The anchor inherits that state; owner handlers
  remain untouched. Resetting SIGCHLD alone was insufficient: a different handler
  could collect the anchor and silently release its pin.
* The helper then creates the anchor without `fork(3)`, whose `pthread_atfork`
  handlers would otherwise run inside the helper after the anchor exists: a
  parent handler could reap it or restore auto-reaping, releasing the pin while
  the helper still signals the group, and a prepare handler could block on a
  lock another owner thread held. Linux and Android on architectures whose raw
  `clone` returns 0 in the child issue the system call the C library's own
  `fork` uses, with the same flags and argument layout (glibc and bionic:
  `clone` with `CHILD_SETTID | CHILD_CLEARTID | SIGCHLD` and zero unused
  arguments; musl: `SYS_fork`, or `clone(SIGCHLD)` where none exists), so a
  sandbox that filters these permits the anchor; only the per-process child
  TID address differs from glibc's. FreeBSD uses
  `rfork`, and every other target, including macOS, uses `posix_spawn` of
  `/usr/bin/true` into a new group with owner-prepared attributes. After the
  signal reset no foreign code runs in the helper, so only the helper can reap
  its anchor.
* A helper that cannot close its inherited descriptors or create and confirm
  the anchor reports that errno to the owner, so guard creation returns the
  real OS error (for example a sandbox's `EACCES` or the spawn error) instead
  of a generic end of file.
* Stopped helpers are resumed during disarm and armed Drop, even when a broker
  already consumed the stop event, so they can process disarm or owner loss.
  A nonterminal child-ownership probe precedes CONT; terminal reaps/ECHILD
  never authorize signalling the numeric helper PID. The deadline falls back to
  helper `SIGKILL`; the final reap still depends on kernel progress, not an
  absolute time bound. The helper resets its own SIGCONT disposition, so these
  new internal resume signals cannot invoke an inherited owner callback or
  break a previously successful disarm. The owner's handler remains unchanged.
* The guard's owner socket is nonblocking. Exchanges and group-record reads
  wait with `poll`; disarm's single-byte send fails immediately if the queue is
  full, then closes the endpoint and performs bounded cleanup rather than
  hanging before it. `MSG_DONTWAIT` alone is not enough: macOS ignores it on sends.
* Anchor release kills the helper's own unreaped anchor PID before waiting,
  covering an anchor stopped before its initial exit without touching workloads.
* Descriptor fallback bounds include open descriptors above a subsequently
  lowered soft limit. Each preparation first probes `close_range`, then prefers
  a pre-fork `/dev/fd` inventory. When directory access/iteration is unavailable,
  descriptor-free `fcntl(F_GETFD)` probes above the soft limit to the finite hard
  limit (on macOS capped by `kern.maxfilesperproc`; Linux hard limits cannot exceed
  `nr_open`). FreeBSD skips its potentially incomplete inventory. Without a finite
  ceiling, preparation fails closed. If `F_GETFD` cannot classify a descriptor,
  closing uses the whole finite ceiling instead. Residual: a descriptor
  opened before lowering the **hard** limit can exceed this fallback's ceiling;
  preserve inventory/range access in that case. Serialize descriptor/limit changes.
  After a successful range probe,
  any actual range-close failure aborts the child operation instead of falling
  back to an incomplete soft bound. A selective seccomp regression covers this.
  The probe runs in the owner, so a sandbox must answer `close_range` with an
  errno: a filter that kills on it now terminates the owner, where 0.10.0 called
  it only in the child and that child died instead. Both fail the operation.
* The pre-exit anchor preload regression runs only on GNU/Linux: static musl
  cannot interpose `setpgid` through `LD_PRELOAD`.
* `signal_process_group` and `process_group_exists` refuse group 1 with
  `ErrorKind::InvalidInput`: `kill(-1, ..)` is the POSIX broadcast to every
  process the caller may signal, never a selector for one group.
* The probe documents that a positive answer never proves the original group
  still exists once it may have emptied; callers must never signal a group
  because the probe reported it present.
* The checked daemon's bounded group cleanup now uses the shared probe, keeping
  its fail-closed behavior for every error other than `ESRCH`.

### Compatibility

The public API is additive; CI compares 0.10.0 with `--release-type minor` instead
of treating a pre-1.0 minor bump as permission to skip compatibility checks.
The behavior audit against `b567941` uses **(a)** unchanged successful contracts,
**(b)** fixes of unsafe or nonworking behavior, and **(c)** real breaks. The only
(c) items are two accepted deployment constraints of the callback-free anchor:
on `posix_spawn` anchor targets guard creation now executes `/usr/bin/true`
through the C library's spawn, and on the raw-fork Linux architectures the
anchor's child TID address differs from the one glibc's `fork` passes. An
environment that denies the first, or a seccomp filter pinned to glibc's exact
per-process address, fails creation where 0.10.0 succeeded. Observable safety
corrections and their consequences are explicit below.

| Surface | Classification and 0.11.0 behavior |
|---|---|
| Legacy fork/daemon/session/PID/wait/IPC/subreaper APIs; command/options/ID validation | (a) Existing implementation and accepted contracts unchanged. |
| Guard creation and prepared spawn with ignored SIGCHLD / SA_NOCLDWAIT | (a) Still succeed; no implicit owner-policy changes. The proposed up-front rejection was removed before release. |
| Checked daemon with an auto-reaping owner | (b) Reject before fork instead of failing its mandatory intermediate wait after creating a daemon and attempting unsafe kills. No old successful parent result is removed. |
| `signal_process_group(group=1, signal)` | (b) Reject the accidental `kill(-1, ...)` broadcast; it never correctly selected one group. Other valid group signals are unchanged. |
| Armed-group lifetime, topology and resources | (b) Keep the anchor until disarm to prevent reused-group kills. The anchor is now the helper's child, not the caller's. One direct helper remains before/after activation; two process-table entries persist while armed. An armed group stays present even without workloads. |
| `activate` and normal `disarm` | (b) Activation is bookkeeping rather than releasing the safety pin; timeout/repeat validation remains. Disarm releases the pin, so await group absence only afterwards. Successful workload-preserving disarm remains supported. |
| Stopped helper/anchor cleanup and owner SIGCONT handlers | (b) Resume/collect stopped helpers and kill only an unreaped stopped anchor; do not strand children or invoke inherited owner callbacks. Running-helper disarm with a custom owner SIGCONT handler, or denied resume delivery, still succeeds via the socket as in 0.10.0. |
| Helper CONT/KILL, failed-spawn and daemon cleanup | (b) Check owner policy and child ownership at delivery; never signal unpinned IDs. Return cleanup errors/obligations where prior termination was unsafe. Daemon failures retain the intermediate through signals when possible; consuming fallback failures skip delivery and report remaining obligations. |
| `waitid` failing with any errno (`ENOSYS`, `EPERM`, `EACCES`, `EINVAL`, ...) | (a) Successful checked daemonization and helper disarm still succeed via exact-child consuming waits as in 0.10.0; consumed terminal outcomes are preserved. |
| Application `pthread_atfork` callbacks and guard creation | (b) The anchor is created without fork callbacks, so callbacks no longer run inside the helper, where they could release the pin or deadlock. Guard creation now runs registered callbacks once, for the helper fork, instead of twice in the owner as 0.10.0's two owner forks did; owner-only callbacks keep their 0.10.0 success. |
| Anchor creation on `posix_spawn` targets (macOS, NetBSD, OpenBSD, DragonFly, other Linux architectures) | (c) Deployment constraint: creation executes `/usr/bin/true` once per guard; denying that exec, or its absence, fails creation with the spawn error where 0.10.0 needed no exec. On Linux architectures outside the raw-fork list a seccomp policy must also permit the C library's spawn process creation (`clone3`, or a vfork-style `clone(CLONE_VM \| CLONE_VFORK)`), not only its fork. Linux/Android on the listed architectures and FreeBSD need no executable. |
| Owner-side `close_range` probe | (b) A filter that kills on `close_range` now terminates the owner instead of only the child; errno-returning filters keep the fallback. Both versions fail the operation under a killing filter. |
| Inherited helper callbacks | (b) All catchable handlers reset before anchor creation, not only SIGCHLD/CONT. Other ignored signals remain ignored. This removes unsafe callback execution in the helper, not owner-side handlers. Default termination can cause helper loss, which never permits unpinned delivery. |
| Unavailable ranges and denied/exhausted descriptor inventory | (a) Safe guard/spawn calls still succeed using descriptor-free hard-limit probing, including high descriptors above a lowered soft limit. No mandatory directory-open prerequisite remains. |
| Descriptor preparation and actual range close | (b) Fail closed if no finite fallback ceiling exists or real `close_range` fails; earlier apparent success could leak descriptors. Residual descriptors above a subsequently lowered hard limit require inventory/range access. |
| Guard startup reservation handshake | (b) Require the helper to report an actually reserved group under a bound; child-side close/reservation failure no longer returns an unusable guard as success, and returns the helper's errno. |
| Anchor creation under `clone`-filtering seccomp policies (Linux/Android raw-fork architectures) | (a) The raw anchor fork uses the same system call, flags and argument layout as the C library's own `fork`, unused arguments zero, so a policy filtering these that permitted 0.10.0's guard creation, which forked with the library, still permits it. |
| Child TID address of the raw anchor fork (Linux/Android raw-fork architectures) | (c) Deployment constraint: glibc and bionic pass their thread descriptor's TID field, the anchor a stack slot. Both are per-process addresses under address randomization, so only a filter built inside the running process, pinned to glibc's exact address, refuses the anchor. Matching it would require calling the library's own fork, whose callbacks the anchor must avoid. |
| Error stage types | (a) Existing enum variants and discriminants remain; no exhaustive-match break. |
| Newly detected failures and cleanup metadata | (b) Previously hidden safety failures now surface at existing SignalState/descriptor/handshake stages. Pending IDs are observations/collection obligations, not signal capabilities. |
| Nonblocking private owner socket | (b) Bound full-queue failure instead of hanging before cleanup. This descriptor is not public; 0.10.0's one-byte disarm had no public request API to fill it. |
| New group probe, `signal_group`, policy query/correction | (a) Additive only; old callers need not call them. New framing serializes request IDs, handles partial/stale replies/EOF, and never falls back to direct group delivery. |
| Test and CI infrastructure | (a) No library runtime change; stdio/cwd fixtures honor TMPDIR and compatibility CI explicitly checks an additive minor release. |
| Internal layout/debug text and dependency refresh | (a) No stable layout/debug-string/PID-value guarantee is removed. `libc` moves from 0.2.186 to 0.2.191 with unchanged exported types/contracts; helper topology/cost changes are classified above. |

These classifications do not claim identical process counts, timing, diagnostic
text, syscall traces, or unsafe side effects. They preserve successful supported
operations while removing the documented safety defects. Programs must not use a
released group, a positive existence probe, or private-helper behavior as signal
authority.

## 0.10.0

### Added
* `acquire_subreaper`, `release_subreaper`, and `is_subreaper` let a supervisor
  that is not PID 1 adopt orphaned descendants so the `wait_any_*` families can
  observe and reap their terminal state. Acquisition and release are idempotent.
  The role is process-global, is not inherited across `fork`, and is preserved
  across `exec`.
* Linux support via `PR_SET_CHILD_SUBREAPER`/`PR_GET_CHILD_SUBREAPER` and FreeBSD
  support via `PROC_REAP_ACQUIRE`/`PROC_REAP_RELEASE`/`PROC_REAP_STATUS`. All
  three functions return `ErrorKind::Unsupported` on other targets.
* Standalone harness-free lifecycle contract covering state transitions, `fork`
  non-inheritance, `exec` preservation, and orphan adoption.

### Safety
* Platform system calls are isolated in a private module and every failure is
  converted through `io::Error::last_os_error`.
* FreeBSD acquisition normalizes `EBUSY` (already a reaper) to success, and
  `is_subreaper` excludes PID 1's implicit `REAPER_STATUS_REALINIT` role so the
  query reflects only an explicitly acquired attribute, matching Linux.
* Adoption observes and reaps orphaned descendants only; it does not enumerate or
  signal still-running descendants that escape an owned process group, so
  `wait_any_*` may report PIDs the supervisor did not spawn directly.

## 0.9.1

### Added
* Additive `ProcessGroupGuard` for fail-closed process-group owner loss, with a
  startup anchor, explicit disarm, bounded helper cleanup, descriptor isolation,
  running/stopped workload contracts, and an explicit session-escape boundary.
* Signal-safe disarm over a CLOEXEC Unix socket with `MSG_NOSIGNAL`, plus an
  adaptive bounded helper wait which avoids both fixed startup delay and busy
  polling.
* Standalone single-threaded contracts which kill the owning broker with
  `SIGKILL` before and after workload startup, proving that helpers and the
  workload disappear without Rust destructors, async cleanup, or inherited
  descriptors.

### Safety
* Helper cleanup never signals a numeric PID after a terminal event was reaped
  or direct-child ownership can no longer be proven, preventing cleanup from
  targeting a reused PID.
* Workload `TERM`, `STOP`, `CONT`, and `KILL` delivery cannot disable the
  out-of-group helper. Invalid deadlines, failed exec, dead or stopped helpers,
  closed standard descriptors, and deliberate session escape have bounded
  regression contracts.
* The API documents exclusive child-reaping ownership and immediate disarm
  after the final group member exits, because portable Unix cannot pin an empty
  numeric process-group ID against reuse.
* Descriptor scan bounds now use checked conversions across signed FreeBSD and
  unsigned Linux `rlim_t` definitions.
* Process tests own and remove their temporary files, suppress core generation
  for intentional `SIGABRT` cases, and fail CI if an artifact or test process
  survives the suite.

### Compatibility
* This release only adds public API. Existing `0.9.0` callers remain source
  compatible.

## 0.9.0

### Added
* Checked positive `ProcessId` and `ProcessGroupId` types.
* `fork_process()` and `ProcessFork` for a typed parent-side fork result.
* Explicit process-group creation, joining, inspection, and signal delivery.
* Nonzero `Signal` with portable supervisor signal constants; signal zero is
  not representable.
* Typed `ChildEvent::{Exited, Signalled, Stopped, Continued}` collection through
  blocking and nonblocking child-specific and any-child wait functions.
* Serial native lifecycle tests covering exit, stop/continue, signal delivery,
  process groups, event draining, cleanup, and `EINTR` retry.
* Native FreeBSD lifecycle CI (latest release) alongside Linux and macOS.
* Owned close-on-exec `Pipe` and `SocketPair` primitives for broker IPC and
  exec-status handshakes, including descriptor flag and data-flow tests.
* Additive `PreparedCommand` fork/exec API with precomputed arguments and
  environment, bounded startup handshakes, explicit process groups, collision-
  safe descriptor map/inherit/close actions, numeric identity transitions, and
  unintended-descriptor closure.
* Public API compatibility CI against the published `0.8.0` crate.
* Additive checked double-fork daemon startup with a bounded readiness/failure
  channel, preserved descriptors, explicit stdio policy, exact startup stages,
  and residual cleanup ownership.
* Default child signal-state reset for prepared exec and checked daemon paths,
  with explicit opt-in inheritance.
* Regression tests pinning the exact portable `Signal` constant values and the
  behavioral compatibility of the pre-broker `fork`/`waitpid`/`waitpid_nohang`
  path, plus a runnable `process_broker` example.

## 0.8.0

### Added
* **`wait_any()` and `wait_any_nohang()`** - Add non-breaking wrappers around
  `waitpid(-1, ...)` for supervisor-style process management.
  - `wait_any()` blocks until any child terminates and returns `(pid, status)`.
  - `wait_any_nohang()` checks for any terminated child without blocking and
    returns `Ok(Some((pid, status)))` or `Ok(None)`.
  - Existing `waitpid()` and `waitpid_nohang()` signatures are unchanged.
* **Checked daemon startup example** - Added `checked_daemon_pattern.rs`,
  demonstrating how to use a pre-fork pipe with the existing low-level
  primitives when the launching process must observe setup success or failure.

### Improved
* Clarified supervisor identity guidance: `HashMap` remains appropriate for
  tracking multiple live children, but long-lived restart/history state should
  use an application-owned monotonic id with PID as the current live lookup
  handle. Updated supervisor examples to show this pattern.
* Documented the `daemon(false, false)` startup pitfall: relative paths are
  resolved from `/`, and stdio/panic diagnostics are discarded through
  `/dev/null`. PID/log/config paths should generally be absolute, or startup
  should use `nochdir = true`, `noclose = true`, or a readiness pipe when
  appropriate.

### Fixed
* Corrected README guidance for `close_fd()`/`close()` on `EINTR`: the safe
  portable behavior is to call `close()` once and treat `EINTR` as success,
  not to retry.

## 0.7.0

### Fixed
* **`close()` no longer retries on `EINTR`** — Previously, the internal `close_retry` helper
  looped on `EINTR`, which is unsafe on all modern Unixes:
  - **Linux**: `close()` always releases the fd *before* returning `EINTR`.
    Retrying can close an unrelated fd opened by another thread between attempts.
  - **FreeBSD / macOS / other BSDs**: The fd state after `close()` + `EINTR` is
    *unspecified* per POSIX 2008+ (Austin Group defect 529), so retrying is equally dangerous.
  - The function has been renamed from `close_retry` to `close_once` and now calls `close()`
    exactly once, treating both `EINTR` and `EBADF` as success — the same approach used by
    Rust's `std::fs::File::drop()`, Go's runtime, and glibc internals.
  - **Note**: `open()` and `dup2()` in `redirect_stdio()` still correctly retry on `EINTR`,
    as those calls do *not* release resources on interruption.

### Improved
* **`daemon()` return value documentation** — Made it prominent that `daemon()` only ever
  returns `Ok(Fork::Child)` or `Err(...)` to the caller; `Ok(Fork::Parent(_))` is never
  returned because both parent processes call `_exit(0)` internally. Added recommended
  `if let` and `match` usage patterns to the doc comment. Updated `#[must_use]` message
  to reflect this guarantee.

### Code Quality
* Added `test_daemon_never_returns_parent` integration test confirming `Fork::Parent` is unreachable
* Renamed `test_close_retry_ok_and_ebadf` to `test_close_once_ok_and_ebadf`
* Replaced fragile `tty` command string-matching in `test_daemon_no_controlling_terminal` with
  portable `open("/dev/tty")` check (works reliably across Linux, macOS, and BSDs)
* Replaced fixed 100ms sleep in `test_getppid_after_parent_exits` with a retry loop (up to 1s),
  preventing flaky failures on slow CI systems
* Updated tests README to reflect new and renamed tests

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
