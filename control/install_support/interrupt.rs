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
//! omitted. Only a complete line releases the barrier, and a recorded signal
//! ends the wait. `HAMN_TEST_UPDATE_FAULTS` names comma-separated steps that
//! then fail as if their filesystem operation had failed: `host-install`,
//! `receipt-write`, `rollback-link`, `retire-journal`, and every barrier
//! `NAME` (a listed barrier fails instead of pausing). A seam can only pause
//! or fail a step; it never skips a check, a journal write or a rollback.
use super::Result;
use std::{
    ffi::CString,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
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
    match release {
        Some(release) => await_release(&release, pending),
        None => Ok(()),
    }
}

/// How long one wait for a release line lasts before `await_release`
/// checks for a recorded signal again. A signal handled while the wait is
/// in poll ends it at once (EINTR); one handled just before (for example
/// right after the ready line, which is when a test sends it) interrupts
/// nothing, so this bounds how long it goes unnoticed.
const RELEASE_POLL_MILLISECONDS: libc::c_int = 50;

/// Waits for a line on the release FIFO `path`; `Interrupted` once
/// `pending` reports a recorded signal. The FIFO is opened without waiting
/// for a writer, and every wait is one bounded poll, so no blocking call can
/// outlast a signal that arrived before it started. Only a complete line
/// releases: on macOS a read end reports neither a writer that has not
/// opened the FIFO yet nor one that closed it (poll shows no hangup, read
/// returns 0 for both), so neither ends the wait.
fn await_release(path: &Path, pending: impl Fn() -> Option<i32>) -> Result<()> {
    let fd = open(path, libc::O_RDONLY | libc::O_NONBLOCK)?;
    let mut byte = [0u8; 1];
    loop {
        if let Some(signal) = pending() {
            return Err(Interrupted(signal).into());
        }
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // One pollfd that lives across the call.
        match unsafe { libc::poll(&mut poll, 1, RELEASE_POLL_MILLISECONDS) } {
            0 => continue,
            ready if ready < 0 => {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error.into());
                }
                continue;
            }
            _ => {}
        }
        // What has arrived, one byte at a time up to the newline; the
        // buffer is owned here and read never blocks.
        loop {
            match unsafe { libc::read(fd.as_raw_fd(), byte.as_mut_ptr().cast(), 1) } {
                1 if byte[0] == b'\n' => return Ok(()),
                1 => continue,
                0 => break,
                _ => {
                    let error = std::io::Error::last_os_error();
                    match error.kind() {
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted => break,
                        _ => return Err(error.into()),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_support::test_support::Temp;
    use std::{
        io::Write,
        os::unix::fs::OpenOptionsExt,
        path::PathBuf,
        sync::{Arc, mpsc},
        time::{Duration, Instant},
    };

    /// Far above one poll slice, so only a wait that ignores `pending`
    /// (or never returns) exceeds it, even on a loaded machine.
    const BOUND: Duration = Duration::from_secs(10);

    fn release_fifo(temp: &Temp) -> PathBuf {
        let path = temp.0.join("release");
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        // mkfifo reads the NUL-terminated path only.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        path
    }

    /// A writer for `path`, once the wait has opened its read end (a
    /// non-blocking open for writing fails with ENXIO until then).
    fn writer(path: &Path) -> std::fs::File {
        let deadline = Instant::now() + BOUND;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)
            {
                Ok(file) => return file,
                Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                    assert!(Instant::now() < deadline, "the wait never opened {path:?}");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{path:?}: {error}"),
            }
        }
    }

    /// `await_release(path, pending)` on a thread while `act` runs:
    /// `Ok(None)` when released, `Ok(Some(signal))` when interrupted. A wait
    /// still running after `BOUND` fails the test, after a release line
    /// ends it.
    fn bounded_wait(
        path: &Path,
        pending: impl Fn() -> Option<i32> + Send + 'static,
        act: impl FnOnce(),
    ) -> std::result::Result<Option<i32>, String> {
        let (sender, receiver) = mpsc::channel();
        let waited = path.to_path_buf();
        let thread = std::thread::spawn(move || {
            let result = match await_release(&waited, pending) {
                Ok(()) => Ok(None),
                Err(error) => match error.downcast_ref::<Interrupted>() {
                    Some(stopped) => Ok(Some(stopped.0)),
                    None => Err(error.to_string()),
                },
            };
            let _ = sender.send(result);
        });
        act();
        let result = receiver.recv_timeout(BOUND);
        if result.is_err() {
            writer(path).write_all(b"\n").unwrap();
        }
        thread.join().unwrap();
        result.expect("the wait outlasted its bound")
    }

    #[test]
    fn a_signal_recorded_before_the_wait_ends_it() {
        // A signal handled right after the ready line, before the wait
        // blocks, interrupts no system call: only `pending` can show it.
        let temp = Temp::new();
        let path = release_fifo(&temp);
        let result = bounded_wait(&path, || Some(libc::SIGTERM), || {});
        assert_eq!(result, Ok(Some(libc::SIGTERM)));
    }

    #[test]
    fn a_signal_recorded_during_the_wait_ends_it() {
        let temp = Temp::new();
        let path = release_fifo(&temp);
        let recorded = Arc::new(AtomicI32::new(0));
        let seen = Arc::clone(&recorded);
        let pending = move || Some(seen.load(Ordering::SeqCst)).filter(|signal| *signal != 0);
        let result = bounded_wait(&path, pending, || {
            // A writer that leaves without a complete line releases nothing.
            writer(&path).write_all(b"partial").unwrap();
            std::thread::sleep(Duration::from_millis(3 * RELEASE_POLL_MILLISECONDS as u64));
            recorded.store(libc::SIGHUP, Ordering::SeqCst);
        });
        assert_eq!(result, Ok(Some(libc::SIGHUP)));
    }

    #[test]
    fn a_release_line_ends_the_wait() {
        let temp = Temp::new();
        let path = release_fifo(&temp);
        // The writer leaves before the wait reads: the line stays readable.
        let result = bounded_wait(
            &path,
            || None,
            || {
                writer(&path).write_all(b"continue\n").unwrap();
            },
        );
        assert_eq!(result, Ok(None));
    }
}
