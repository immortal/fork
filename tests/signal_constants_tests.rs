//! Regression guard for the portable `Signal` constants.
//!
//! `cargo semver-checks` verifies that the constants still exist, but it cannot
//! verify that each one still maps to the correct operating-system signal
//! number. These pure, deterministic assertions catch an accidental edit,
//! reorder, or typo (for example swapping `HUP` and `INT`) that would silently
//! change delivered-signal behavior for downstream supervisors.

use fork::Signal;

#[test]
fn portable_constants_map_to_exact_os_numbers() {
    let pairs: [(Signal, libc::c_int); 13] = [
        (Signal::HUP, libc::SIGHUP),
        (Signal::INT, libc::SIGINT),
        (Signal::QUIT, libc::SIGQUIT),
        (Signal::KILL, libc::SIGKILL),
        (Signal::ALRM, libc::SIGALRM),
        (Signal::TERM, libc::SIGTERM),
        (Signal::STOP, libc::SIGSTOP),
        (Signal::CONT, libc::SIGCONT),
        (Signal::USR1, libc::SIGUSR1),
        (Signal::USR2, libc::SIGUSR2),
        (Signal::TTIN, libc::SIGTTIN),
        (Signal::TTOU, libc::SIGTTOU),
        (Signal::WINCH, libc::SIGWINCH),
    ];
    for (signal, expected) in pairs {
        assert_eq!(signal.get(), expected);
    }
}

#[test]
fn new_rejects_zero_and_negative_but_accepts_positive() {
    assert_eq!(Signal::new(0), None);
    assert_eq!(Signal::new(-1), None);
    assert!(Signal::new(libc::SIGUSR1).is_some());
}

#[test]
fn try_from_round_trips_and_reports_rejected_value() {
    let accepted = Signal::try_from(libc::SIGTERM);
    assert!(accepted.is_ok());
    if let Ok(signal) = accepted {
        assert_eq!(signal, Signal::TERM);
        assert_eq!(libc::c_int::from(signal), libc::SIGTERM);
    }

    let rejected = Signal::try_from(0);
    assert!(rejected.is_err());
    if let Err(error) = rejected {
        assert_eq!(error.raw(), 0);
    }
}
