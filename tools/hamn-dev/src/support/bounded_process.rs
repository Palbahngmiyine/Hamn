//! Child processes waited for within a deadline, like Python's
//! `subprocess.run(..., timeout=...)` and `Popen.communicate(timeout=...)`.
//! Output pipes are drained while waiting, so a full pipe cannot stall the
//! child past its deadline.
use super::pty;
use std::io::{ErrorKind, Read};
use std::os::fd::AsRawFd;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

/// What a child wrote to its piped stdout and stderr so far.
#[derive(Debug, Default)]
pub struct Captured {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Reads the child's piped stdout and stderr (those that are piped) into
/// `captured` until end of file, then waits for the child to exit, all
/// within `timeout`. On timeout it returns `None` and leaves the child
/// running with its unread pipes attached, so a caller can kill it and call
/// again for the rest of its output. A pipe that reached end of file is
/// closed and removed from the child.
pub fn communicate(child: &mut Child, captured: &mut Captured, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    let mut buffer = vec![0u8; 65536];
    loop {
        let stdout = child.stdout.as_ref().map(AsRawFd::as_raw_fd);
        let stderr = child.stderr.as_ref().map(AsRawFd::as_raw_fd);
        let open: Vec<i32> = stdout.into_iter().chain(stderr).collect();
        if open.is_empty() {
            break;
        }
        let remaining = deadline.checked_duration_since(Instant::now())?;
        let ready = pty::readable(&open, remaining);
        if ready.is_empty() {
            // The loop re-reads the clock, so an early wakeup is not a timeout.
            continue;
        }
        for fd in ready {
            let (pipe, sink): (&mut dyn Read, &mut Vec<u8>) = if Some(fd) == stdout {
                (child.stdout.as_mut().expect("stdout pipe"), &mut captured.stdout)
            } else {
                (child.stderr.as_mut().expect("stderr pipe"), &mut captured.stderr)
            };
            // poll reported the pipe readable, so this read does not block.
            let count = match pipe.read(&mut buffer) {
                Ok(count) => count,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => panic!("read child output: {error}"),
            };
            sink.extend_from_slice(&buffer[..count]);
            if count == 0 {
                if Some(fd) == stdout {
                    child.stdout = None;
                } else {
                    child.stderr = None;
                }
            }
        }
    }
    // Both pipes are closed; the exit normally follows at once. std has no
    // bounded wait, so poll the status as Python's Popen.wait(timeout) does.
    loop {
        if let Some(status) = child.try_wait().expect("wait for child") {
            return Some(status);
        }
        let remaining = deadline.checked_duration_since(Instant::now())?;
        std::thread::sleep(remaining.min(Duration::from_millis(5)));
    }
}

/// Runs `command` with piped stdout and stderr (stdin inherited), like
/// `subprocess.run(capture_output=True, timeout=...)`. A child still running
/// at the deadline is killed and reaped, and the call panics.
pub fn output(command: &mut Command, timeout: Duration) -> Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let (status, captured) = finish(command, timeout);
    Output { status, stdout: captured.stdout, stderr: captured.stderr }
}

/// Runs `command` with inherited standard streams, like
/// `subprocess.run(..., timeout=...)`; panics after killing and reaping a
/// child still running at the deadline.
pub fn status(command: &mut Command, timeout: Duration) -> ExitStatus {
    finish(command, timeout).0
}

fn finish(command: &mut Command, timeout: Duration) -> (ExitStatus, Captured) {
    let mut child = command.spawn().unwrap_or_else(|error| panic!("spawn {command:?}: {error}"));
    let mut captured = Captured::default();
    match communicate(&mut child, &mut captured, timeout) {
        Some(status) => (status, captured),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "{command:?} did not finish within {timeout:?}; stdout={:?} stderr={:?}",
                String::from_utf8_lossy(&captured.stdout),
                String::from_utf8_lossy(&captured.stderr)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_collects_both_pipes_and_the_status() {
        let result = output(Command::new("/bin/sh").args(["-c", "echo out; echo err >&2; exit 3"]), Duration::from_secs(10));
        assert_eq!(result.status.code(), Some(3));
        assert_eq!(result.stdout, b"out\n");
        assert_eq!(result.stderr, b"err\n");
    }

    #[test]
    fn communicate_times_out_and_resumes_after_kill() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "echo started; exec sleep 30"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut captured = Captured::default();
        // Output read by one timed-out call is kept for the next; wait for the
        // line first so that a slow shell cannot be killed before it echoes.
        let deadline = Instant::now() + Duration::from_secs(10);
        while captured.stdout.is_empty() {
            assert!(Instant::now() < deadline, "the child never wrote its first line");
            assert!(communicate(&mut child, &mut captured, Duration::from_millis(50)).is_none());
        }
        let started = Instant::now();
        assert!(communicate(&mut child, &mut captured, Duration::from_millis(300)).is_none());
        assert!(started.elapsed() >= Duration::from_millis(300));
        child.kill().unwrap();
        let status = communicate(&mut child, &mut captured, Duration::from_secs(10)).expect("killed child exits");
        assert!(!status.success());
        assert_eq!(captured.stdout, b"started\n");
    }

    #[test]
    #[should_panic(expected = "did not finish within")]
    fn output_panics_when_the_deadline_passes() {
        output(Command::new("/bin/sleep").arg("30"), Duration::from_millis(200));
    }
}
