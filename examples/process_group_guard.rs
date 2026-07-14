//! Guard one direct-exec workload against unexpected broker death.

use std::{error::Error, time::Duration};

use fork::{PreparedCommand, ProcessGroup, ProcessGroupGuard, wait_event};

fn main() -> Result<(), Box<dyn Error>> {
    let timeout = Duration::from_secs(3);
    let mut guard = ProcessGroupGuard::new()?;
    let mut command = PreparedCommand::new("/bin/sleep")?;
    command
        .arg("5")?
        .process_group(ProcessGroup::Join(guard.process_group()));
    let child = command.spawn(timeout)?;

    guard.activate(timeout)?;
    let event = wait_event(child.process())?;
    guard.disarm(timeout)?;
    println!("workload finished: {event:?}");
    Ok(())
}
