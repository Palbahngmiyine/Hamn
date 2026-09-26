//! Installed CLIs (docker, kubectl) that suites compare with Hamn: PATH
//! lookup like `shutil.which`, bounded runs like `subprocess.run(...,
//! timeout=...)`, and wrapper fixtures that exec the real CLI from a test's
//! private `bin/`, the way the Python suites' `os.execv` wrappers did.
use super::pty;
use super::py_text;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
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

/// Reads a started child's piped standard output and error to their ends and
/// waits for it, failing after `timeout` (`Popen.communicate(timeout=...)`):
/// both the exit and the end of both streams (which a descendant can hold
/// open) must arrive in time. A child still running then is killed and
/// reaped first.
pub fn communicate(mut child: Child, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;
    let (sender, receiver) = mpsc::channel::<(usize, Vec<u8>)>();
    let streams: [Option<Box<dyn Read + Send>>; 2] = [
        child.stdout.take().map(|stream| Box::new(stream) as Box<dyn Read + Send>),
        child.stderr.take().map(|stream| Box::new(stream) as Box<dyn Read + Send>),
    ];
    for (index, stream) in streams.into_iter().enumerate() {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut data = Vec::new();
            if let Some(mut stream) = stream {
                stream.read_to_end(&mut data).expect("read child output");
            }
            let _ = sender.send((index, data));
        });
    }
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if Instant::now() >= deadline {
            // The child is unreaped, so its pid still names it.
            let _ = child.kill();
            let _ = child.wait();
            panic!("process {} timed out after {timeout:?}", child.id());
        }
        // A bounded poll of an owned child; try_wait has no timeout form.
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut outputs = [Vec::new(), Vec::new()];
    for _ in 0..2 {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (index, data) = receiver
            .recv_timeout(remaining)
            .unwrap_or_else(|_| panic!("output of process {} still open after {timeout:?}", child.id()));
        outputs[index] = data;
    }
    let [stdout, stderr] = outputs;
    Output { returncode: returncode(status), stdout, stderr }
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

/// Appends one line to `path`, creating it.
pub fn append_line(path: &Path, line: &str) {
    let mut file = OpenOptions::new().create(true).append(true).open(path).unwrap();
    writeln!(file, "{line}").unwrap();
}

/// The lines of `path` parsed as JSON string lists (a recorded argv log).
pub fn recorded_argv(path: &Path) -> Vec<Vec<String>> {
    let text = fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    py_text::splitlines(&text).into_iter().map(|line| serde_json::from_str(line).unwrap()).collect()
}
