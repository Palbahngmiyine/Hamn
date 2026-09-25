//! The real TUI deletes a Kubernetes object with server-side preconditions:
//! a loopback API server accepts the DELETE only when the selected UID and
//! resourceVersion still match, and preserves a replacement otherwise. The
//! transport is the installed kubectl when present, else a recorded fixture.
use crate::runner::{self, case};
use crate::support::exec::{self, Session, which};
use crate::support::http::{Options, Request, Response, Server};
use crate::support::pty::{self, Pty};
use crate::support::screen::RatatuiScreen;
use crate::support::tui::install_fixture;
use crate::support::{hamn, tmp::TempDir};
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn main(filters: &[String]) -> ExitCode {
    let transport = if which("kubectl").is_some() { "installed kubectl" } else { "fixture" };
    let cases = [None, Some("uid"), Some("resourceVersion")]
        .into_iter()
        .map(|changed| case(format!("scenario/{}", changed.unwrap_or("None")), move || scenario(changed)))
        .collect();
    runner::run(
        "tui-guarded-delete",
        &format!("selected UID/version guarded at DELETE server; transport={transport}"),
        cases,
        filters,
    )
}

/// The object identity the server holds, and the DELETE requests it saw.
#[derive(Default)]
struct Api {
    identity: Mutex<Value>,
    requests: Mutex<Vec<(String, Value)>>,
}

impl Api {
    fn handle(&self, request: &Request) -> Response {
        if request.method != "DELETE" {
            // BaseHTTPRequestHandler's answer to a method it does not serve.
            return Response::new(501, format!("Unsupported method ({:?})", request.method));
        }
        let body: Value = serde_json::from_slice(&request.body).expect("a JSON DELETE body");
        self.requests.lock().unwrap().push((request.target.clone(), body.clone()));
        let matches = body.get("preconditions") == Some(&*self.identity.lock().unwrap());
        let status = if matches { 200 } else { 409 };
        let data = json!({"apiVersion": "v1", "kind": "Status",
            "status": if matches { "Success" } else { "Failure" },
            "reason": if matches { "Success" } else { "Conflict" }, "code": status,
            "message": if matches { "DELETE_ACCEPTED" } else { "REPLACEMENT_PRESERVED" }});
        Response::json(status, &data)
    }
}

fn scenario(changed: Option<&'static str>) {
    let api = Arc::new(Api::default());
    *api.identity.lock().unwrap() = json!({"uid": "selected-uid", "resourceVersion": "42"});
    let directory = TempDir::new("hamn-guarded-delete-");
    let root = directory.path();
    fs::create_dir(root.join("bin")).unwrap();
    fs::create_dir(root.join(".hamn")).unwrap();
    fs::set_permissions(root.join(".hamn"), fs::Permissions::from_mode(0o700)).unwrap();
    let preferences = root.join(".hamn/tui.json");
    fs::write(&preferences, r#"{"version":1,"defaultWorkspace":"kubernetes"}"#).unwrap();
    fs::set_permissions(&preferences, fs::Permissions::from_mode(0o600)).unwrap();
    let handler = Arc::clone(&api);
    let server = Server::tcp(Options::default(), move |request| handler.handle(request).into());
    let endpoint = server.url("http");
    let config = root.join("config");
    fs::write(
        &config,
        json!({"apiVersion": "v1", "kind": "Config", "current-context": "fixture",
            "contexts": [{"name": "fixture", "context": {"cluster": "fixture", "namespace": "test"}}],
            "clusters": [{"name": "fixture", "cluster": {"server": endpoint}}]})
        .to_string(),
    )
    .unwrap();
    install_fixture(&root.join("bin"), "kubectl");
    let real_kubectl = which("kubectl").map(|path| path.into_os_string()).unwrap_or_default();
    let pty = Pty::open(32, 140);
    let mut screen = RatatuiScreen::new(32, 140);
    let mut command = Command::new(hamn());
    command
        .env("HOME", root)
        .env("KUBECONFIG", &config)
        .env("PATH", format!("{}/bin:/usr/bin:/bin", root.display()))
        .env("TERM", "xterm-256color")
        .env("REAL_KUBECTL", real_kubectl)
        .env("FIXTURE_ENDPOINT", &endpoint)
        .env("HAMN_DEV_FIXTURE", "tui-guarded-delete");
    let session = Session(pty.spawn(&mut command));
    let master = pty.master.as_raw_fd();
    // Python's finally: SIGTERM, then wait up to 5 s while draining the PTY
    // (a timeout fails the scenario); the session guard then kills the group.
    let mut terminate = Terminate { session, master };
    let mut until = |marker: &str| {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !screen.text().contains(marker) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero() && !pty::readable(&[master], left).is_empty(), "{}", screen.text());
            screen.feed(&pty::read_some(master));
        }
    };
    until("victim");
    pty::write_all(master, b"d");
    until("Confirm delete pods victim");
    if let Some(field) = changed {
        api.identity.lock().unwrap()[field] = json!("replacement");
    }
    pty::write_all(master, b"y");
    until(if changed.is_some() { "REPLACEMENT_PRESERVED" } else { "DELETE_ACCEPTED" });
    until(if changed.is_some() { "Exit code 1" } else { "Exit code 0" });
    let requests = api.requests.lock().unwrap().clone();
    let expected = vec![(
        "/api/v1/namespaces/test/pods/victim".to_owned(),
        json!({"apiVersion": "v1", "kind": "DeleteOptions",
            "preconditions": {"uid": "selected-uid", "resourceVersion": "42"}}),
    )];
    assert_eq!(requests, expected, "{requests:?}");
    terminate.finish();
}

/// Ends the TUI like the former `finally`: SIGTERM, then a five-second
/// wait that drains the PTY; the session guard kills whatever remains.
struct Terminate {
    session: Session,
    master: i32,
}

impl Terminate {
    fn finish(&mut self) {
        if let Err(message) = self.stop() {
            panic!("{message}");
        }
    }

    fn stop(&mut self) -> Result<(), String> {
        if !self.session.running() {
            return Ok(());
        }
        pty::kill(self.session.id(), libc::SIGTERM);
        match pty::wait_for_exit(&mut self.session.0, self.master, Duration::from_secs(5)) {
            Some(_) => Ok(()),
            None => Err("TUI did not exit within 5 s of SIGTERM".to_owned()),
        }
    }
}

impl Drop for Terminate {
    fn drop(&mut self) {
        if let Err(message) = self.stop() {
            eprintln!("{message}");
        }
    }
}

/// Former Python kubectl fixture: lists the victim pod, and sends a guarded
/// delete through the installed kubectl (when `REAL_KUBECTL` names one) or
/// directly to the fixture server. Uncaught failures exit 1.
pub fn fixture(_program: &str, args: &[String]) -> ExitCode {
    exec::python_exit(|| {
        let has = |value: &str| args.iter().any(|arg| arg == value);
        if has("get") {
            println!(
                "{}",
                json!({"items": [{"apiVersion": "v1", "kind": "Pod", "metadata": {
                    "name": "victim", "namespace": "test", "uid": "selected-uid", "resourceVersion": "42"}}]})
            );
            return ExitCode::SUCCESS;
        }
        assert!(has("delete") && has("--raw") && has("--filename"), "{args:?}");
        let after = |flag: &str| args[args.iter().position(|arg| arg == flag).unwrap() + 1].clone();
        let path = after("--filename");
        assert!(path.starts_with("/dev/fd/"), "{path}");
        let body = fs::File::open(&path).unwrap();
        assert_eq!(body.metadata().unwrap().nlink(), 0, "{path} has a name");
        drop(body);
        let real = std::env::var_os("REAL_KUBECTL").filter(|real| !real.is_empty());
        if let Some(real) = real {
            let error = Command::new(&real).args(args).exec();
            panic!("exec {real:?}: {error}");
        }
        let mut data = Vec::new();
        fs::File::open(&path).unwrap().read_to_end(&mut data).unwrap();
        let endpoint = std::env::var("FIXTURE_ENDPOINT").expect("FIXTURE_ENDPOINT");
        let (status, reply) = delete(&endpoint, &after("--raw"), &data);
        println!("{}", String::from_utf8(reply).expect("a UTF-8 reply"));
        if (200..300).contains(&status) { ExitCode::SUCCESS } else { ExitCode::from(1) }
    })
}

/// `DELETE endpoint+path` with a JSON body and a five-second timeout, like
/// the former urllib request; returns the status and the response body.
fn delete(endpoint: &str, path: &str, data: &[u8]) -> (u16, Vec<u8>) {
    let authority = endpoint.strip_prefix("http://").expect("an http:// endpoint");
    let timeout = Duration::from_secs(5);
    let address = authority.parse().expect("a numeric host and port");
    let mut stream = TcpStream::connect_timeout(&address, timeout).unwrap();
    stream.set_read_timeout(Some(timeout)).unwrap();
    stream.set_write_timeout(Some(timeout)).unwrap();
    let head = format!(
        "DELETE {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        data.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(data).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    let split = reply.windows(4).position(|window| window == b"\r\n\r\n").expect("a response head");
    let head = String::from_utf8_lossy(&reply[..split]).into_owned();
    let status = head.split(' ').nth(1).and_then(|code| code.parse().ok()).expect("a status code");
    let mut body = reply[split + 4..].to_vec();
    let length = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse::<usize>().expect("a Content-Length"));
    if let Some(length) = length {
        assert!(body.len() >= length, "truncated response body");
        body.truncate(length);
    }
    (status, body)
}
