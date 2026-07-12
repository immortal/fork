//! Checked daemon startup with an explicit readiness notification.

use std::{fs, time::Duration};

use fork::{CheckedDaemon, DaemonOptions, checked_daemon};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let marker = std::env::temp_dir().join(format!(
        "fork_checked_daemon_pattern_{}.marker",
        std::process::id()
    ));
    let _ = fs::remove_file(&marker);

    let mut options = DaemonOptions::new();
    options
        .current_directory("/")?
        .redirect_standard_io_to_null()?;

    match checked_daemon(options, Duration::from_secs(3))? {
        CheckedDaemon::Parent(process) => {
            println!(
                "daemon {} reported ready in process group {}",
                process.process(),
                process.process_group()
            );
            let contents = fs::read_to_string(&marker)?;
            println!("marker: {}", contents.trim());
            fs::remove_file(marker)?;
        }
        CheckedDaemon::Daemon(notifier) => {
            let process = notifier.process();
            if let Err(error) = fs::write(&marker, format!("daemon pid={}\n", process.process())) {
                notifier.fail_and_exit(&error);
            }
            if notifier.notify_ready().is_err() {
                daemon_exit(1);
            }
            daemon_exit(0);
        }
    }
    Ok(())
}

fn daemon_exit(code: libc::c_int) -> ! {
    // SAFETY: the example daemon exits without running the invoking process's
    // inherited destructors or buffered output a second time.
    unsafe { libc::_exit(code) }
}
