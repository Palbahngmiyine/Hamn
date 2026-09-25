//! The embedded Docker client against a fake Engine on the profile's Unix
//! socket, with no Docker CLI on PATH: listing, guarded mutations, framed
//! log streams split inside a UTF-8 character, and error classification.
use crate::runner::{self, case};
use crate::support::api_fixtures::{self, Completed, MkdTemp, py_json};
use crate::support::hamn;
use crate::support::http::{Options, Reply, Server};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "docker-api",
        "Docker API without Docker CLI",
        vec![case("docker_api_without_docker_cli", docker_api_without_docker_cli)],
        filters,
    )
}

/// The fake Engine's record and the statuses the test switches.
struct Engine {
    requests: Mutex<Vec<(String, String)>>,
    mode: Mutex<Mode>,
    /// The script's server handled one connection at a time.
    serial: Mutex<()>,
}

#[derive(Clone, Copy)]
struct Mode {
    status: u16,
    post: u16,
}

impl Engine {
    fn set(&self, change: impl FnOnce(&mut Mode)) {
        change(&mut self.mode.lock().unwrap());
    }

    fn requests(&self) -> Vec<(String, String)> {
        self.requests.lock().unwrap().clone()
    }

    /// One request per connection: the status line reason is `Fixture`, and
    /// the connection closes after the response.
    fn answer(&self, method: &str, target: &str) -> Vec<u8> {
        self.requests.lock().unwrap().push((method.to_owned(), target.to_owned()));
        let mode = *self.mode.lock().unwrap();
        let path = strip_api_version(target).split('?').next().unwrap_or("");
        let mut status = mode.status;
        let mut data;
        if path == "/version" {
            (data, status) = (json!({"ApiVersion": "1.53", "MinAPIVersion": "1.40"}), 200);
        } else if path == "/containers/json" {
            data = json!([{"Id": "abc123", "Names": ["/sample"], "State": "running"}]);
        } else if path.ends_with("/json") {
            data = json!({"Id": "abc123", "Name": "/sample", "State": {"Running": true}});
        } else {
            (data, status) = (json!({}), if method == "POST" { mode.post } else { 204 });
        }
        if status != 200 && status != 204 {
            data = json!({"message": "fixture denied"});
        }
        let mut body = if status == 204 { Vec::new() } else { py_json(&data).into_bytes() };
        if path.ends_with("/logs") {
            status = 200;
            body = log_frames();
        }
        let mut response = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(&body);
        response
    }
}

/// `re.sub(r"^/v[0-9.]+", "", target)`: drops a leading API version.
pub fn strip_api_version(target: &str) -> &str {
    let Some(rest) = target.strip_prefix("/v") else { return target };
    let version = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.').len();
    if version == 0 { target } else { &rest[version..] }
}

/// Fifty `한글 line N` lines as two stdout frames of the multiplexed log
/// stream; the first frame ends inside the first character.
fn log_frames() -> Vec<u8> {
    let logs: String = (0..50).map(|index| format!("한글 line {index}\n")).collect();
    let logs = logs.as_bytes();
    let mut body = Vec::new();
    for part in [&logs[..2], &logs[2..]] {
        body.extend_from_slice(b"\x01\0\0\0");
        body.extend_from_slice(&u32::try_from(part.len()).unwrap().to_be_bytes());
        body.extend_from_slice(part);
    }
    body
}

struct Hamn {
    binary: PathBuf,
    home: PathBuf,
}

impl Hamn {
    fn headless(&self, arguments: &[&str]) -> Completed {
        let mut command = Command::new(&self.binary);
        command.arg("--headless").args(arguments).env("HOME", &self.home).env("PATH", "/usr/bin:/bin");
        api_fixtures::run(&mut command, None, Duration::from_secs(15))
    }

    /// The exit status and the JSON envelope.
    fn run(&self, arguments: &[&str]) -> (bool, Value) {
        let completed = self.headless(arguments);
        (completed.success(), completed.json())
    }
}

fn docker_api_without_docker_cli() {
    let directory = MkdTemp::new("hamn-docker-");
    let hamn = Hamn { binary: hamn(), home: directory.path().to_path_buf() };
    assert!(hamn.run(&["vm", "create", "--profile", "test", "--yes"]).0);
    let socket = directory.path().join(".hamn/test/docker.sock");
    let engine = Arc::new(Engine {
        requests: Mutex::new(Vec::new()),
        mode: Mutex::new(Mode { status: 200, post: 204 }),
        serial: Mutex::new(()),
    });
    let server = serve(&socket, &engine);

    let (ok, result) = hamn.run(&["docker", "containers", "list", "--profile", "test"]);
    assert!(ok && result["data"][0]["Id"] == "abc123", "{result}");
    let (ok, result) = hamn.run(&["docker", "containers", "start", "sample", "--profile", "test", "--yes"]);
    assert!(ok, "{result}");
    let requests = engine.requests();
    assert!(requests.iter().any(|(method, path)| method == "POST" && path.contains("/containers/abc123/start")), "{requests:?}");
    let before = engine.requests().len();
    assert!(!hamn.run(&["docker", "containers", "delete", "sample", "--profile", "test"]).0);
    assert_eq!(engine.requests().len(), before);
    for flags in [&[][..], &["--follow"][..]] {
        let mut arguments = vec!["docker", "containers", "logs", "sample", "--profile", "test"];
        arguments.extend_from_slice(flags);
        let streamed = hamn.headless(&arguments);
        assert!(streamed.success(), "{streamed:?}");
        let events: Vec<Value> = streamed.stdout.lines().map(api_fixtures::parse).collect();
        assert_eq!(events.len(), 51, "{events:?}");
        assert_eq!(events[0]["data"]["text"], "한글 line 0\n");
        let last = events.last().unwrap();
        assert!(last["type"] == "result" && last["sequence"] == 50, "{last}");
    }
    engine.set(|mode| mode.post = 503);
    let (ok, result) = hamn.run(&["docker", "containers", "start", "sample", "--profile", "test", "--yes"]);
    assert!(!ok && result["error"]["code"] == "outcomeUnknown", "{result}");
    engine.set(|mode| mode.post = 403);
    let (ok, result) = hamn.run(&["docker", "containers", "start", "sample", "--profile", "test", "--yes"]);
    assert!(!ok && result["error"]["code"] == "permissionDenied", "{result}");
    engine.set(|mode| mode.post = 204);
    engine.set(|mode| mode.status = 403);
    let (ok, result) = hamn.run(&["docker", "containers", "list", "--profile", "test"]);
    assert!(!ok && result["error"]["code"] == "permissionDenied", "{result}");
    drop(server);
}

fn serve(socket: &Path, engine: &Arc<Engine>) -> Server {
    let engine = Arc::clone(engine);
    Server::unix(socket, Options::default(), move |request| {
        let (engine, method, target) = (Arc::clone(&engine), request.method.clone(), request.target.clone());
        Reply::Raw(Box::new(move |stream| {
            // Held until the response is written, as the script's
            // single-threaded server finished one connection at a time.
            let _serial = engine.serial.lock().unwrap();
            let response = engine.answer(&method, &target);
            let _ = stream.write_all(&response).and_then(|()| stream.flush());
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::strip_api_version;

    #[test]
    fn strip_api_version_matches_the_script_pattern() {
        assert_eq!(strip_api_version("/v1.47/containers/json?all=1"), "/containers/json?all=1");
        assert_eq!(strip_api_version("/version"), "/version");
        assert_eq!(strip_api_version("/v/containers"), "/v/containers");
        assert_eq!(strip_api_version("/_ping"), "/_ping");
    }
}
