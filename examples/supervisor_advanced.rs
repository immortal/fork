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

//! Advanced Process Supervisor
//!
//! This example shows how to build a supervisor that:
//! - Tracks supervised processes by stable worker id in a HashMap
//! - Uses PID as a live lookup handle for wait_any_nohang() results
//! - Reaps whichever child exits using wait_any_nohang()
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

use fork::{Fork, fork, wait_any_nohang, waitpid};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct WorkerId(u64);

#[derive(Debug, Clone)]
struct ProcessInfo {
    id: WorkerId,
    name: String,
    pid: libc::pid_t,
    command: Vec<String>,
    started_at: Instant,
    restarts: u32,
    max_restarts: u32,
}

struct Supervisor {
    processes: HashMap<WorkerId, ProcessInfo>,
    by_pid: HashMap<libc::pid_t, WorkerId>,
    next_id: u64,
    shutdown: Arc<AtomicBool>,
}

impl Supervisor {
    fn new() -> Self {
        Self {
            processes: HashMap::new(),
            by_pid: HashMap::new(),
            next_id: 1,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Spawn a new supervised process
    fn spawn(&mut self, name: String, command: Vec<String>) -> std::io::Result<WorkerId> {
        let id = self.allocate_id();
        match fork()? {
            Fork::Parent(pid) => {
                println!("✅ Spawned '{}' with ID: {}, PID: {}", name, id.0, pid);

                self.processes.insert(
                    id,
                    ProcessInfo {
                        id,
                        name: name.clone(),
                        pid,
                        command,
                        started_at: Instant::now(),
                        restarts: 0,
                        max_restarts: 3,
                    },
                );
                self.by_pid.insert(pid, id);

                Ok(id)
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

    fn allocate_id(&mut self) -> WorkerId {
        let id = WorkerId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Handle a process exit
    fn handle_exit(&mut self, pid: libc::pid_t) {
        if let Some(id) = self.by_pid.remove(&pid) {
            let Some(info) = self.processes.remove(&id) else {
                return;
            };

            let uptime = info.started_at.elapsed();
            let name = info.name.clone();
            let restarts = info.restarts;
            let max_restarts = info.max_restarts;

            println!(
                "\n💀 Process '{}' (ID: {}, PID: {}) exited after {:.2}s",
                name,
                info.id.0,
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
        let command = info.command.clone();
        info.restarts += 1;
        info.started_at = Instant::now();

        match fork()? {
            Fork::Parent(pid) => {
                info.pid = pid;
                self.by_pid.insert(pid, info.id);
                self.processes.insert(info.id, info);
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

    /// Poll all children for exits without blocking.
    fn wait_for_exit(&mut self) -> std::io::Result<()> {
        // Reap whichever children exited and collect their PIDs first, then
        // handle them so the restart path can mutate the process map.
        let mut exited = Vec::new();
        loop {
            match wait_any_nohang() {
                Ok(Some((pid, _status))) => {
                    exited.push(pid);
                }
                Ok(None) => {
                    break;
                }
                Err(e) if e.raw_os_error() == Some(libc::ECHILD) => {
                    break;
                }
                Err(e) => return Err(e),
            }
        }

        for pid in exited {
            self.handle_exit(pid);
        }

        // Back off briefly between polls.
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
        println!("┌──────┬─────────────────┬──────────┬──────────┬──────────┐");
        println!("│ ID   │ Name            │ PID      │ Uptime   │ Restarts │");
        println!("├──────┼─────────────────┼──────────┼──────────┼──────────┤");

        for info in self.processes.values() {
            let uptime = format!("{}s", info.started_at.elapsed().as_secs());
            println!(
                "│ {:4} │ {:15} │ {:8} │ {:>8} │ {:8} │",
                info.id.0, info.name, info.pid, uptime, info.restarts
            );
        }
        println!("└──────┴─────────────────┴──────────┴──────────┴──────────┘\n");
    }

    /// Shutdown all supervised processes
    fn shutdown(&mut self) {
        println!("\n🛑 Shutting down supervisor...");
        self.shutdown.store(true, Ordering::Relaxed);

        // Send SIGTERM to all children (not shown - would use libc::kill)
        // Then wait for them to exit gracefully

        for info in self.processes.values() {
            println!("  Stopping '{}' (PID: {})", info.name, info.pid);
            // In production: unsafe { libc::kill(info.pid, libc::SIGTERM) };
            let _ = waitpid(info.pid);
        }

        self.by_pid.clear();
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
