//! Installed CLIs (docker, kubectl) that suites compare with Hamn: PATH
//! lookup like `shutil.which`, bounded runs like `subprocess.run(...,
//! timeout=...)`, and wrapper fixtures that exec the real CLI from a test's
//! private `bin/`, the way the Python suites' `os.execv` wrappers did.
use super::pty;
use super::py_text;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

/// `shutil.which(name)` with this process's PATH: the first PATH entry
/// holding an executable regular file (or a link to one) called `name`.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|directory| directory.join(name)).find(|candidate| {
        fs::metadata(candidate).is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    })
}

/// A finished process, as `subprocess.run(..., capture_output=True)`
/// returns it.
#[derive(Debug)]
pub struct Output {
    /// The exit status, or the negated signal number (Python's convention).
    pub returncode: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    /// Standard output as `text=True` decodes it.
    pub fn stdout(&self) -> String {
        py_text::universal_newlines(&self.stdout)
    }

    pub fn stderr(&self) -> String {
        py_text::universal_newlines(&self.stderr)
    }
}

/// Python's `returncode` for a status: the exit code, or `-signal`.
pub fn returncode(status: ExitStatus) -> i32 {
    status.code().unwrap_or_else(|| -status.signal().expect("a status without a code has a signal"))
}

/// Runs `command` with captured output (standard input inherited, as
/// Python does), failing if it is still running after `timeout`; the child
/// is then killed and reaped first.
pub fn run(command: &mut Command, timeout: Duration) -> Output {
    let child = command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("spawn");
    communicate(child, timeout)
}

/// Like `run`, failing unless the exit status is zero (`check=True`).
pub fn run_checked(command: &mut Command, timeout: Duration) -> Output {
    let output = run(command, timeout);
    assert_eq!(output.returncode, 0, "{command:?} failed: {output:?}");
    output
}

/// How much of each stream a `communicate` failure reports: the end, where
/// a stuck child's last progress and error lines are.
const REPORTED_TAIL: usize = 8 * 1024;

/// How long the streams of a timed-out child may take to end after it was
/// killed and reaped (a descendant can hold them open) before the failure
/// reports what arrived. It bounds only the report, not the timeout.
const DRAIN_AFTER_KILL: Duration = Duration::from_secs(1);

/// Reads a started child's piped standard output and error to their ends and
/// waits for it, failing after `timeout` (`Popen.communicate(timeout=...)`):
/// both the exit and the end of both streams (which a descendant can hold
/// open) must arrive in time. A child still running then is killed and
/// reaped first. Either failure reports the last `REPORTED_TAIL` bytes of
/// each stream read by then.
pub fn communicate(mut child: Child, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;
    // Each reader appends as data arrives, so a failure can report output
    // from a child that never finished; the index sent marks a stream's end.
    let captured: [Arc<Mutex<Vec<u8>>>; 2] = Default::default();
    let (sender, receiver) = mpsc::channel::<usize>();
    let streams: [Option<Box<dyn Read + Send>>; 2] = [
        child.stdout.take().map(|stream| Box::new(stream) as Box<dyn Read + Send>),
        child.stderr.take().map(|stream| Box::new(stream) as Box<dyn Read + Send>),
    ];
    for (index, stream) in streams.into_iter().enumerate() {
        let (sender, captured) = (sender.clone(), Arc::clone(&captured[index]));
        std::thread::spawn(move || {
            if let Some(stream) = stream {
                capture(stream, &captured);
            }
            let _ = sender.send(index);
        });
    }
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if Instant::now() >= deadline {
            let id = child.id();
            // The child is unreaped, so its pid still names it.
            let _ = child.kill();
            let _ = child.wait();
            let ended = await_ends(&receiver, Instant::now() + DRAIN_AFTER_KILL);
            panic!("process {id} timed out after {timeout:?}; it was killed and reaped\n{}", report(&captured, ended));
        }
        // A bounded poll of an owned child; try_wait has no timeout form.
        std::thread::sleep(Duration::from_millis(5));
    };
    let ended = await_ends(&receiver, deadline);
    if ended != [true; 2] {
        panic!("output of process {} still open after {timeout:?}\n{}", child.id(), report(&captured, ended));
    }
    let [stdout, stderr] = captured.map(|stream| std::mem::take(&mut *locked(&stream)));
    Output { returncode: returncode(status), stdout, stderr }
}

/// Appends everything read from `stream` to `captured` as it arrives.
fn capture(mut stream: impl Read, captured: &Mutex<Vec<u8>>) {
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(count) => locked(captured).extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => panic!("read child output: {error}"),
        }
    }
}

/// A capture buffer; a reader that panicked leaves its bytes usable.
fn locked(captured: &Mutex<Vec<u8>>) -> std::sync::MutexGuard<'_, Vec<u8>> {
    captured.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Which of standard output and error ended by `until`.
fn await_ends(receiver: &mpsc::Receiver<usize>, until: Instant) -> [bool; 2] {
    let mut ended = [false; 2];
    while ended != [true; 2] {
        match receiver.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok(index) => ended[index] = true,
            Err(_) => break,
        }
    }
    ended
}

/// The end of each captured stream for a failure message: its last
/// `REPORTED_TAIL` bytes, the total read, and whether it had ended.
fn report(captured: &[Arc<Mutex<Vec<u8>>>; 2], ended: [bool; 2]) -> String {
    let mut text = String::new();
    for ((name, captured), ended) in ["stdout", "stderr"].into_iter().zip(captured).zip(ended) {
        let data = locked(captured);
        let tail = &data[data.len().saturating_sub(REPORTED_TAIL)..];
        let state = if ended { "ended" } else { "still open" };
        text.push_str(&format!("--- {name}: last {} of {} bytes, {state} ---\n", tail.len(), data.len()));
        text.push_str(&String::from_utf8_lossy(tail));
        if !tail.is_empty() && !tail.ends_with(b"\n") {
            text.push('\n');
        }
    }
    text
}

/// Reads `child`'s piped standard output until `marker` appears, failing
/// after `timeout`. Returns what was read.
pub fn read_until(child: &mut Child, marker: &[u8], timeout: Duration) -> Vec<u8> {
    use std::os::fd::AsRawFd;
    let fd = child.stdout.as_ref().expect("piped standard output").as_raw_fd();
    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    while !output.windows(marker.len()).any(|window| window == marker) {
        let remaining = deadline.checked_duration_since(Instant::now());
        let remaining = remaining.unwrap_or_else(|| panic!("timed out: {}", String::from_utf8_lossy(&output)));
        assert!(!pty::readable(&[fd], remaining).is_empty(), "timed out: {}", String::from_utf8_lossy(&output));
        let data = pty::read_some(fd);
        assert!(!data.is_empty(), "{}", String::from_utf8_lossy(&output));
        output.extend_from_slice(&data);
    }
    output
}

/// Kills and reaps a child still running when dropped (a test's `finally`).
/// It never panics, since it also runs while a failed case unwinds.
pub struct Reaped(pub Option<Child>);

impl Drop for Reaped {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take()
            && child.try_wait().ok().flatten().is_none()
        {
            // SIGKILL cannot be caught, so the wait ends.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Makes `root/bin/<cli>` (a fixture link) run `fixture` from now on, which
/// execs `real` through `exec_real`. The selection is replaced atomically so
/// a concurrently starting fixture reads the old or the new choice.
pub fn wrap(root: &Path, cli: &str, real: &Path, fixture: &str) {
    fs::write(root.join(format!("real-{cli}")), real.as_os_str().as_bytes()).unwrap();
    select(root, cli, fixture);
}

/// Selects `fixture` for `root/bin/<program>`, replacing any earlier choice
/// atomically.
pub fn select(root: &Path, program: &str, fixture: &str) {
    let temporary = root.join(format!(".fixture-{program}.new"));
    fs::write(&temporary, fixture).unwrap();
    fs::rename(&temporary, root.join(format!("fixture-{program}"))).unwrap();
}

/// In a wrapper fixture: the real CLI that `wrap` recorded.
pub fn real(cli: &str) -> PathBuf {
    let root = PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"));
    let path = fs::read(root.join(format!("real-{cli}"))).unwrap_or_else(|error| panic!("real {cli}: {error}"));
    PathBuf::from(std::ffi::OsStr::from_bytes(&path))
}

/// The fixture `wrap` selects for a plain wrapper: execs the real CLI of
/// its program name with the same arguments (`os.execv(real, [real] +
/// sys.argv[1:])`).
pub fn exec_real(program: &str, args: &[String]) -> std::process::ExitCode {
    exec(Command::new(real(program)).args(args))
}

/// Replaces this process with `command` (`os.execv`); returns only by
/// panicking when exec fails.
pub fn exec(command: &mut Command) -> ! {
    let error = command.exec();
    panic!("exec {command:?}: {error}");
}

/// A command for the real Docker CLI with `args`, never inheriting a user's
/// Docker target (see `without_docker_variables`).
pub fn docker_command(real: &Path, args: &[String]) -> Command {
    let mut command = Command::new(real);
    command.args(args);
    without_docker_variables(&mut command);
    command
}

/// Removes every inherited `DOCKER_*` variable from `command`'s environment
/// and sets `DOCKER_API_VERSION=1.47`.
pub fn without_docker_variables(command: &mut Command) -> &mut Command {
    let keys: Vec<OsString> =
        std::env::vars_os().map(|(key, _)| key).filter(|key| key.as_bytes().starts_with(b"DOCKER_")).collect();
    for key in keys {
        command.env_remove(key);
    }
    command.env("DOCKER_API_VERSION", "1.47")
}

/// Writes `line` and its newline to `out` as one buffer, so with one `write`
/// call on a file that takes the whole of it.
///
/// A record file is appended to by several fixture processes at once: the
/// program under test runs its CLIs concurrently. An append is atomic per
/// `write` call only. `writeln!` on a `File` makes a call for each formatted
/// piece (the text, then the newline; one per JSON token for a
/// `serde_json::Value`), and two processes can interleave those pieces into a
/// line that is no record.
pub fn write_line(out: &mut impl Write, line: &str) -> io::Result<()> {
    out.write_all(format!("{line}\n").as_bytes())
}

/// Appends one line to `path`, creating it, as `write_line` writes it: a
/// reader, and a process appending at the same time, see whole lines only.
pub fn append_line(path: &Path, line: &str) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    write_line(&mut file, line).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// The lines of `path` parsed as JSON string lists (a recorded argv log).
pub fn recorded_argv(path: &Path) -> Vec<Vec<String>> {
    let text = fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    py_text::splitlines(&text).into_iter().map(|line| serde_json::from_str(line).unwrap()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::tmp::TempDir;

    /// Records each `write` call it receives.
    struct Calls(Vec<Vec<u8>>);

    impl Write for Calls {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.push(bytes.to_vec());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The message of the panic `f` must raise.
    fn panic_message(f: impl FnOnce()) -> String {
        let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).expect_err("expected a panic");
        match payload.downcast::<String>() {
            Ok(message) => *message,
            Err(payload) => payload.downcast_ref::<&str>().expect("a text panic").to_string(),
        }
    }

    /// Kills a test child's whole process group when dropped, so a
    /// descendant holding its pipes cannot outlive the test.
    struct GroupKill(libc::pid_t);

    impl Drop for GroupKill {
        fn drop(&mut self) {
            // SAFETY: killpg takes no pointers; ESRCH (group gone) is fine.
            unsafe { libc::killpg(self.0, libc::SIGKILL) };
        }
    }

    /// Starts `script` (`$1` names a marker it must create once everything
    /// it writes before that is written) under /bin/sh in a new process
    /// group with piped output, and waits for the marker.
    fn started(directory: &Path, script: &str) -> (Child, GroupKill) {
        let marker = directory.join("marker");
        let child = Command::new("/bin/sh")
            .args(["-c", script, "sh"])
            .arg(&marker)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let group = GroupKill(child.id() as libc::pid_t);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !marker.exists() {
            assert!(Instant::now() < deadline, "{script:?} did not reach its marker");
            std::thread::sleep(Duration::from_millis(5));
        }
        (child, group)
    }

    #[test]
    fn a_timed_out_child_is_reaped_and_its_output_reported() {
        let directory = TempDir::new("hamn-communicate-");
        let (child, _group) = started(directory.path(), "echo started; echo failing >&2; : > \"$1\"; exec sleep 30");
        let pid = child.id() as libc::pid_t;
        let message = panic_message(|| {
            communicate(child, Duration::from_millis(100));
        });
        assert!(message.starts_with(&format!("process {pid} timed out after 100ms; it was killed and reaped\n")), "{message}");
        assert!(message.contains("--- stdout: last 8 of 8 bytes, ended ---\nstarted\n"), "{message}");
        assert!(message.contains("--- stderr: last 8 of 8 bytes, ended ---\nfailing\n"), "{message}");
        // Reaped: no zombie keeps the pid.
        // SAFETY: kill with signal 0 only checks for the process.
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[test]
    fn a_descendant_holding_the_output_delays_a_timeout_report_only_briefly() {
        let directory = TempDir::new("hamn-communicate-");
        let (child, _group) = started(directory.path(), "echo started; sleep 30 & : > \"$1\"; exec sleep 30");
        let begun = Instant::now();
        let message = panic_message(|| {
            communicate(child, Duration::from_millis(100));
        });
        let elapsed = begun.elapsed();
        assert!(elapsed >= DRAIN_AFTER_KILL && elapsed < Duration::from_secs(10), "{elapsed:?}");
        assert!(message.contains("--- stdout: last 8 of 8 bytes, still open ---\nstarted\n"), "{message}");
        assert!(message.contains("--- stderr: last 0 of 0 bytes, still open ---\n"), "{message}");
    }

    #[test]
    fn output_held_open_past_the_deadline_is_reported() {
        let directory = TempDir::new("hamn-communicate-");
        let (child, _group) = started(directory.path(), "echo started; sleep 30 & : > \"$1\"");
        let pid = child.id();
        let message = panic_message(|| {
            communicate(child, Duration::from_millis(500));
        });
        assert!(message.starts_with(&format!("output of process {pid} still open after 500ms\n")), "{message}");
        assert!(message.contains("--- stdout: last 8 of 8 bytes, still open ---\nstarted\n"), "{message}");
    }

    #[test]
    fn a_report_keeps_the_end_of_long_output() {
        let mut stdout = b"first\n".to_vec();
        stdout.resize(REPORTED_TAIL * 3, b'x');
        stdout.extend_from_slice(b"\nlast\n");
        let captured = [Arc::new(Mutex::new(stdout.clone())), Arc::new(Mutex::new(b"partial".to_vec()))];
        let tail = String::from_utf8_lossy(&stdout[stdout.len() - REPORTED_TAIL..]).into_owned();
        assert_eq!(
            report(&captured, [true, false]),
            format!(
                "--- stdout: last {REPORTED_TAIL} of {} bytes, ended ---\n{tail}--- stderr: last 7 of 7 bytes, still open ---\npartial\n",
                stdout.len()
            )
        );
    }

    #[test]
    fn a_finished_run_returns_all_of_its_output() {
        let script = "head -c 100000 /dev/zero; printf done >&2; exit 3";
        let output = run(Command::new("/bin/sh").args(["-c", script]), Duration::from_secs(30));
        assert_eq!((output.returncode, output.stdout.len(), output.stderr), (3, 100_000, b"done".to_vec()));
    }

    #[test]
    fn a_record_line_is_one_write() {
        let mut calls = Calls(Vec::new());
        let record = serde_json::json!(["docker", ["ps", "--format", "{{json .}}"]]).to_string();
        write_line(&mut calls, &record).unwrap();
        assert_eq!(calls.0, [format!("{record}\n").into_bytes()]);
        // The hazard: the same text through `writeln!` arrives in pieces.
        let mut pieces = Calls(Vec::new());
        writeln!(pieces, "{record}").unwrap();
        assert!(pieces.0.len() > 1, "{:?}", pieces.0);
    }

    #[test]
    fn lines_appended_at_once_stay_whole() {
        let directory = TempDir::new("hamn-record-");
        let path = directory.path().join("calls");
        const WRITERS: usize = 8;
        const LINES: usize = 200;
        std::thread::scope(|scope| {
            for writer in 0..WRITERS {
                let path = &path;
                scope.spawn(move || {
                    for line in 0..LINES {
                        // Each call opens the file anew, as each fixture process does.
                        let args = vec![format!("writer-{writer}"), format!("line-{line}"), "x".repeat(300)];
                        append_line(path, &serde_json::json!(["docker", args]).to_string());
                    }
                });
            }
        });
        let text = fs::read_to_string(&path).unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for line in text.lines() {
            let (program, args): (String, Vec<String>) =
                serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}: {line:?}"));
            assert_eq!(program, "docker");
            assert_eq!(args[2], "x".repeat(300), "{line:?}");
            assert!(seen.insert((args[0].clone(), args[1].clone())), "{line:?}");
        }
        assert_eq!(seen.len(), WRITERS * LINES);
    }
}
