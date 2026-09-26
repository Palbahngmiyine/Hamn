//! Bounded child processes for the release tooling.
//!
//! Every command runs under a deadline enforced here, where the child was
//! started: on expiry the child is killed and reaped before the error is
//! returned, so no caller relies on an outer timeout alone. A grandchild
//! that keeps the output pipes open past the deadline also ends the wait
//! with an error rather than blocking forever.
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A finished child: its status and its complete output.
#[derive(Debug)]
pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    /// The exit code, or `None` when a signal ended the child.
    pub fn code(&self) -> Option<i32> {
        self.status.code()
    }

    pub fn stdout_text(&self) -> Result<String, String> {
        String::from_utf8(self.stdout.clone()).map_err(|error| format!("command output is not UTF-8: {error}"))
    }

    pub fn stderr_lossy(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// How a child is started.
#[derive(Default)]
pub struct Spec<'a> {
    /// Exactly these environment variables; `None` inherits ours.
    pub environment: Option<&'a BTreeMap<String, String>>,
    /// Bytes written to standard input, which is otherwise empty.
    pub input: Option<&'a [u8]>,
}

/// Runs `program args` to completion within `timeout`. Only a failure to
/// start, a timeout or unreadable output is an error; any exit status is
/// returned for the caller to judge.
pub fn capture<S: AsRef<OsStr>>(program: &OsStr, args: &[S], spec: &Spec, timeout: Duration) -> Result<Output, String> {
    execute(program, args, spec, timeout, true)
}

/// Like [`run`], but the child's standard error goes straight to ours, so a
/// long build's progress and warnings stay visible; only standard output is
/// captured, and a failure carries its tail alone.
pub fn run_passing_stderr<S: AsRef<OsStr>>(
    program: &OsStr,
    args: &[S],
    spec: &Spec,
    timeout: Duration,
) -> Result<String, String> {
    checked(program, execute(program, args, spec, timeout, false)?)
}

fn execute<S: AsRef<OsStr>>(
    program: &OsStr,
    args: &[S],
    spec: &Spec,
    timeout: Duration,
    capture_stderr: bool,
) -> Result<Output, String> {
    let name = program.to_string_lossy().into_owned();
    let mut command = Command::new(program);
    command.args(args).stdout(Stdio::piped());
    command.stderr(if capture_stderr { Stdio::piped() } else { Stdio::inherit() });
    command.stdin(if spec.input.is_some() { Stdio::piped() } else { Stdio::null() });
    if let Some(environment) = spec.environment {
        command.env_clear().envs(environment);
    }
    let deadline = Instant::now() + timeout;
    let mut child = command.spawn().map_err(|error| format!("{name}: {error}"))?;
    let (sender, receiver) = mpsc::channel::<(usize, std::io::Result<Vec<u8>>)>();
    for (index, stream) in [
        child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
        child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .enumerate()
    {
        let Some(mut stream) = stream else {
            assert!(index == 1 && !capture_stderr, "standard output is always piped");
            continue;
        };
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut data = Vec::new();
            let result = stream.read_to_end(&mut data).map(|_| data);
            let _ = sender.send((index, result));
        });
    }
    if let (Some(input), Some(mut stdin)) = (spec.input, child.stdin.take()) {
        let input = input.to_vec();
        // A child that exits without reading its input is judged by its
        // status, not by the broken pipe.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{name}: wait: {error}"));
            }
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{name} timed out after {}s", timeout.as_secs()));
        };
        std::thread::sleep(remaining.min(Duration::from_millis(10)));
    };
    // An inherited standard error has no reader and stays empty here.
    let mut outputs: [Option<Vec<u8>>; 2] = [None, if capture_stderr { None } else { Some(Vec::new()) }];
    while outputs.iter().any(Option::is_none) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok((index, Ok(data))) => outputs[index] = Some(data),
            Ok((_, Err(error))) => return Err(format!("{name}: read output: {error}")),
            Err(_) => return Err(format!("{name} exited but its output stayed open past {}s", timeout.as_secs())),
        }
    }
    let [stdout, stderr] = outputs.map(Option::unwrap_or_default);
    Ok(Output { status, stdout, stderr })
}

/// Runs a command that must succeed and returns its standard output. A
/// failure names the command and carries the tails of both streams.
pub fn run<S: AsRef<OsStr>>(program: &OsStr, args: &[S], spec: &Spec, timeout: Duration) -> Result<String, String> {
    checked(program, capture(program, args, spec, timeout)?)
}

fn checked(program: &OsStr, output: Output) -> Result<String, String> {
    if !output.status.success() {
        let status = output.code().map_or_else(|| output.status.to_string(), |code| code.to_string());
        return Err(format!(
            "{} failed ({status}): {} {}",
            program.to_string_lossy(),
            tail(&output.stderr_lossy(), 4096),
            tail(&String::from_utf8_lossy(&output.stdout), 4096)
        ));
    }
    output.stdout_text()
}

/// The last `limit` bytes of `text`, cut at a character boundary.
pub fn tail(text: &str, limit: usize) -> &str {
    let mut start = text.len().saturating_sub(limit);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, spec: &Spec, timeout: Duration) -> Result<Output, String> {
        capture(OsStr::new("/bin/sh"), &["-c", script], spec, timeout)
    }

    #[test]
    fn output_status_and_input_are_returned() {
        let output =
            sh("cat; echo err >&2; exit 3", &Spec { input: Some(b"in"), ..Spec::default() }, Duration::from_secs(10))
                .unwrap();
        assert_eq!(
            (output.code(), output.stdout.as_slice(), output.stderr.as_slice()),
            (Some(3), &b"in"[..], &b"err\n"[..])
        );
        let failure = run(
            OsStr::new("/bin/sh"),
            &["-c", "echo out; echo bad >&2; exit 4"],
            &Spec::default(),
            Duration::from_secs(10),
        )
        .unwrap_err();
        assert!(failure.contains("failed (4)") && failure.contains("bad") && failure.contains("out"), "{failure}");
    }

    #[test]
    fn exact_environment_replaces_the_inherited_one() {
        let environment = BTreeMap::from([("ONLY".to_owned(), "value".to_owned())]);
        let output = capture(
            OsStr::new("/usr/bin/env"),
            &[] as &[&str],
            &Spec { environment: Some(&environment), ..Spec::default() },
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(output.stdout, b"ONLY=value\n");
    }

    #[test]
    fn deadline_kills_the_child_and_a_held_pipe_cannot_block() {
        let started = Instant::now();
        let error = sh("sleep 30", &Spec::default(), Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        // The shell exits at once; its background grandchild keeps stdout.
        let error = sh("sleep 5 & exit 0", &Spec::default(), Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("output stayed open"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(
            capture(OsStr::new("/nonexistent/program"), &[] as &[&str], &Spec::default(), Duration::from_secs(1))
                .is_err()
        );
    }

    #[test]
    fn passed_through_stderr_keeps_status_output_and_deadline() {
        let run = |script: &str, timeout: Duration| {
            run_passing_stderr(OsStr::new("/bin/sh"), &["-c", script], &Spec::default(), timeout)
        };
        let seconds = Duration::from_secs(10);
        assert_eq!(run("echo out; echo passed-through >&2", seconds).unwrap(), "out\n");
        let failure = run("echo partial; exit 5", seconds).unwrap_err();
        assert!(failure.contains("failed (5)") && failure.contains("partial"), "{failure}");
        let started = Instant::now();
        let error = run("sleep 30", Duration::from_millis(300)).unwrap_err();
        assert!(error.contains("timed out") && started.elapsed() < Duration::from_secs(10), "{error}");
    }

    #[test]
    fn tail_keeps_character_boundaries() {
        assert_eq!(tail("abc", 2), "bc");
        assert_eq!(tail("aé", 1), "");
        assert_eq!(tail("ab", 10), "ab");
    }
}
