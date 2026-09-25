//! Shared pieces of the headless, worker and API fixture suites: child
//! processes run with the semantics of Python's `subprocess` (which those
//! suites replace), HTTP responses written byte for byte, and the text of
//! Python's `json.dumps`.
use super::http::Stream;
use serde::Serialize;
use serde_json::Value;
use serde_json::ser::Formatter;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// A finished child, as Python's `CompletedProcess` with `text=True`: both
/// streams are strict UTF-8 with universal newlines.
#[derive(Debug)]
pub struct Completed {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

impl Completed {
    pub fn success(&self) -> bool {
        self.status.success()
    }

    /// The whole standard output as one JSON document (`json.loads`).
    pub fn json(&self) -> Value {
        parse(&self.stdout)
    }
}

/// Parses one JSON document, failing the case with the text on error.
pub fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("invalid JSON ({error}): {text:?}"))
}

/// A finished child's undecoded output (`capture_output=True` without
/// `text=True`).
#[derive(Debug)]
pub struct Captured {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Runs `command` like `subprocess.run(..., capture_output=True, text=True,
/// timeout=timeout)`; see [`run_captured`].
pub fn run(command: &mut Command, input: Option<&[u8]>, timeout: Duration) -> Completed {
    let captured = run_captured(command, input, timeout);
    Completed { status: captured.status, stdout: text(captured.stdout), stderr: text(captured.stderr) }
}

/// Runs `command` like `subprocess.run(..., capture_output=True,
/// timeout=timeout)`: standard input is `input` when given and is otherwise
/// inherited, and both output pipes are read to end-of-file and the child is
/// reaped before the deadline. A child still running then is killed and
/// reaped, and the case fails.
pub fn run_captured(command: &mut Command, input: Option<&[u8]>, timeout: Duration) -> Captured {
    let stdin = if input.is_some() { Stdio::piped() } else { Stdio::inherit() };
    command.stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().unwrap_or_else(|error| panic!("spawn {command:?}: {error}"));
    if let Some(input) = input {
        let mut pipe = child.stdin.take().expect("piped stdin");
        let input = input.to_vec();
        // A child that exits without reading its input closes the pipe; like
        // `communicate`, ignore that. Dropping the pipe sends end-of-file.
        std::thread::spawn(move || {
            let _ = pipe.write_all(&input);
        });
    }
    match communicate(&mut child, timeout) {
        Some(captured) => captured,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("timed out after {timeout:?}: {command:?}");
        }
    }
}

/// Like `Popen.communicate(timeout=timeout)`: reads the child's piped
/// stdout and stderr to end-of-file and waits for it to exit. `None` means
/// the deadline passed first; the child is left running for the caller.
pub fn communicate(child: &mut Child, timeout: Duration) -> Option<Captured> {
    let deadline = Instant::now() + timeout;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let stdout = stdout.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok()?;
    let stderr = stderr.recv_timeout(deadline.saturating_duration_since(Instant::now())).ok()?;
    let status = wait_until(child, deadline)?;
    Some(Captured { status, stdout, stderr })
}

/// Waits for `child` to exit until `deadline`, polling like Python's
/// `Popen.wait(timeout=...)`. The child is reaped only once it has exited,
/// so its PID stays valid for a caller that kills it after `None`.
pub fn wait_until(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        if let Some(status) = child.try_wait().expect("wait for child") {
            return Some(status);
        }
        let remaining = deadline.checked_duration_since(Instant::now())?;
        std::thread::sleep(remaining.min(Duration::from_millis(5)));
    }
}

/// Reads `pipe` to end-of-file on its own thread.
fn drain(pipe: Option<impl Read + Send + 'static>) -> Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut data = Vec::new();
        if let Some(mut pipe) = pipe {
            pipe.read_to_end(&mut data).expect("read child output");
        }
        let _ = sender.send(data);
    });
    receiver
}

/// Decodes child output as `text=True` does: strict UTF-8, and `\r\n` and
/// `\r` read as `\n`.
pub fn text(data: Vec<u8>) -> String {
    let text = String::from_utf8(data).unwrap_or_else(|error| panic!("child output is not UTF-8: {error}"));
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// A private directory named as Python's `tempfile.mkdtemp` names one:
/// `prefix` and 8 random characters. Paths below it (Unix sockets included)
/// are as long as they were in the Python suites, which matters: a socket
/// path past `sun_path` changes Hamn's behavior. Removed when dropped.
pub struct MkdTemp(PathBuf);

impl MkdTemp {
    /// In `$TMPDIR` (else /tmp), like `TemporaryDirectory(prefix=...)`.
    pub fn new(prefix: &str) -> Self {
        Self::new_in(&std::env::temp_dir(), prefix)
    }

    /// Like `TemporaryDirectory(prefix=..., dir=parent)`.
    pub fn new_in(parent: &Path, prefix: &str) -> Self {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
        let mut random = std::fs::File::open("/dev/urandom").expect("open /dev/urandom");
        for _ in 0..100 {
            let mut bytes = [0u8; 8];
            random.read_exact(&mut bytes).expect("read /dev/urandom");
            let suffix: String = bytes.iter().map(|byte| ALPHABET[usize::from(*byte) % ALPHABET.len()] as char).collect();
            let path = parent.join(format!("{prefix}{suffix}"));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("{}: {error}", path.display()),
            }
        }
        panic!("cannot create a unique directory in {}", parent.display());
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for MkdTemp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The first executable `name` on `$PATH`, as `shutil.which`.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|directory| directory.join(name)).find(|candidate| {
        std::fs::metadata(candidate).is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    })
}

/// Python's truth value of an optional JSON member (`bool(value.get(key))`).
pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_f64() != Some(0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(members)) => !members.is_empty(),
    }
}

/// `path` as a UTF-8 string, for JSON documents and arguments.
pub fn utf8(path: &Path) -> &str {
    path.to_str().unwrap_or_else(|| panic!("{} is not UTF-8", path.display()))
}

/// Writes one HTTP response byte for byte: `status_line` (for example
/// `HTTP/1.0 200 OK`), `headers` in order, a blank line and `body`. A write
/// error means the client already gave up (a timed-out request), which the
/// Python fixtures tolerated too.
pub fn respond(stream: &mut dyn Stream, status_line: &str, headers: &[(&str, String)], body: &[u8]) {
    let mut response = format!("{status_line}\r\n");
    for (name, value) in headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("\r\n");
    let mut response = response.into_bytes();
    response.extend_from_slice(body);
    let _ = stream.write_all(&response).and_then(|()| stream.flush());
}

/// The reason phrase Python's `http.server` sends with `status`.
pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        403 => "Forbidden",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        _ => panic!("no fixture sends status {status}"),
    }
}

/// The text of Python's `json.dumps(value)`: `", "` and `": "` separators and
/// every character outside printable ASCII escaped. Object members come in
/// serde_json's key order, which JSON readers do not observe.
pub fn py_json(value: &impl Serialize) -> String {
    let mut output = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut output, PythonFormatter);
    value.serialize(&mut serializer).expect("serialize JSON");
    String::from_utf8(output).expect("escaped JSON is ASCII")
}

struct PythonFormatter;

impl Formatter for PythonFormatter {
    fn begin_array_value<W: ?Sized + Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        if first { Ok(()) } else { writer.write_all(b", ") }
    }

    fn begin_object_key<W: ?Sized + Write>(&mut self, writer: &mut W, first: bool) -> io::Result<()> {
        if first { Ok(()) } else { writer.write_all(b", ") }
    }

    fn begin_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        writer.write_all(b": ")
    }

    /// serde_json escapes quotes, backslashes and control characters itself;
    /// `ensure_ascii` also escapes DEL and non-ASCII characters (as UTF-16).
    fn write_string_fragment<W: ?Sized + Write>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()> {
        for character in fragment.chars() {
            if (' '..='~').contains(&character) {
                writer.write_all(&[character as u8])?;
            } else {
                for unit in character.encode_utf16(&mut [0; 2]) {
                    write!(writer, "\\u{unit:04x}")?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn py_json_matches_python_json_dumps() {
        // Expected texts are Python 3's json.dumps output for the same values.
        assert_eq!(py_json(&json!({"a": [1, true, null], "b": {"c": "d"}})), r#"{"a": [1, true, null], "b": {"c": "d"}}"#);
        // U+D55C U+AE00, then escapes, DEL and U+1F600 as a surrogate pair.
        let escape = |hex: &str| format!("{}u{hex}", '\\');
        let expected = format!(
            "\"{}{}\\n\\\"\\\\{}{}{}/\"",
            escape("d55c"),
            escape("ae00"),
            escape("007f"),
            escape("d83d"),
            escape("de00")
        );
        assert_eq!(py_json(&json!("\u{d55c}\u{ae00}\n\"\\\u{7f}\u{1f600}/")), expected);
        assert_eq!(py_json(&json!([])), "[]");
        assert_eq!(py_json(&json!({})), "{}");
    }

    #[test]
    fn mkdtemp_names_match_python() {
        let directory = MkdTemp::new("hamn-dev-mkdtemp-");
        let name = directory.path().file_name().unwrap().to_str().unwrap().to_owned();
        let suffix = name.strip_prefix("hamn-dev-mkdtemp-").unwrap();
        assert_eq!(suffix.len(), 8);
        assert!(suffix.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'));
        assert_eq!(std::fs::metadata(directory.path()).unwrap().permissions().mode() & 0o777, 0o700);
        let path = directory.path().to_path_buf();
        drop(directory);
        assert!(!path.exists());
    }

    #[test]
    fn run_reports_output_and_enforces_its_deadline() {
        let completed = run(Command::new("/bin/cat").arg("-"), Some(b"line\r\nnext"), Duration::from_secs(10));
        assert!(completed.success());
        assert_eq!(completed.stdout, "line\nnext");
        let started = Instant::now();
        let outcome = std::panic::catch_unwind(|| run(Command::new("/bin/sleep").arg("30"), None, Duration::from_millis(200)));
        assert!(outcome.is_err());
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
