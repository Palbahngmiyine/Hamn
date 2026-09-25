//! Child processes: bounded waits without polling sleeps, bounded output
//! capture (Python's `subprocess.run(..., timeout=)`), PATH lookup, and a
//! guard that reaps a test's process session.
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Waits up to `timeout` for `child` to exit and reaps it, like Python's
/// `Popen.wait(timeout)`. Returns `None` on timeout, leaving the child
/// running and unreaped, so its PID cannot be reused before the caller
/// kills or waits for it. A stopped child has not exited.
pub fn wait_timeout(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    if let Some(status) = child.try_wait().expect("wait") {
        return Some(status);
    }
    // SAFETY: kqueue returns a new descriptor or -1.
    let queue = unsafe { libc::kqueue() };
    assert!(queue >= 0, "kqueue: {}", io::Error::last_os_error());
    // SAFETY: queue is a new descriptor owned here.
    let queue = unsafe { OwnedFd::from_raw_fd(queue) };
    let change = libc::kevent {
        ident: child.id() as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: one initialized change and no event buffer.
    let registered = unsafe { libc::kevent(queue.as_raw_fd(), &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
    if registered < 0 {
        let error = io::Error::last_os_error();
        // The child exited after try_wait and is waiting to be reaped.
        assert_eq!(error.raw_os_error(), Some(libc::ESRCH), "kevent: {error}");
        return Some(child.wait().expect("wait"));
    }
    // An exit between try_wait and registration may not raise NOTE_EXIT.
    if let Some(status) = child.try_wait().expect("wait") {
        return Some(status);
    }
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let limit = libc::timespec { tv_sec: remaining.as_secs() as libc::time_t, tv_nsec: remaining.subsec_nanos() as _ };
        // SAFETY: kevent is plain data; the call writes at most one event.
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        // SAFETY: no changes, one writable event, a valid timeout.
        let count = unsafe { libc::kevent(queue.as_raw_fd(), std::ptr::null(), 0, &mut event, 1, &limit) };
        if count < 0 {
            let error = io::Error::last_os_error();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted, "kevent: {error}");
            continue;
        }
        return if count > 0 { Some(child.wait().expect("wait")) } else { child.try_wait().expect("wait") };
    }
}

/// Runs `command` with its output captured, like Python's
/// `subprocess.run(..., capture_output=True, timeout=)`: standard input is
/// inherited, and a command that has not exited and closed its output by
/// `timeout` is killed and fails the caller.
pub fn output_within(command: &mut Command, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {command:?}: {error}"));
    let stdout = drain(child.stdout.take().expect("piped standard output"));
    let stderr = drain(child.stderr.take().expect("piped standard error"));
    let status = wait_timeout(&mut child, timeout);
    // A descendant can hold the pipes open after the child exits.
    let collect = |stream: mpsc::Receiver<io::Result<Vec<u8>>>| {
        stream.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok().map(|data| data.expect("read output"))
    };
    let (stdout, stderr) = (collect(stdout), collect(stderr));
    let (Some(status), Some(stdout), Some(stderr)) = (status, stdout, stderr) else {
        let _ = child.kill();
        let _ = child.wait();
        panic!("{command:?} timed out after {timeout:?}");
    };
    Output { status, stdout, stderr }
}

/// Reads `stream` to its end on a thread and sends the bytes.
fn drain(mut stream: impl Read + Send + 'static) -> mpsc::Receiver<io::Result<Vec<u8>>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut data = Vec::new();
        let _ = sender.send(stream.read_to_end(&mut data).map(|_| data));
    });
    receiver
}

/// Python's `shutil.which(name)`: the first `PATH` entry holding an
/// executable non-directory `name`, joined as given (an empty entry is the
/// working directory).
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|directory| directory.join(name)).find(|candidate| executable(candidate))
}

fn executable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: name is a valid C string.
    let accessible = unsafe { libc::access(name.as_ptr(), libc::X_OK) } == 0;
    accessible && !path.is_dir()
}

/// A test's child session leader. When dropped (the tests' `finally`), a
/// still-running child's process group is killed with SIGKILL and the
/// child is reaped within five seconds.
pub struct Session(pub Child);

impl Session {
    pub fn id(&self) -> u32 {
        self.0.id()
    }

    /// Whether the child is still running (not reaped here or elsewhere).
    pub fn running(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.running() {
            crate::support::pty::kill_group(self.0.id(), libc::SIGKILL);
        }
        // A child already reaped reports an error here, as it has exited.
        if self.0.try_wait().is_ok() && wait_timeout(&mut self.0, Duration::from_secs(5)).is_none() {
            let message = format!("process {} survived SIGKILL for 5 s", self.0.id());
            if std::thread::panicking() {
                eprintln!("{message}");
            } else {
                panic!("{message}");
            }
        }
    }
}

/// Runs a fixture body with Python's exit semantics: an uncaught failure
/// (a panic) exits with status 1, not Rust's 101.
pub fn python_exit(body: impl FnOnce() -> std::process::ExitCode) -> std::process::ExitCode {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).unwrap_or(std::process::ExitCode::from(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_timeout_reports_exit_and_timeout_without_reaping_a_live_child() {
        let mut quick = Command::new("/usr/bin/true").spawn().unwrap();
        assert!(wait_timeout(&mut quick, Duration::from_secs(5)).unwrap().success());
        let mut slow = Command::new("/bin/sleep").arg("5").spawn().unwrap();
        let started = Instant::now();
        assert!(wait_timeout(&mut slow, Duration::from_millis(200)).is_none());
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(slow.try_wait().unwrap().is_none(), "a timed-out child stays unreaped");
        slow.kill().unwrap();
        assert!(!wait_timeout(&mut slow, Duration::from_secs(5)).unwrap().success());
    }

    #[test]
    fn output_within_captures_both_streams() {
        let output =
            output_within(Command::new("/bin/sh").args(["-c", "printf out; printf err >&2; exit 3"]), Duration::from_secs(5));
        assert_eq!((output.status.code(), &output.stdout[..], &output.stderr[..]), (Some(3), &b"out"[..], &b"err"[..]));
    }

    #[test]
    #[should_panic(expected = "timed out")]
    fn output_within_fails_when_a_descendant_holds_the_output_past_the_deadline() {
        output_within(Command::new("/bin/sh").args(["-c", "/bin/sleep 1 & exit 0"]), Duration::from_millis(200));
    }

    #[test]
    #[should_panic(expected = "timed out")]
    fn output_within_kills_a_command_past_its_deadline() {
        output_within(Command::new("/bin/sleep").arg("5"), Duration::from_millis(100));
    }
}
