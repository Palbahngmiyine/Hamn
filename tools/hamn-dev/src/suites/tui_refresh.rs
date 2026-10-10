//! Periodic refresh through a real PTY: an automatic refresh keeps the title
//! and rows of the last result while it runs and does not refuse actions on
//! them. A Hamn profile whose Docker CLI cannot reach the daemon while its VM
//! is observed not running shows how to start the VM instead of the CLI
//! error, then shows rows without retry backoff. CLI failures that starting
//! the VM cannot fix, and an unreadable profile, keep the CLI error; the
//! header then says why the status of the profile is unavailable. When
//! Docker is installed, its own connection messages are checked as well.
use super::tui_native_regressions::{self as native, record};
use crate::runner::{self, Case, case};
use crate::support::docker_engine;
use crate::support::harness_peers::{GateOnDrop, notify, select_peer, wait_gate};
use crate::support::http::{Reply, Request, Server};
use crate::support::py_text;
use crate::support::real_cli;
use crate::support::tui::Harness;
use serde_json::json;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const GUIDANCE: &str = "Hamn VM default is not running (stopped). Press s to start it";

pub fn main(filters: &[String]) -> ExitCode {
    let mut cases: Vec<Case> = vec![
        case("automatic_refresh_keeps_the_screen", automatic_refresh_keeps_the_screen),
        case("stopped_vm_shows_start_guidance", stopped_vm_shows_start_guidance),
        case("stopped_vm_keeps_errors_that_starting_it_cannot_fix", stopped_vm_keeps_errors_that_starting_it_cannot_fix),
        case("header_says_why_the_status_is_unavailable", header_says_why_the_status_is_unavailable),
    ];
    match real_cli::which("docker") {
        Some(docker) => cases.push(case("installed_docker_connection_failures", move || installed_docker_connection_failures(&docker))),
        None => println!("SKIP installed_docker_connection_failures: installed Docker unavailable; messages are covered in Rust"),
    }
    runner::run(
        "tui-refresh",
        "automatic refresh keeps the screen and actions; only an unreachable Docker of a stopped Hamn VM shows start guidance",
        cases,
        filters,
    )
}

fn docker_queries(harness: &Harness) -> usize {
    harness.calls().iter().filter(|(program, args)| program == "docker" && args.iter().any(|arg| arg == "ps")).count()
}

/// Creates the harness's default profile, whose VM status is then stopped.
fn stopped_profile(root: &Path) {
    let profile = root.join(".hamn/default");
    fs::DirBuilder::new().mode(0o700).create(&profile).unwrap();
    let config = profile.join("config.yaml");
    fs::write(&config, "cpus: 2\nmemoryMiB: 2048\ndiskGiB: 60\n").unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
}

/// Makes the `tui-refresh` peer fail its next `ps` queries as `mode`.
fn fail_queries(root: &Path, mode: &str) {
    let stage = root.join("docker-failure.tmp");
    fs::write(&stage, mode).unwrap();
    fs::rename(&stage, root.join("docker-failure")).unwrap();
}

fn automatic_refresh_keeps_the_screen() {
    let mut harness = Harness::new("containers");
    let _release = GateOnDrop::new(&harness.gate);
    harness.until("old-target-row");
    harness.wait(|harness| !harness.text().contains("[loading]"));
    select_peer(&harness.root, "docker", "tui-refresh");
    fs::write(harness.root.join("hold-query"), "").unwrap();
    // Only the periodic tick queries now; its peer holds until the gate.
    harness.noticed("query-held");
    // Typed input is drawn after the tick that started the held refresh.
    harness.send(b":REFRESH_BARRIER", ":REFRESH_BARRIER");
    let text = harness.text();
    assert!(!text.contains("[loading]"), "{text}");
    assert!(text.contains("old-target-row"), "{text}");
    harness.write(b"\x1b");
    harness.wait(|harness| !harness.text().contains(":REFRESH_BARRIER"));
    // The displayed rows remain actionable; the action cancels the refresh.
    harness.send(b"\r", "ACTION_DONE");
    harness.until("Exit code 0");
    assert!(!harness.text().contains("refresh before acting"), "{}", harness.text());
    let inspected = harness
        .calls()
        .into_iter()
        .any(|(program, args)| program == "docker" && args.ends_with(&["container".into(), "inspect".into(), "abc123".into()]));
    assert!(inspected, "{:?}", harness.calls());
}

fn stopped_vm_shows_start_guidance() {
    let mut harness = Harness::new("containers");
    harness.until("old-target-row");
    select_peer(&harness.root, "docker", "tui-refresh");
    fail_queries(&harness.root, "unreachable");
    // Without a readable default profile no VM state explains the failure,
    // so the CLI error stays visible.
    harness.until("dockerUnreachable: docker exited");
    assert!(!harness.text().contains("Press s to start it"), "{}", harness.text());
    // An existing, stopped profile reports its state to the next refresh.
    stopped_profile(&harness.root);
    harness.until(GUIDANCE);
    let first = docker_queries(&harness);
    // Several failed queries in a row must not be reported as a CLI error or
    // back off: the list has to appear soon after the VM starts.
    harness.wait(|harness| docker_queries(harness) >= first + 3);
    let text = harness.text();
    for hidden in
        ["cliError", "dockerUnreachable", "Cannot connect", "old-target-row", "retry backoff", "previous data may be stale"]
    {
        assert!(!text.contains(hidden), "{hidden}: {text}");
    }
    assert!(text.contains("Waiting for the Hamn VM"), "{text}");
    // s offers the VM start; this test never confirms it.
    harness.send(b"s", "Confirm vm start");
    harness.write(b"n");
    harness.wait(|harness| !harness.text().contains("Confirm vm start"));
    // Docker answering again (a VM started elsewhere) needs no key press.
    fs::remove_file(harness.root.join("docker-failure")).unwrap();
    harness.until("old-target-row");
    let text = harness.text();
    assert!(!text.contains("Hamn VM default is not running"), "{text}");
    assert!(!text.contains("Waiting for the Hamn VM"), "{text}");
}

fn stopped_vm_keeps_errors_that_starting_it_cannot_fix() {
    let mut harness = Harness::new("containers");
    harness.until("old-target-row");
    // Only R queries from here, so each result belongs to the mode set before it.
    harness.send(b"p", "Paused");
    select_peer(&harness.root, "docker", "tui-refresh");
    stopped_profile(&harness.root);
    for (mode, shown) in [
        ("usage", "cliError: docker exited exit status: 125: unknown flag: --bogus-flag"),
        ("unknown-command", "cliError: docker exited exit status: 1: docker: unknown command: docker compose"),
        ("overflow", "cliError: CLI output exceeds 16 MiB"),
    ] {
        fail_queries(&harness.root, mode);
        harness.write(b"R");
        harness.until(shown);
        let text = harness.text();
        assert!(!text.contains("Press s to start it") && !text.contains("Waiting for the Hamn VM"), "{mode}: {text}");
    }
    // The same stopped VM explains a daemon the CLI could not reach.
    fail_queries(&harness.root, "unreachable");
    harness.write(b"R");
    harness.until(GUIDANCE);
}

/// The header of a Hamn profile whose status cannot be read shows that
/// failure and not a state of a VM. It tells a profile that does not exist
/// from one whose configuration cannot be read, and shows the state again
/// once the file reads. The rows of the Docker CLI are not touched.
fn header_says_why_the_status_is_unavailable() {
    let mut harness = Harness::new("containers");
    harness.until("old-target-row");
    // Only R queries from here, so each header belongs to the profile as it
    // was left before that key.
    harness.send(b"p", "Paused");
    select_peer(&harness.root, "docker", "tui-refresh");
    harness.write(b"R");
    harness.until("Status: profile default does not exist");
    let text = harness.text();
    assert!(text.contains("VM: status unavailable | Docker: status unavailable"), "{text}");
    assert!(!text.contains("not created"), "{text}");

    let profile = harness.root.join(".hamn/default");
    fs::DirBuilder::new().mode(0o700).create(&profile).unwrap();
    let config = profile.join("config.yaml");
    fs::write(&config, "kubernetes:\n  enabled: true\n").unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    harness.write(b"R");
    harness.until("Status: cannot read the configuration of profile default: unknown configuration key: kubernetes");
    let text = harness.text();
    assert!(text.contains("VM: status unavailable") && text.contains("old-target-row"), "{text}");

    fs::write(&config, "cpus: 2\nmemoryMiB: 2048\ndiskGiB: 60\n").unwrap();
    harness.write(b"R");
    harness.until("VM: stopped | Docker: Connection unavailable");
    assert!(!harness.text().contains("Status:"), "{}", harness.text());
}

/// A Unix socket that accepts each connection and closes it at once, like a
/// forward with nothing listening behind it. Dropping it stops the listener
/// and removes the socket.
struct ClosingSocket {
    path: PathBuf,
    stopped: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ClosingSocket {
    fn bind(path: &Path) -> Self {
        let listener = UnixListener::bind(path).unwrap_or_else(|error| panic!("bind {}: {error}", path.display()));
        listener.set_nonblocking(true).unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stopped);
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => drop(stream),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            }
        });
        Self { path: path.to_path_buf(), stopped, thread: Some(thread) }
    }
}

impl Drop for ClosingSocket {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

/// An Engine API that lists one container, `engine-row`.
fn engine(socket: &Path) -> Server {
    docker_engine::serve(socket, |request: &Request| -> Reply {
        let (path, _) = py_text::urlsplit(&request.target);
        let body = if path.ends_with("/_ping") {
            b"OK".to_vec()
        } else if path.ends_with("/containers/json") {
            json!([{"Id": "e".repeat(64), "Names": ["/engine-row"], "Image": "fixture",
                "ImageID": "f".repeat(64), "Command": "fixture", "Created": 0, "State": "running",
                "Status": "Up", "Ports": [], "Labels": {}}])
            .to_string()
            .into_bytes()
        } else {
            return docker_engine::not_found(request);
        };
        docker_engine::reply(request, &body, Some("application/json"))
    })
}

/// The installed Docker CLI's own messages: a missing socket and a socket
/// that closes each connection are an unreachable daemon, a reachable Engine
/// lists rows although the VM status is stopped, and a usage error keeps its
/// CLI message.
fn installed_docker_connection_failures(docker: &Path) {
    let mut harness = Harness::new("containers");
    harness.until("old-target-row");
    harness.send(b"p", "Paused");
    let root = harness.root.clone();
    stopped_profile(&root);
    let socket = root.join(".hamn/default/docker.sock");
    real_cli::wrap(&root, "docker", docker, "docker-api-1.47");
    harness.write(b"R");
    harness.until(GUIDANCE);
    assert!(!harness.text().contains("failed to connect"), "{}", harness.text());
    let server = engine(&socket);
    harness.write(b"R");
    harness.until("engine-row");
    drop(server);
    let _closing = ClosingSocket::bind(&socket);
    harness.write(b"R");
    harness.until(GUIDANCE);
    let text = harness.text();
    assert!(!text.contains("engine-row") && !text.contains("error during connect"), "{text}");
    // A usage error needs no daemon and keeps its CLI message.
    harness.send(b":ps --bogus-flag\r", "unknown flag: --bogus-flag");
    let text = harness.text();
    assert!(text.contains("cliError: docker exited exit status: 125"), "{text}");
    assert!(!text.contains("Press s to start it"), "{text}");
}

/// The native peer, except for `ps`: `docker-failure` selects how it fails
/// (as the Docker CLI does without a daemon, with a usage error, with an
/// unknown command, or with output over the TUI's limit), and the first `ps`
/// after `hold-query` appears reports `query-held` and waits for the gate.
pub fn fixture(program: &str, args: &[String]) -> ExitCode {
    let root = PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"));
    if program != "docker" || !args.iter().any(|arg| arg == "ps") {
        return native::fixture(program, args);
    }
    if let Ok(mode) = fs::read_to_string(root.join("docker-failure")) {
        record(&root, program, args);
        match mode.trim() {
            "unreachable" => {
                eprintln!(
                    "Cannot connect to the Docker daemon at unix://{}/.hamn/default/docker.sock. Is the docker daemon running?",
                    root.display()
                );
                return ExitCode::from(1);
            }
            "usage" => {
                eprintln!("unknown flag: --bogus-flag\n\nUsage:  docker ps [OPTIONS]");
                return ExitCode::from(125);
            }
            "unknown-command" => {
                eprintln!("docker: unknown command: docker compose\n\nRun 'docker --help' for more information");
                return ExitCode::from(1);
            }
            "overflow" => {
                // The TUI stops reading at its limit and ends this process.
                let _ = io::stdout().write_all(&vec![b'x'; 17 * 1024 * 1024]);
                return ExitCode::SUCCESS;
            }
            other => panic!("unknown docker-failure mode {other:?}"),
        }
    }
    // Removing the marker first lets exactly one query hold.
    if fs::remove_file(root.join("hold-query")).is_ok() {
        notify(&root, "query-held\n");
        wait_gate(&root);
    }
    native::fixture(program, args)
}
