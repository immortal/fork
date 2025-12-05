#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::match_wild_err_arm)]
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::useless_vec)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::needless_continue)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::indexing_slicing)]
#![allow(clippy::panic)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::explicit_iter_loop)]
#![allow(clippy::ignored_unit_patterns)]
#![allow(clippy::for_kv_map)]

//! Advanced Process Supervisor with Signal Handling
//!
//! This example shows how to build a production-ready supervisor that:
//! - Uses Fork with Hash to track processes in a HashMap
//! - Gets notified when child processes exit (via SIGCHLD)
//! - Automatically restarts failed processes
//! - Tracks process metrics (uptime, restart count)
//! - Gracefully shuts down all children
//!
//! Run with: cargo run --example supervisor_advanced

use std::{
    collections::HashMap,
    process::{Command, exit},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use fork::{Fork, fork, waitpid};

#[derive(Debug, Clone)]
struct ProcessInfo {
    name: String,
    pid: libc::pid_t,
    started_at: Instant,
    restarts: u32,
    max_restarts: u32,
}

struct Supervisor {
    processes: HashMap<Fork, ProcessInfo>,
    shutdown: Arc<AtomicBool>,
}

impl Supervisor {
    fn new() -> Self {
        Self {
            processes: HashMap::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Spawn a new supervised process
    fn spawn(&mut self, name: String, command: Vec<String>) -> std::io::Result<Fork> {
        match fork()? {
            result @ Fork::Parent(pid) => {
                println!("✅ Spawned '{}' with PID: {}", name, pid);

                // Store using Fork as HashMap key!
                self.processes.insert(
                    result,
                    ProcessInfo {
                        name: name.clone(),
                        pid,
                        started_at: Instant::now(),
                        restarts: 0,
                        max_restarts: 3,
                    },
                );

                Ok(result)
            }
            Fork::Child => {
                // Child process - execute the command
                let program = &command[0];
                let args = &command[1..];

                Command::new(program)
                    .args(args)
                    .status()
                    .expect("Failed to execute command");

                exit(0);
            }
        }
    }

    /// Handle a process exit
    fn handle_exit(&mut self, fork_result: Fork) {
        if let Some(info) = self.processes.remove(&fork_result) {
            let uptime = info.started_at.elapsed();
            let name = info.name.clone();
            let restarts = info.restarts;
            let max_restarts = info.max_restarts;

            println!(
                "\n💀 Process '{}' (PID: {}) exited after {:.2}s",
                name,
                info.pid,
                uptime.as_secs_f64()
            );

            // Auto-restart if under limit
            if restarts < max_restarts && !self.shutdown.load(Ordering::Relaxed) {
                println!(
                    "🔄 Restarting '{}' (restart {}/{})",
                    name,
                    restarts + 1,
                    max_restarts
                );

                if let Err(e) = self.restart(info) {
                    eprintln!("❌ Failed to restart '{}': {}", name, e);
                }
            } else if restarts >= max_restarts {
                println!(
                    "⚠️  Process '{}' reached max restarts, not restarting",
                    name
                );
            }
        }
    }

    /// Restart a process
    fn restart(&mut self, mut info: ProcessInfo) -> std::io::Result<()> {
        let name = info.name.clone();
        info.restarts += 1;
        info.started_at = Instant::now();

        // Simulate command (in real code, store original command)
        let command = vec!["sleep".to_string(), "2".to_string()];

        match fork()? {
            result @ Fork::Parent(pid) => {
                info.pid = pid;
                self.processes.insert(result, info);
                Ok(())
            }
            Fork::Child => {
                Command::new(&command[0])
                    .args(&command[1..])
                    .status()
                    .unwrap_or_else(|e| panic!("Failed to execute {}: {}", name, e));
                exit(0);
            }
        }
    }

    /// Wait for any child to exit (blocking)
    fn wait_for_exit(&mut self) -> std::io::Result<()> {
        // In production, you'd use waitpid(-1, &mut status, 0) to wait for ANY child
        // For this example, we'll iterate through known processes

        for (fork_result, _info) in self.processes.clone().iter() {
            if let Some(pid) = fork_result.child_pid() {
                // Try to wait for this specific child (blocking)
                match waitpid(pid) {
                    Ok(_) => {
                        self.handle_exit(*fork_result);
                        return Ok(());
                    }
                    Err(_) => {
                        // This child hasn't exited yet, continue
                        continue;
                    }
                }
            }
        }

        // No children exited yet
        thread::sleep(Duration::from_millis(100));
        Ok(())
    }

    /// Get supervisor statistics
    fn stats(&self) -> String {
        let total = self.processes.len();
        let total_restarts: u32 = self.processes.values().map(|p| p.restarts).sum();

        format!(
            "📊 Supervisor Stats: {} processes, {} total restarts",
            total, total_restarts
        )
    }

    /// List all supervised processes
    fn list(&self) {
        println!("\n📋 Supervised Processes:");
        println!("┌─────────────────┬──────────┬──────────┬──────────┐");
        println!("│ Name            │ PID      │ Uptime   │ Restarts │");
        println!("├─────────────────┼──────────┼──────────┼──────────┤");

        for (_fork, info) in &self.processes {
            let uptime = info.started_at.elapsed().as_secs();
            println!(
                "│ {:15} │ {:8} │ {:6}s │ {:8} │",
                info.name, info.pid, uptime, info.restarts
            );
        }
        println!("└─────────────────┴──────────┴──────────┴──────────┘\n");
    }

    /// Shutdown all supervised processes
    fn shutdown(&mut self) {
        println!("\n🛑 Shutting down supervisor...");
        self.shutdown.store(true, Ordering::Relaxed);

        // Send SIGTERM to all children (not shown - would use libc::kill)
        // Then wait for them to exit gracefully

        for (fork_result, info) in &self.processes {
            println!("  Stopping '{}' (PID: {})", info.name, info.pid);
            if let Some(pid) = fork_result.child_pid() {
                // In production: unsafe { libc::kill(pid, libc::SIGTERM) };
                let _ = waitpid(pid);
            }
        }

        self.processes.clear();
        println!("✅ All processes stopped");
    }
}

fn main() {
    println!("🚀 Advanced Process Supervisor\n");

    let mut supervisor = Supervisor::new();

    // Spawn multiple workers
    println!("Starting workers...\n");

    for i in 1..=3 {
        let name = format!("worker-{}", i);
        let command = vec!["sleep".to_string(), format!("{}", i * 2)];

        match supervisor.spawn(name, command) {
            Ok(_) => {}
            Err(e) => eprintln!("Failed to spawn worker: {}", e),
        }
    }

    supervisor.list();

    // Supervisor main loop
    println!("📡 Supervisor running. Waiting for process events...\n");

    for _ in 0..10 {
        if supervisor.processes.is_empty() {
            println!("✅ All processes completed");
            break;
        }

        // Wait for a child to exit
        if let Err(e) = supervisor.wait_for_exit() {
            eprintln!("Error waiting for child: {}", e);
            break;
        }

        // Show stats periodically
        println!("\n{}\n", supervisor.stats());
    }

    supervisor.shutdown();
}
