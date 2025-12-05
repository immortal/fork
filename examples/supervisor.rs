#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::match_wild_err_arm)]
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::ignored_unit_patterns)]
#![allow(clippy::for_kv_map)]
#![allow(clippy::double_ended_iterator_last)]
#![allow(clippy::single_match_else)]

//! Process Supervisor Example
//!
//! Demonstrates how to use Fork with Hash to build a simple process supervisor
//! that tracks multiple child processes and gets notified when they exit.
//!
//! Run with: cargo run --example supervisor

use std::{
    collections::HashMap,
    process::{Command, exit},
    time::{Duration, Instant},
};

use fork::{Fork, fork, waitpid};

#[derive(Debug)]
struct ProcessInfo {
    name: String,
    started_at: Instant,
    restarts: u32,
}

fn main() {
    println!("🚀 Starting Process Supervisor\n");

    // HashMap using Fork as the key!
    let mut supervised: HashMap<Fork, ProcessInfo> = HashMap::new();

    // Spawn 3 worker processes
    for i in 1..=3 {
        match spawn_worker(i, &mut supervised) {
            Ok(_) => println!("✅ Worker {} spawned", i),
            Err(e) => eprintln!("❌ Failed to spawn worker {}: {}", i, e),
        }
    }

    println!("\n📊 Supervisor managing {} processes\n", supervised.len());

    // Supervisor loop - wait for children to exit
    loop {
        if supervised.is_empty() {
            println!("✅ All workers completed. Supervisor exiting.");
            break;
        }

        // Check each supervised process
        let mut exited = Vec::new();

        for (fork_result, info) in &supervised {
            if let Some(pid) = fork_result.child_pid() {
                // Try non-blocking wait to see if process exited
                // Note: In real code, you'd use WNOHANG with waitpid
                // For this example, we'll simulate with a simple check
                println!("⏳ Checking worker '{}' (PID: {})", info.name, pid);
            }
        }

        // In a real supervisor, you'd use signal handlers (SIGCHLD)
        // or non-blocking waitpid with WNOHANG to detect exits
        // For this demo, we'll wait for any child
        std::thread::sleep(Duration::from_millis(500));

        // Simple approach: try to find which child exited
        // In production, use SIGCHLD signal handler
        for (fork_result, _info) in &supervised {
            if let Some(pid) = fork_result.child_pid() {
                // Check if this specific child exited (blocking wait)
                // In real code, use waitpid with WNOHANG
                match waitpid(pid) {
                    Ok(_) => {
                        exited.push(*fork_result);
                    }
                    Err(_) => {
                        // Process still running or error
                    }
                }
            }
        }

        // Handle exited processes
        for fork_result in exited {
            if let Some(info) = supervised.remove(&fork_result) {
                let pid = fork_result.child_pid().unwrap();
                let uptime = info.started_at.elapsed();

                println!(
                    "\n💀 Worker '{}' (PID: {}) exited after {:.2}s",
                    info.name,
                    pid,
                    uptime.as_secs_f64()
                );

                // Optional: Restart the worker
                if info.restarts < 3 {
                    println!("🔄 Restarting worker '{}'...", info.name);
                    let worker_num: u32 = info
                        .name
                        .split('-')
                        .last()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);

                    match restart_worker(worker_num, &mut supervised, info.restarts + 1) {
                        Ok(_) => println!("✅ Worker '{}' restarted", info.name),
                        Err(e) => eprintln!("❌ Failed to restart: {}", e),
                    }
                } else {
                    println!(
                        "⚠️  Worker '{}' reached max restarts, not restarting",
                        info.name
                    );
                }
            }
        }
    }
}

fn spawn_worker(id: u32, supervised: &mut HashMap<Fork, ProcessInfo>) -> std::io::Result<()> {
    match fork()? {
        result @ Fork::Parent(_) => {
            // Store in HashMap using Fork as key!
            supervised.insert(
                result,
                ProcessInfo {
                    name: format!("worker-{}", id),
                    started_at: Instant::now(),
                    restarts: 0,
                },
            );
            Ok(())
        }
        Fork::Child => {
            // Worker process - simulate some work
            println!("👷 Worker {} starting work...", id);

            // Simulate different work durations
            Command::new("sleep")
                .arg(format!("{}", id))
                .status()
                .expect("Failed to execute sleep");

            println!("✅ Worker {} completed", id);
            exit(0);
        }
    }
}

fn restart_worker(
    id: u32,
    supervised: &mut HashMap<Fork, ProcessInfo>,
    restart_count: u32,
) -> std::io::Result<()> {
    match fork()? {
        result @ Fork::Parent(_) => {
            supervised.insert(
                result,
                ProcessInfo {
                    name: format!("worker-{}", id),
                    started_at: Instant::now(),
                    restarts: restart_count,
                },
            );
            Ok(())
        }
        Fork::Child => {
            println!(
                "👷 Worker {} (restart #{}) starting work...",
                id, restart_count
            );

            Command::new("sleep")
                .arg("1")
                .status()
                .expect("Failed to execute sleep");

            println!("✅ Worker {} (restart #{}) completed", id, restart_count);
            exit(0);
        }
    }
}
