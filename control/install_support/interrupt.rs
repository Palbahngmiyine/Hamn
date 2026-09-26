//! HUP, INT and TERM during an update transaction, and test-only seams.
//!
//! Signals. Once an update journal is durable, `arm` records these signals
//! instead of terminating: the transaction checks `check` between steps
//! (and blocking waits return EINTR, since SA_RESTART is not set), rolls the
//! journal back and exits with 128 + the signal number. Before arming and
//! after `disarm`, the default dispositions apply; a process killed before
//! the journal exists changed nothing, and one killed later (including by
//! SIGKILL) leaves the journal for the next run's recovery.
//!
//! Seams (never used by releases, only by regression suites). A barrier
//! `NAME` pauses a transaction at one named point:
//! `HAMN_TEST_UPDATE_<NAME>_READY_FIFO` receives `ready\n` and then a line
//! is awaited from `HAMN_TEST_UPDATE_<NAME>_RELEASE_FIFO`; either may be
//! omitted. `HAMN_TEST_UPDATE_FAULTS` names comma-separated steps that then
//! fail as if their filesystem operation had failed: `host-install`,
//! `receipt-write`, `rollback-link`, `retire-journal`, and every barrier
//! `NAME` (a listed barrier fails instead of pausing). A seam can only pause
//! or fail a step; it never skips a check, a journal write or a rollback.
use super::Result;
use std::{
    ffi::CString,
    os::{
        fd::{FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
    sync::atomic::{AtomicI32, Ordering},
};

static PENDING: AtomicI32 = AtomicI32::new(0);

extern "C" fn record(signal: libc::c_int) {
    // A lock-free atomic store is async-signal-safe.
    PENDING.store(signal, Ordering::SeqCst);
}

const SIGNALS: [libc::c_int; 3] = [libc::SIGHUP, libc::SIGINT, libc::SIGTERM];

fn install(handler: libc::sighandler_t) {
    for signal in SIGNALS {
        // The action is fully initialized; `record` only stores an atomic.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handler;
            libc::sigemptyset(&mut action.sa_mask);
            action.sa_flags = 0;
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

/// Record HUP/INT/TERM from now on (see the module documentation).
pub(super) fn arm() {
    install(record as extern "C" fn(libc::c_int) as libc::sighandler_t);
}

/// Restore the default (terminating) dispositions. A signal recorded while
/// armed is then delivered again, so it still ends the process.
pub(super) fn disarm() {
    install(libc::SIG_DFL);
    let signal = PENDING.swap(0, Ordering::SeqCst);
    if signal != 0 {
        // Re-raise with the default disposition: the process ends as the
        // sender intended, after the committed state is complete.
        unsafe { libc::raise(signal) };
    }
}

pub(super) fn pending() -> Option<i32> {
    match PENDING.load(Ordering::SeqCst) {
        0 => None,
        signal => Some(signal),
    }
}

/// A recorded signal stopped the current step.
#[derive(Debug)]
pub(super) struct Interrupted(pub(super) i32);

impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "interrupted by {}", name(self.0))
    }
}

impl std::error::Error for Interrupted {}

pub(super) fn check() -> Result<()> {
    match pending() {
        Some(signal) => Err(Interrupted(signal).into()),
        None => Ok(()),
    }
}

pub(super) fn name(signal: i32) -> &'static str {
    match signal {
        libc::SIGHUP => "HUP",
        libc::SIGINT => "INT",
        libc::SIGTERM => "TERM",
        _ => "a signal",
    }
}

/// The shell convention for a process ended by `signal`.
pub(super) fn exit_status(signal: i32) -> i32 {
    128 + signal
}

/// Fails with an injected error when `step` is listed in
/// `HAMN_TEST_UPDATE_FAULTS` (see the module documentation).
pub(super) fn fault(step: &str) -> Result<()> {
    let listed = std::env::var("HAMN_TEST_UPDATE_FAULTS")
        .is_ok_and(|faults| faults.split(',').any(|name| name == step));
    if listed {
        return Err(format!("injected {step} fault").into());
    }
    Ok(())
}

fn open(path: &Path, flags: libc::c_int) -> Result<OwnedFd> {
    let name = CString::new(path.as_os_str().as_bytes())?;
    loop {
        // open returns a new descriptor owned below, or -1.
        let fd = unsafe { libc::open(name.as_ptr(), flags | libc::O_CLOEXEC) };
        if fd >= 0 {
            return Ok(unsafe { OwnedFd::from_raw_fd(fd) });
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.into());
        }
        check()?;
    }
}

fn fifo(variable: &str) -> Result<Option<std::path::PathBuf>> {
    let Some(value) = std::env::var_os(variable).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let path = std::path::PathBuf::from(value);
    use std::os::unix::fs::FileTypeExt;
    if !std::fs::metadata(&path).is_ok_and(|m| m.file_type().is_fifo()) {
        return Err(format!("{variable} is not a FIFO").into());
    }
    Ok(Some(path))
}

/// The test barrier `name` (see the module documentation). Interruption by
/// a recorded signal returns `Interrupted`.
pub(super) fn barrier(name: &str) -> Result<()> {
    fault(name)?;
    let ready = fifo(&format!("HAMN_TEST_UPDATE_{name}_READY_FIFO"))?;
    let release = fifo(&format!("HAMN_TEST_UPDATE_{name}_RELEASE_FIFO"))?;
    if let Some(ready) = ready {
        let fd = open(&ready, libc::O_WRONLY)?;
        let mut file = std::fs::File::from(fd);
        std::io::Write::write_all(&mut file, b"ready\n")?;
    }
    let Some(release) = release else {
        return Ok(());
    };
    let fd = open(&release, libc::O_RDONLY)?;
    let mut byte = [0u8; 1];
    loop {
        // One byte at a time up to the newline; the buffer is owned here.
        let count = unsafe {
            libc::read(
                std::os::fd::AsRawFd::as_raw_fd(&fd),
                byte.as_mut_ptr().cast(),
                1,
            )
        };
        match count {
            1 if byte[0] == b'\n' => return Ok(()),
            1 => continue,
            0 => {
                return Err(
                    format!("test barrier {name} was closed without a release line").into(),
                );
            }
            _ => {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error.into());
                }
                check()?;
            }
        }
    }
}
