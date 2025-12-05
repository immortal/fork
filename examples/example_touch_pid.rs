#![allow(clippy::expect_used)]
#![allow(clippy::match_wild_err_arm)]

/// run with `cargo run --example example_touch_pid`
use std::{fs::OpenOptions, process::Command};

use fork::{Fork, daemon};

fn main() {
    match daemon(false, false) {
        Ok(Fork::Child) => {
            // Touch a PID file from the daemon process itself
            let file_name = format!("/tmp/{}.pid", std::process::id());
            OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&file_name)
                .expect("failed to open file");

            // Do some work
            Command::new("sleep")
                .arg("300")
                .output()
                .expect("failed to execute process");
        }
        // Parent exits inside daemon(); this arm is unreachable.
        Ok(Fork::Parent(_)) => unreachable!("daemon exits parent processes"),
        Err(err) => eprintln!("daemon failed: {err}"),
    }
}
