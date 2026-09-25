//! Observes the actual HTTP requests of the C snapshot reader
//! (`TEST_BIN read-snapshot PROFILE`) against a bounded Docker Engine
//! fixture of 1, 10 and 100 published containers, and prints each
//! measurement as a JSON line.
//!
//! `hamn-dev test observer-requests TEST_BIN WORK_DIRECTORY [FILTER...]`
use crate::runner::{self, Case, case};
use crate::support::bounded_process;
use crate::support::http::{Options, Reply, Request, Response, Server};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn main(args: &[String]) -> ExitCode {
    let [binary, root, filters @ ..] = args else {
        eprintln!("usage: hamn-dev test observer-requests TEST_BINARY WORK_DIRECTORY [FILTER...]");
        return ExitCode::from(2);
    };
    let cases = cases(Path::new(binary), Path::new(root));
    runner::run("observer-requests", "the snapshot reader sends one bounded request per snapshot", cases, filters)
}

/// The measurements, each in its own profile directory below `root`; the
/// port-forwarding suite runs them against its compiled driver.
pub fn cases(binary: &Path, root: &Path) -> Vec<Case> {
    [1usize, 10, 100]
        .into_iter()
        .map(|count| {
            let (binary, root) = (binary.to_path_buf(), root.to_path_buf());
            case(format!("measure/{count}"), move || measure(&binary, &root, count))
        })
        .collect()
}

fn measure(binary: &Path, root: &Path, count: usize) {
    let profile = root.join(format!("observer-{count}"));
    fs::create_dir(&profile).unwrap_or_else(|error| panic!("{}: {error}", profile.display()));
    let socket = profile.join("docker.sock");
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let payload = containers(count);
    let payload_bytes = payload.len();
    let recorded = Arc::clone(&calls);
    let server = Server::unix(&socket, Options::default(), move |request: &Request| -> Reply {
        // Like the Python fixture, which handled only GET, other methods get
        // 501 and are not recorded.
        if request.method != "GET" {
            return Response::new(501, "").into();
        }
        recorded.lock().unwrap().push(request.target.clone());
        std::thread::sleep(Duration::from_millis(10)); // Deliberate workload latency, not synchronization.
        let listed = request.target == "/containers/json";
        let body = if listed { payload.clone() } else { b"{}".to_vec() };
        Response::new(if listed { 200 } else { 404 }, body).into()
    });
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let started = Instant::now();
    let result = bounded_process::output(Command::new(binary).arg("read-snapshot").arg(&profile), Duration::from_secs(8));
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(result.status.code(), Some(0), "{result:?}");
    assert_eq!(String::from_utf8_lossy(&result.stdout).trim(), count.to_string(), "{result:?}");
    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls, ["/containers/json"], "{calls:?}");
    println!(
        "{{\"containers\": {count}, \"requests\": {}, \"elapsedMs\": {}, \"payloadBytes\": {payload_bytes}}}",
        calls.len(),
        (elapsed * 100.0).round() / 100.0
    );
    // The reader must leave the socket in place and nothing else behind.
    assert!(socket.exists(), "{} disappeared", socket.display());
    drop(server);
    fs::remove_dir(&profile).unwrap_or_else(|error| panic!("{}: {error}", profile.display()));
}

/// `count` running containers, each publishing 127.0.0.1:40000+i -> 80/tcp,
/// serialized exactly as Python's `json.dumps` wrote them.
fn containers(count: usize) -> Vec<u8> {
    let rows: Vec<String> = (0..count)
        .map(|index| {
            format!(
                "{{\"Id\": \"{index:064x}\", \"Ports\": [{{\"IP\": \"127.0.0.1\", \"PrivatePort\": 80, \"PublicPort\": {}, \"Type\": \"tcp\"}}]}}",
                40000 + index
            )
        })
        .collect();
    format!("[{}]", rows.join(", ")).into_bytes()
}
