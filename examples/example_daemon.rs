#![allow(clippy::expect_used)]
#![allow(clippy::match_wild_err_arm)]

/// run with `cargo run --example example_daemon`
use std::process::Command;

use fork::{Fork, daemon};

fn main() {
    // Keep stdio open (noclose = true) so we can print the daemon PID
    match daemon(false, true) {
        Ok(Fork::Child) => {
            println!("daemon pid: {}", std::process::id());
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
