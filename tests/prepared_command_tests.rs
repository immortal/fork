//! Native contract tests for the additive prepared fork/exec API.

use std::{
    error::Error,
    fs::File,
    io::{self, BufRead, BufReader, Read},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    thread,
    time::{Duration, Instant},
};

use fork::{
    ChildEvent, PreparedCommand, ProcessCredentials, ProcessGroup, ProcessGroupId, ProcessId,
    Signal, SpawnError, SpawnStage, SupplementaryGroups, pipe_cloexec, signal_process,
    signal_process_group, wait_any_event_nohang, wait_event, waitpid,
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(3);

struct ChildGuard {
    process: ProcessId,
    reaped: bool,
}

struct ProcessGroupGuard {
    child: ChildGuard,
    group: ProcessGroupId,
}

struct SignalStateGuard {
    signal: libc::c_int,
    previous_action: libc::sigaction,
    previous_mask: libc::sigset_t,
}

impl Drop for SignalStateGuard {
    fn drop(&mut self) {
        // SAFETY: both previous values were initialized by successful libc calls.
        unsafe {
            libc::sigaction(
                self.signal,
                &raw const self.previous_action,
                std::ptr::null_mut(),
            );
            libc::sigprocmask(
                libc::SIG_SETMASK,
                &raw const self.previous_mask,
                std::ptr::null_mut(),
            );
        }
    }
}

impl ProcessGroupGuard {
    const fn new(process: ProcessId, group: ProcessGroupId) -> Self {
        Self {
            child: ChildGuard::new(process),
            group,
        }
    }

    fn mark_reaped(&mut self) {
        self.child.mark_reaped();
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        let _ = signal_process_group(self.group, Signal::KILL);
    }
}

impl ChildGuard {
    const fn new(process: ProcessId) -> Self {
        Self {
            process,
            reaped: false,
        }
    }

    fn mark_reaped(&mut self) {
        self.reaped = true;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let _ = signal_process(self.process, Signal::KILL);
        let _ = waitpid(self.process.get());
    }
}

#[test]
fn prepared_command_reports_exact_exit() -> Result<(), Box<dyn Error>> {
    let mut command = PreparedCommand::new("/bin/sh")?;
    command.arg("-c")?.arg("exit 23")?;

    let child = command.spawn(STARTUP_TIMEOUT)?;
    let mut guard = ChildGuard::new(child.process());
    assert_eq!(
        wait_event(child.process())?,
        ChildEvent::Exited {
            pid: child.process(),
            code: 23,
        }
    );
    guard.mark_reaped();
    Ok(())
}

#[test]
fn exec_failure_is_typed_and_child_is_reaped() -> Result<(), Box<dyn Error>> {
    let command = PreparedCommand::new("/definitely/not/an/immortal-executable")?;
    match command.spawn(STARTUP_TIMEOUT) {
        Err(SpawnError::OperatingSystem {
            stage,
            error,
            cleanup_pending,
        }) => {
            assert_eq!(stage, SpawnStage::Execute);
            assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
            assert_eq!(cleanup_pending, None);
        }
        other => {
            return Err(
                io::Error::other(format!("expected typed exec failure, got {other:?}")).into(),
            );
        }
    }
    let error = match wait_any_event_nohang() {
        Err(error) => error,
        Ok(event) => {
            return Err(io::Error::other(format!(
                "failed exec child was not already reaped: {event:?}"
            ))
            .into());
        }
    };
    assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
    Ok(())
}

#[test]
fn environment_directory_and_stdout_mapping_are_materialized() -> Result<(), Box<dyn Error>> {
    // Resolve symlinks so the expected value matches the shell's `$PWD`, which
    // is derived from `getcwd(3)` and is therefore canonical. On macOS `/tmp`
    // is a symlink to `/private/tmp`, so a literal comparison would fail there.
    let working_directory = std::fs::canonicalize("/tmp")?;
    let working_directory = working_directory.to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "temp path is not valid UTF-8")
    })?;
    let pipe = pipe_cloexec()?;
    let (reader, writer) = pipe.into_parts();
    let mut command = PreparedCommand::new("/bin/sh")?;
    command
        .arg("-c")?
        .arg("printf '%s:%s' \"$IMMORTAL_TEST_VALUE\" \"$PWD\"")?;
    command
        .clear_environment()
        .environment("IMMORTAL_TEST_VALUE", "ready")?
        .current_directory(working_directory)?
        .map_descriptor(writer, libc::STDOUT_FILENO)?;

    let child = command.spawn(STARTUP_TIMEOUT)?;
    let mut guard = ChildGuard::new(child.process());
    let mut output = String::new();
    File::from(reader).read_to_string(&mut output)?;
    assert_eq!(output, format!("ready:{working_directory}"));
    assert!(wait_event(child.process())?.is_terminal());
    guard.mark_reaped();
    Ok(())
}

#[test]
fn stdout_and_stderr_are_redirected_independently() -> Result<(), Box<dyn Error>> {
    let stdout_pipe = pipe_cloexec()?;
    let stderr_pipe = pipe_cloexec()?;
    let (stdout_reader, stdout_writer) = stdout_pipe.into_parts();
    let (stderr_reader, stderr_writer) = stderr_pipe.into_parts();
    let mut command = PreparedCommand::new("/bin/sh")?;
    command
        .arg("-c")?
        .arg("printf output; printf diagnostic >&2")?
        .map_descriptor(stdout_writer, libc::STDOUT_FILENO)?
        .map_descriptor(stderr_writer, libc::STDERR_FILENO)?;

    let child = command.spawn(STARTUP_TIMEOUT)?;
    let mut guard = ChildGuard::new(child.process());
    let mut stdout = String::new();
    let mut stderr = String::new();
    File::from(stdout_reader).read_to_string(&mut stdout)?;
    File::from(stderr_reader).read_to_string(&mut stderr)?;
    assert_eq!(stdout, "output");
    assert_eq!(stderr, "diagnostic");
    assert!(wait_event(child.process())?.is_terminal());
    guard.mark_reaped();
    Ok(())
}

#[test]
fn overlapping_descriptor_mappings_preserve_both_sources() -> Result<(), Box<dyn Error>> {
    let first_pipe = pipe_cloexec()?;
    let second_pipe = pipe_cloexec()?;
    let (first_reader, first_writer) = first_pipe.into_parts();
    let (second_reader, second_writer) = second_pipe.into_parts();
    let first_target = second_writer.as_raw_fd();
    let second_target = first_writer.as_raw_fd();

    let mut command = PreparedCommand::new("/bin/sh")?;
    command
        .arg("-c")?
        .arg(format!(
            "printf first >&{first_target}; printf second >&{second_target}"
        ))?
        .map_descriptor(first_writer, first_target)?
        .map_descriptor(second_writer, second_target)?;

    let child = command.spawn(STARTUP_TIMEOUT)?;
    let mut guard = ChildGuard::new(child.process());
    let mut first = String::new();
    let mut second = String::new();
    File::from(first_reader).read_to_string(&mut first)?;
    File::from(second_reader).read_to_string(&mut second)?;
    assert_eq!(first, "first");
    assert_eq!(second, "second");
    assert!(wait_event(child.process())?.is_terminal());
    guard.mark_reaped();
    Ok(())
}

#[test]
fn unintended_non_cloexec_descriptor_is_closed_before_exec() -> Result<(), Box<dyn Error>> {
    let source = File::open("/dev/null")?;
    // SAFETY: F_DUPFD returns a distinct descriptor owned by this test.
    let leaked_raw = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_DUPFD, 64) };
    if leaked_raw == -1 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: successful F_DUPFD transferred a new descriptor to this owner.
    let leaked = unsafe { OwnedFd::from_raw_fd(leaked_raw) };

    let mut command = PreparedCommand::new("/bin/sh")?;
    command.arg("-c")?.arg(format!(
        "if (: >&{leaked_raw}) 2>/dev/null; then exit 41; else exit 0; fi"
    ))?;
    let child = command.spawn(STARTUP_TIMEOUT)?;
    let mut guard = ChildGuard::new(child.process());
    assert_eq!(
        wait_event(child.process())?,
        ChildEvent::Exited {
            pid: child.process(),
            code: 0,
        }
    );
    guard.mark_reaped();
    drop(leaked);
    Ok(())
}

#[test]
fn new_process_group_is_created_before_exec() -> Result<(), Box<dyn Error>> {
    let mut command = PreparedCommand::new("/bin/sleep")?;
    command.arg("30")?.process_group(ProcessGroup::New);
    let child = command.spawn(STARTUP_TIMEOUT)?;
    let mut guard = ChildGuard::new(child.process());
    let group = child
        .process_group()
        .ok_or_else(|| io::Error::other("new group was not returned"))?;
    assert_eq!(group.get(), child.process().get());

    signal_process_group(group, Signal::TERM)?;
    assert_eq!(
        wait_event(child.process())?,
        ChildEvent::Signalled {
            pid: child.process(),
            signal: Signal::TERM,
        }
    );
    guard.mark_reaped();
    Ok(())
}

#[test]
fn owned_group_signal_removes_remaining_descendants() -> Result<(), Box<dyn Error>> {
    let pipe = pipe_cloexec()?;
    let (reader, writer) = pipe.into_parts();
    let report_fd = writer.as_raw_fd();
    let mut command = PreparedCommand::new("/bin/sh")?;
    command
        .arg("-c")?
        .arg(format!("sleep 30 & echo $! >&{report_fd}; wait"))?
        .process_group(ProcessGroup::New)
        .inherit_descriptor(writer)?;
    let child = command.spawn(STARTUP_TIMEOUT)?;
    let group = child
        .process_group()
        .ok_or_else(|| io::Error::other("new group was not returned"))?;
    let mut guard = ProcessGroupGuard::new(child.process(), group);

    let mut descendant_text = String::new();
    BufReader::new(File::from(reader)).read_line(&mut descendant_text)?;
    let descendant_raw: libc::pid_t = descendant_text.trim().parse()?;
    let descendant = ProcessId::new(descendant_raw)
        .ok_or_else(|| io::Error::other("shell reported a nonpositive descendant PID"))?;

    signal_process_group(group, Signal::TERM)?;
    assert!(wait_event(child.process())?.is_terminal());
    guard.mark_reaped();

    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        // SAFETY: signal zero performs a read-only liveness probe in this test;
        // the public Signal type intentionally cannot represent it.
        let result = unsafe { libc::kill(descendant.get(), 0) };
        if result == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            break;
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("descendant remained after process-group cleanup").into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

#[test]
fn explicitly_inherited_descriptor_keeps_its_number() -> Result<(), Box<dyn Error>> {
    let pipe = pipe_cloexec()?;
    let (reader, writer) = pipe.into_parts();
    let inherited = writer.as_raw_fd();
    let mut command = PreparedCommand::new("/bin/sh")?;
    command
        .arg("-c")?
        .arg(format!("printf inherited >&{inherited}"))?
        .inherit_descriptor(writer)?;

    let child = command.spawn(STARTUP_TIMEOUT)?;
    let mut guard = ChildGuard::new(child.process());
    let mut output = String::new();
    File::from(reader).read_to_string(&mut output)?;
    assert_eq!(output, "inherited");
    assert!(wait_event(child.process())?.is_terminal());
    guard.mark_reaped();
    Ok(())
}

#[test]
fn mapped_and_closed_descriptor_actions_cannot_conflict() -> Result<(), Box<dyn Error>> {
    let first = pipe_cloexec()?;
    let (_, first_writer) = first.into_parts();
    let mut mapped_first = PreparedCommand::new("/bin/true")?;
    mapped_first.map_descriptor(first_writer, libc::STDOUT_FILENO)?;
    assert!(mapped_first.close_descriptor(libc::STDOUT_FILENO).is_err());

    let second = pipe_cloexec()?;
    let (_, second_writer) = second.into_parts();
    let mut closed_first = PreparedCommand::new("/bin/true")?;
    closed_first.close_descriptor(libc::STDERR_FILENO)?;
    assert!(
        closed_first
            .map_descriptor(second_writer, libc::STDERR_FILENO)
            .is_err()
    );
    Ok(())
}

#[test]
fn numeric_identity_is_applied_from_precomputed_values() -> Result<(), Box<dyn Error>> {
    // SAFETY: getuid and getgid have no failure mode or pointer arguments.
    let credentials = unsafe {
        ProcessCredentials::new(
            libc::getuid(),
            libc::getgid(),
            SupplementaryGroups::Preserve,
        )
    };
    let expected_user = credentials.user();
    let pipe = pipe_cloexec()?;
    let (reader, writer) = pipe.into_parts();
    let mut command = PreparedCommand::new("/usr/bin/id")?;
    command
        .arg("-u")?
        .credentials(credentials)
        .map_descriptor(writer, libc::STDOUT_FILENO)?;

    let child = command.spawn(STARTUP_TIMEOUT)?;
    let mut guard = ChildGuard::new(child.process());
    let mut output = String::new();
    File::from(reader).read_to_string(&mut output)?;
    assert_eq!(output.trim(), expected_user.to_string());
    assert_eq!(
        wait_event(child.process())?,
        ChildEvent::Exited {
            pid: child.process(),
            code: 0,
        }
    );
    guard.mark_reaped();
    Ok(())
}

#[test]
fn default_child_signal_state_clears_mask_and_ignored_disposition() -> Result<(), Box<dyn Error>> {
    let _guard = ignore_and_block(libc::SIGUSR1)?;
    let mut command = PreparedCommand::new("/bin/sh")?;
    command.arg("-c")?.arg("kill -USR1 $$; exit 99")?;
    let child = command.spawn(STARTUP_TIMEOUT)?;
    let mut child_guard = ChildGuard::new(child.process());
    assert_eq!(
        wait_event(child.process())?,
        ChildEvent::Signalled {
            pid: child.process(),
            signal: Signal::USR1,
        }
    );
    child_guard.mark_reaped();
    Ok(())
}

fn ignore_and_block(signal: libc::c_int) -> io::Result<SignalStateGuard> {
    // SAFETY: each C structure is initialized before it is read.
    let mut ignored: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: sigaction initializes the previous action on success.
    let mut previous_action: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: sigprocmask initializes the previous mask on success.
    let mut previous_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: sigemptyset initializes the new mask before sigaddset/sigprocmask.
    let mut blocked: libc::sigset_t = unsafe { std::mem::zeroed() };
    ignored.sa_sigaction = libc::SIG_IGN;
    ignored.sa_flags = 0;
    // SAFETY: all pointers refer to valid storage for the duration of each call.
    unsafe {
        if libc::sigemptyset(&raw mut ignored.sa_mask) == -1
            || libc::sigaction(signal, &raw const ignored, &raw mut previous_action) == -1
            || libc::sigemptyset(&raw mut blocked) == -1
            || libc::sigaddset(&raw mut blocked, signal) == -1
            || libc::sigprocmask(libc::SIG_BLOCK, &raw const blocked, &raw mut previous_mask) == -1
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(SignalStateGuard {
        signal,
        previous_action,
        previous_mask,
    })
}
