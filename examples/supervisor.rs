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
//! Demonstrates how to build a simple process supervisor that tracks child
//! processes by durable worker id, uses PID as a live lookup handle, and
//! detects exits without blocking.
//!
//! Run with: cargo run --example supervisor

use std::{
    collections::HashMap,
    process::{Command, exit},
    time::{Duration, Instant},
};

use fork::{Fork, WEXITSTATUS, WIFEXITED, fork, wait_any_nohang};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct WorkerId(u64);

#[derive(Debug)]
struct ProcessInfo {
    id: WorkerId,
    worker_num: u32,
    name: String,
    pid: libc::pid_t,
    started_at: Instant,
    restarts: u32,
}

fn main() {
    println!("🚀 Starting Process Supervisor\n");

    let mut supervised: HashMap<WorkerId, ProcessInfo> = HashMap::new();
    let mut by_pid: HashMap<libc::pid_t, WorkerId> = HashMap::new();

    // Spawn 3 worker processes
    for i in 1_u32..=3 {
        let id = WorkerId(u64::from(i));
        match spawn_worker(id, i, &mut supervised, &mut by_pid) {
            Ok(_) => println!("✅ Worker {} spawned", i),
            Err(e) => eprintln!("❌ Failed to spawn worker {}: {}", i, e),
        }
    }

    println!("\n📊 Supervisor managing {} processes\n", supervised.len());

    // Supervisor loop - poll for child exits without blocking.
    loop {
        if supervised.is_empty() {
            println!("✅ All workers completed. Supervisor exiting.");
            break;
        }

        // Reap whichever children exited. The returned PID is a live lookup
        // handle into by_pid; WorkerId is the durable supervisor identity.
        let mut exited = Vec::new();
        loop {
            match wait_any_nohang() {
                Ok(Some((pid, status))) => {
                    if let Some(id) = by_pid.get(&pid) {
                        let info = supervised
                            .get(id)
                            .expect("pid map points to supervised worker");
                        let uptime = info.started_at.elapsed();
                        if WIFEXITED(status) {
                            println!(
                                "\n💀 Worker '{}' (ID: {}, PID: {}) exited with code {} after {:.2}s",
                                info.name,
                                info.id.0,
                                info.pid,
                                WEXITSTATUS(status),
                                uptime.as_secs_f64()
                            );
                        } else {
                            println!(
                                "\n💀 Worker '{}' (ID: {}, PID: {}) terminated after {:.2}s",
                                info.name,
                                info.id.0,
                                info.pid,
                                uptime.as_secs_f64()
                            );
                        }
                    }
                    exited.push(pid);
                }
                Ok(None) => break,
                Err(e) if e.raw_os_error() == Some(libc::ECHILD) => {
                    // No unwaited-for children remain.
                    break;
                }
                Err(e) => {
                    eprintln!("⚠️  wait_any_nohang failed: {}", e);
                    break;
                }
            }
        }

        // Handle exited processes
        for pid in exited {
            if let Some(id) = by_pid.remove(&pid) {
                let Some(info) = supervised.remove(&id) else {
                    continue;
                };

                // Optional: Restart the worker
                if info.restarts < 3 {
                    println!("🔄 Restarting worker '{}'...", info.name);

                    match restart_worker(
                        info.id,
                        info.worker_num,
                        &mut supervised,
                        &mut by_pid,
                        info.restarts + 1,
                    ) {
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

        // Poll interval. In production prefer a SIGCHLD handler over polling.
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn spawn_worker(
    id: WorkerId,
    worker_num: u32,
    supervised: &mut HashMap<WorkerId, ProcessInfo>,
    by_pid: &mut HashMap<libc::pid_t, WorkerId>,
) -> std::io::Result<()> {
    spawn_worker_with_restart_count(id, worker_num, supervised, by_pid, 0)
}

fn restart_worker(
    id: WorkerId,
    worker_num: u32,
    supervised: &mut HashMap<WorkerId, ProcessInfo>,
    by_pid: &mut HashMap<libc::pid_t, WorkerId>,
    restart_count: u32,
) -> std::io::Result<()> {
    spawn_worker_with_restart_count(id, worker_num, supervised, by_pid, restart_count)
}

fn spawn_worker_with_restart_count(
    id: WorkerId,
    worker_num: u32,
    supervised: &mut HashMap<WorkerId, ProcessInfo>,
    by_pid: &mut HashMap<libc::pid_t, WorkerId>,
    restart_count: u32,
) -> std::io::Result<()> {
    match fork()? {
        Fork::Parent(pid) => {
            supervised.insert(
                id,
                ProcessInfo {
                    id,
                    worker_num,
                    name: format!("worker-{}", worker_num),
                    pid,
                    started_at: Instant::now(),
                    restarts: restart_count,
                },
            );
            by_pid.insert(pid, id);
            Ok(())
        }
        Fork::Child => {
            // Worker process - simulate some work
            if restart_count == 0 {
                println!("👷 Worker {} starting work...", worker_num);
            } else {
                println!(
                    "👷 Worker {} (restart #{}) starting work...",
                    worker_num, restart_count
                );
            }

            // Simulate different work durations
            Command::new("sleep")
                .arg(if restart_count == 0 {
                    worker_num.to_string()
                } else {
                    "1".to_string()
                })
                .status()
                .expect("Failed to execute sleep");

            if restart_count == 0 {
                println!("✅ Worker {} completed", worker_num);
            } else {
                println!(
                    "✅ Worker {} (restart #{}) completed",
                    worker_num, restart_count
                );
            }
            exit(0);
        }
    }
}
