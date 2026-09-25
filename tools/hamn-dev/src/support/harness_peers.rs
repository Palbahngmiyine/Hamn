//! Helpers for the Harness-based TUI suites and the fixtures they select:
//! switching a harness CLI peer to another fixture, the notice/gate FIFO
//! protocol seen from a fixture, a one-shot event for loopback servers, a
//! bounded external command and a PATH lookup.
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

/// Makes `root/bin/<cli>` act as `fixture` from its next start. The
/// selection file is replaced by a rename, so a peer starting concurrently
/// sees the old or the new fixture, never an empty name.
pub fn select_peer(root: &Path, cli: &str, fixture: &str) {
    let stage = root.join(format!(".fixture-{cli}.tmp"));
    fs::write(&stage, fixture).unwrap_or_else(|error| panic!("{}: {error}", stage.display()));
    fs::rename(&stage, root.join(format!("fixture-{cli}"))).unwrap();
}

/// From a fixture: writes `message` to the harness notice FIFO, which the
/// test holds open, so the open does not block.
pub fn notify(root: &Path, message: &str) {
    let mut notice = OpenOptions::new().write(true).open(root.join("notice")).unwrap();
    notice.write_all(message.as_bytes()).unwrap();
}

/// From a fixture: blocks until the test writes one gate byte. Like the
/// former Python fixtures, an end-of-file (the test closed the gate) also
/// returns.
pub fn wait_gate(root: &Path) {
    let mut gate = File::open(root.join("gate")).unwrap();
    let mut byte = [0u8; 1];
    loop {
        match gate.read(&mut byte) {
            Ok(_) => return,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => panic!("gate: {error}"),
        }
    }
}

/// Writes one byte to the harness gate when dropped, releasing a fixture
/// that still waits (the Python suites did this in `finally`). Declared
/// after the harness, it is dropped before the harness closes.
pub struct GateOnDrop(OwnedFd);

impl GateOnDrop {
    pub fn new(gate: &OwnedFd) -> Self {
        Self(gate.try_clone().expect("duplicate gate descriptor"))
    }
}

impl Drop for GateOnDrop {
    fn drop(&mut self) {
        // SAFETY: the buffer is one readable byte. A full non-blocking FIFO
        // already holds a release byte, so a failed write is not an error.
        unsafe { libc::write(self.0.as_raw_fd(), b"1".as_ptr().cast(), 1) };
    }
}

/// A one-shot flag with a bounded wait (Python's `threading.Event`).
#[derive(Default)]
pub struct Event {
    set: Mutex<bool>,
    changed: Condvar,
}

impl Event {
    pub fn set(&self) {
        *self.set.lock().unwrap() = true;
        self.changed.notify_all();
    }

    /// Waits up to `timeout` and returns whether the event is set.
    pub fn wait(&self, timeout: Duration) -> bool {
        let guard = self.set.lock().unwrap();
        let (guard, _) = self.changed.wait_timeout_while(guard, timeout, |set| !*set).unwrap();
        *guard
    }
}

/// Runs `command` with captured stdout and stderr (stdin is inherited, as
/// with Python's `subprocess.run(capture_output=True)`), killing it and
/// failing after `timeout`.
pub fn run_bounded(command: &mut Command, timeout: Duration) -> Output {
    let child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn");
    let pid = child.id();
    let (done, finished) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let output = child.wait_with_output();
        let _ = done.send(());
        output
    });
    if finished.recv_timeout(timeout).is_err() {
        // The waiter has not reaped the child, so the PID is still its own.
        super::pty::kill(pid, libc::SIGKILL);
        let _ = waiter.join();
        panic!("{command:?} did not finish within {timeout:?}");
    }
    waiter.join().unwrap().expect("wait")
}

/// The first executable `name` on `PATH` (Python's `shutil.which`).
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_else(|| "/bin:/usr/bin".into());
    std::env::split_paths(&path).map(|directory| directory.join(name)).find(|candidate| {
        let executable = CString::new(candidate.as_os_str().as_bytes()).unwrap();
        // SAFETY: access only reads the NUL-terminated path.
        candidate.is_file() && unsafe { libc::access(executable.as_ptr(), libc::X_OK) } == 0
    })
}

/// Creates a FIFO and opens its read end non-blocking. Unlike the harness
/// FIFOs it is not also a writer, so the last writer's close is visible.
pub fn fifo_reader(path: &Path) -> OwnedFd {
    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is a valid C string.
    let made = unsafe { libc::mkfifo(name.as_ptr(), 0o666) };
    assert_eq!(made, 0, "mkfifo {}: {}", path.display(), io::Error::last_os_error());
    // SAFETY: as above; open returns a new descriptor or -1.
    let fd = unsafe { libc::open(name.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    assert!(fd >= 0, "open {}: {}", path.display(), io::Error::last_os_error());
    // SAFETY: fd is a new descriptor owned here.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

/// Waits up to `timeout` for `fd` to become readable using select(2). On
/// macOS, poll(2) (`pty::readable`) does not report a FIFO whose last
/// writer closed, while select(2) reports it readable at end-of-file.
pub fn select_readable(fd: RawFd, timeout: Duration) -> bool {
    assert!((0..libc::FD_SETSIZE as RawFd).contains(&fd), "descriptor {fd} out of select range");
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let mut wait = libc::timeval {
            tv_sec: remaining.as_secs() as libc::time_t,
            tv_usec: remaining.subsec_micros() as libc::suseconds_t,
        };
        // SAFETY: fd_set is plain data; FD_ZERO and FD_SET initialize it for
        // a descriptor below FD_SETSIZE.
        let mut set: libc::fd_set = unsafe { std::mem::zeroed() };
        unsafe {
            libc::FD_ZERO(&mut set);
            libc::FD_SET(fd, &mut set);
        }
        // SAFETY: select reads and writes only the set and the timeval.
        let result = unsafe { libc::select(fd + 1, &mut set, std::ptr::null_mut(), std::ptr::null_mut(), &mut wait) };
        if result >= 0 {
            // SAFETY: the set was filled in by select above.
            return result > 0 && unsafe { libc::FD_ISSET(fd, &set) };
        }
        let error = io::Error::last_os_error();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted, "select: {error}");
    }
}

/// One `read(2)` of at most `limit` bytes (Python's `os.read`); an empty
/// result is end-of-file. A non-blocking descriptor without data fails.
pub fn read_up_to(fd: RawFd, limit: usize) -> Vec<u8> {
    let mut buffer = vec![0u8; limit];
    loop {
        // SAFETY: buffer is writable for its length.
        let count = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
        if count >= 0 {
            buffer.truncate(count as usize);
            return buffer;
        }
        let error = io::Error::last_os_error();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted, "read: {error}");
    }
}
