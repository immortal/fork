//! A non-SIGCHLD callback must never run in the helper's ownership context.

use std::{
    io, ptr,
    sync::atomic::{AtomicI32, Ordering},
    time::Instant,
};

use fork::{ProcessGroupGuard, Signal};

use super::{BOUND, POLL, install};

static OWNER: AtomicI32 = AtomicI32::new(0);

extern "C" fn reap_in_helper(_: libc::c_int) {
    // SAFETY: only async-signal-safe operations. The owner's handler deliberately
    // does not collect anything, so this is not a competing owner waiter.
    unsafe {
        if libc::getpid() != OWNER.load(Ordering::Relaxed) {
            while libc::waitpid(-1, ptr::null_mut(), libc::WNOHANG) > 0 {}
        }
    }
}

pub(super) fn run(owner_loss: bool) -> Result<(), Box<dyn std::error::Error>> {
    OWNER.store(i32::try_from(std::process::id())?, Ordering::Relaxed);
    // USR1 defaults to exit; URG defaults to ignore. The helper must remove
    // the application callback from both and ignore them, so it stays alive
    // and keeps its pin whatever the signal's default action is.
    for raw in [libc::SIGUSR1, libc::SIGURG] {
        let _action = install(raw, reap_in_helper as *const () as usize, 0)?;
        let _ignored = install(libc::SIGUSR2, libc::SIG_IGN, 0)?;
        let guard = ProcessGroupGuard::new()?;
        let group = guard.process_group();
        let helper = guard.guard_process();
        wait_zombie(group.get())?;
        fork::signal_process(helper, Signal::USR2)?; // ignored stays ignored
        fork::signal_process(helper, Signal::try_from(raw)?)?;
        // Neither signal has an acknowledgement, so a bounded scheduling
        // window permits a surviving callback to remove the anchor, or a
        // default action to end the helper, before exercising all paths.
        std::thread::sleep(std::time::Duration::from_millis(100));
        // SAFETY: a query only; owner disposition must be unchanged.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        if unsafe { libc::sigaction(raw, ptr::null(), &raw mut action) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        assert_eq!(action.sa_sigaction, reap_in_helper as *const () as usize);
        if owner_loss {
            drop(guard);
        } else {
            // The helper survived with its anchor, so both must succeed.
            guard.signal_group(Signal::TERM, BOUND)?;
            guard.disarm(BOUND)?;
        }
    }
    Ok(())
}

fn wait_zombie(pid: i32) -> io::Result<()> {
    let deadline = Instant::now() + BOUND;
    loop {
        if std::fs::read_to_string(format!("/proc/{pid}/status"))?
            .lines()
            .any(|line| line.starts_with("State:") && line.contains('Z'))
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            // Old USR1 handler keeps the helper alive; still exercise cleanup
            // so the delivery-suppressing oracle records the lost pin.
            return Ok(());
        }
        std::thread::sleep(POLL);
    }
}
