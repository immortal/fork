//! Safe process-broker example.
//!
//! Spawns a child in its own process group through the additive, safe
//! `PreparedCommand` API, then terminates the whole group and reports the
//! resulting typed `ChildEvent`. Every string, argument array, and descriptor
//! is materialized before the fork, and the exec-status pipe is close-on-exec.
//!
//! This example doubles as a compile-time regression guard: it exercises the
//! public broker surface end to end, so an accidental signature change breaks
//! the build.
//!
//! Run with: `cargo run --example process_broker`

use std::time::Duration;

use fork::{ChildEvent, PreparedCommand, ProcessGroup, Signal, signal_process_group, wait_event};

fn main() -> std::io::Result<()> {
    // Build the command entirely in the parent, before any fork.
    let mut command = PreparedCommand::new("/bin/sleep")?;
    command.arg("30")?.process_group(ProcessGroup::New);

    // Spawn with a bounded startup handshake: success means execve completed.
    let child = match command.spawn(Duration::from_secs(5)) {
        Ok(child) => child,
        Err(error) => {
            eprintln!("spawn failed: {error}");
            return Ok(());
        }
    };
    println!(
        "spawned pid {} in a new process group",
        child.process().get()
    );

    // Terminate every member of the owned process group.
    if let Some(group) = child.process_group() {
        println!("signalling process group {group} with TERM");
        signal_process_group(group, Signal::TERM)?;
    }

    // Observe the outcome as a typed event instead of a raw status integer.
    match wait_event(child.process())? {
        ChildEvent::Signalled { pid, signal } => {
            println!("pid {} terminated by signal {signal}", pid.get());
        }
        ChildEvent::Exited { pid, code } => {
            println!("pid {} exited with code {code}", pid.get());
        }
        other => println!("unexpected event: {other:?}"),
    }

    Ok(())
}
