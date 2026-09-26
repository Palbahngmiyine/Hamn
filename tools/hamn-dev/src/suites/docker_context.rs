//! The headless Engine schema over an explicit context of the real Docker
//! CLI: the context and Docker config are used only when named, mutations
//! use the immutable container ID, large and denied responses keep their
//! meaning, the deadline holds, and no profile state is touched. Requires the
//! Docker CLI, as the script did.
use super::docker_api::strip_api_version;
use crate::runner::{self, case};
use crate::support::api_fixtures::{self, Completed, MkdTemp, py_json, reason, respond, utf8};
use crate::support::hamn;
use crate::support::http::{Options, Reply, Server};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "docker-context",
        "explicit real Docker context, Engine schema, immutable mutation ID, no profile side effects, deadline",
        vec![case("explicit_docker_context_keeps_the_engine_schema", explicit_docker_context_keeps_the_engine_schema)],
        filters,
    )
}

#[derive(Default, Clone, Copy)]
struct Mode {
    deny: bool,
    wait: bool,
    large: bool,
}

/// A fake Engine: one request per connection, each on its own thread.
#[derive(Default)]
struct Engine {
    calls: Mutex<Vec<(String, String)>>,
    mode: Mutex<Mode>,
    /// Set when the test ends; a waiting list request then answers.
    released: Mutex<bool>,
    release: Condvar,
}

impl Engine {
    fn set(&self, change: impl FnOnce(&mut Mode)) {
        change(&mut self.mode.lock().unwrap());
    }

    fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().unwrap().clone()
    }

    /// Waits up to 5 seconds for the end of the test.
    fn wait_for_release(&self) {
        let released = self.released.lock().unwrap();
        let _ = self.release.wait_timeout_while(released, Duration::from_secs(5), |released| !*released).unwrap();
    }

    fn answer(&self, method: &str, target: &str) -> (u16, Vec<u8>) {
        self.calls.lock().unwrap().push((method.to_owned(), target.to_owned()));
        let path = strip_api_version(target).split('?').next().unwrap_or("");
        let (mut code, mut body) = (200, json!({}));
        if path == "/version" {
            body = json!({"ApiVersion": "1.47", "MinAPIVersion": "1.24"});
        } else if path == "/containers/json" {
            if self.mode.lock().unwrap().wait {
                self.wait_for_release();
            }
            body = json!([{"Id": "a".repeat(64), "Names": ["/external"], "State": "running"}]);
            if self.mode.lock().unwrap().large {
                let padding = "x".repeat(4096);
                let rows: Vec<Value> = (0..2048)
                    .map(|index| json!({"Id": format!("{index:064x}"), "Names": [format!("/row-{index}")], "Labels": {"padding": padding}}))
                    .collect();
                body = Value::Array(rows);
            }
        } else if path.ends_with("/json") {
            body = json!({"Id": "a".repeat(64), "Name": "/external", "State": {"Running": true}});
        } else if path.ends_with("/start") {
            code = 204;
        }
        if self.mode.lock().unwrap().deny && path != "/version" {
            (code, body) = (403, json!({"message": "fixture permission denied"}));
        }
        (code, if code == 204 { Vec::new() } else { py_json(&body).into_bytes() })
    }
}

/// Releases a waiting request however the case ends.
struct Release(Arc<Engine>);

impl Drop for Release {
    fn drop(&mut self) {
        *self.0.released.lock().unwrap() = true;
        self.0.release.notify_all();
    }
}

fn serve(socket: &Path, engine: &Arc<Engine>) -> Server {
    let engine = Arc::clone(engine);
    Server::unix(socket, Options::default(), move |request| {
        let (engine, method, target) = (Arc::clone(&engine), request.method.clone(), request.target.clone());
        Reply::Raw(Box::new(move |stream| {
            if method != "GET" && method != "POST" {
                respond(stream, "HTTP/1.1 501 Not Implemented", &[("Connection", "close".into())], b"");
                return;
            }
            let (code, data) = engine.answer(&method, &target);
            let status_line = format!("HTTP/1.1 {code} {}", reason(code));
            let headers = [
                ("Content-Type", "application/json".to_owned()),
                ("Content-Length", data.len().to_string()),
                ("Connection", "close".to_owned()),
            ];
            respond(stream, &status_line, &headers, &data);
        }))
    })
}

struct Hamn {
    binary: PathBuf,
    env: Vec<(&'static str, String)>,
}

impl Hamn {
    fn set(&mut self, name: &'static str, value: String) {
        self.env.retain(|(existing, _)| *existing != name);
        self.env.push((name, value));
    }

    fn get(&self, name: &str) -> String {
        self.env.iter().find(|(existing, _)| *existing == name).map(|(_, value)| value.clone()).unwrap()
    }

    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        command.env_remove("DOCKER_API_VERSION").envs(self.env.iter().map(|(name, value)| (name, value)));
        command
    }

    fn run(&self, arguments: &[&str]) -> (Completed, Value) {
        let mut command = self.command(&self.binary);
        command.args(["--headless", "docker"]).args(arguments);
        let completed = api_fixtures::run(&mut command, None, Duration::from_secs(12));
        let value = completed.json();
        (completed, value)
    }
}

fn explicit_docker_context_keeps_the_engine_schema() {
    let docker = api_fixtures::which("docker").expect("Docker CLI is required for external-context transport validation");
    let temp = MkdTemp::new_in(Path::new("/tmp"), "hamn-context-");
    let root = temp.path();
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    let socket = root.join("engine.sock");
    let mut hamn = Hamn {
        binary: hamn(),
        env: vec![
            ("HOME", utf8(&home).to_owned()),
            ("DOCKER_CONFIG", utf8(&root.join("docker-config")).to_owned()),
            ("DOCKER_HOST", "unix:///not-the-selected-engine.sock".to_owned()),
            ("DOCKER_CONTEXT", "not-selected".to_owned()),
        ],
    };
    let mut create = hamn.command(&docker);
    create.args(["context", "create", "fixture", "--docker", &format!("host=unix://{}", socket.display())]);
    let created = api_fixtures::run_captured(&mut create, None, Duration::from_secs(10));
    assert!(created.status.success(), "{created:?}");

    let engine = Arc::new(Engine::default());
    let server = serve(&socket, &engine);
    let release = Release(Arc::clone(&engine));
    let a64 = "a".repeat(64);

    let (result, data) = hamn.run(&["containers", "list", "--context", "fixture"]);
    assert!(result.success() && data["data"][0]["Id"] == a64.as_str(), "{result:?} {data}");
    assert!(data["target"]["context"] == "fixture" && data["target"].get("profile") == Some(&Value::Null), "{data}");
    let config = hamn.get("DOCKER_CONFIG");
    hamn.set("DOCKER_CONFIG", utf8(&root.join("wrong-config")).to_owned());
    let (result, explicit) = hamn.run(&["containers", "list", "--context", "fixture", "--docker-config", &config]);
    assert!(result.success() && explicit["target"]["dockerConfig"] == config.as_str(), "{explicit}");
    hamn.set("DOCKER_CONFIG", config);
    let (result, data) = hamn.run(&["containers", "start", "external", "--context", "fixture", "--yes"]);
    assert!(result.success(), "{result:?} {data}");
    let start = format!("/containers/{a64}/start");
    let calls = engine.calls();
    assert!(calls.iter().any(|(method, path)| method == "POST" && path.contains(&start)), "{calls:?}");
    let count = engine.calls().len();
    for arguments in [
        &["containers", "list"][..],
        &["containers", "list", "--profile", "default", "--context", "fixture"],
        &["containers", "start", "external", "--context", "fixture"],
    ] {
        let (result, data) = hamn.run(arguments);
        assert!(!result.success() && data["error"]["code"] == "invalidRequest", "{data}");
        assert_eq!(engine.calls().len(), count);
    }
    let (result, data) = hamn.run(&["containers", "list", "--context", "does-not-exist"]);
    assert!(!result.success() && engine.calls().len() == count, "{data}");
    engine.set(|mode| mode.large = true);
    let (result, data) = hamn.run(&["containers", "list", "--context", "fixture"]);
    let rows = data["data"].as_array();
    assert!(
        result.success() && rows.map(Vec::len) == Some(2048) && rows.and_then(|rows| rows.last()).unwrap()["Names"] == json!(["/row-2047"]),
        "{:?} {}",
        result.status,
        data["error"]
    );
    engine.set(|mode| mode.large = false);
    engine.set(|mode| mode.deny = true);
    let (result, data) = hamn.run(&["containers", "list", "--context", "fixture"]);
    assert!(!result.success() && data["error"]["code"] == "permissionDenied", "{data}");
    engine.set(|mode| (mode.deny, mode.wait) = (false, true));
    let (result, data) = hamn.run(&["containers", "list", "--context", "fixture", "--timeout", "1"]);
    assert!(!result.success() && data["error"]["code"] == "timeout", "{data}");
    // No VM status or profile operation is permitted on this path.
    assert!(!home.join(".hamn").exists());
    drop(release);
    drop(server);
}
