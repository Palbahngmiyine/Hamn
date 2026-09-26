//! Detached CLI sessions keep running; browser/list controls retain owned
//! cleanup, and a structured query past its deadline is terminated with its
//! process group while input stays responsive.
use super::tui_native_regressions::{self as native, block_until_released, record};
use crate::runner::{self, case};
use crate::support::harness_peers::{notify, select_peer, wait_gate};
use crate::support::tui::Harness;
use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

const CONTROL_B: &[u8] = b"\x1b\x02";
const CONTROL_S: &[u8] = b"\x1b\x13";

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "tui-session-management",
        "multiple PTY sessions, foreground input, owned group cleanup and responsive query deadlines",
        vec![case("sessions", sessions), case("query_timeout", query_timeout)],
        filters,
    )
}

fn process_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    let error = io::Error::last_os_error();
    assert_eq!(error.raw_os_error(), Some(libc::ESRCH), "kill {pid}: {error}");
    false
}

/// Polls every 10 ms, as the Python suite did, for up to 5 seconds. The
/// processes are not this test's children, so there is no exit to wait on.
fn wait_dead(pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_alive(pid) {
        assert!(Instant::now() < deadline, "owned process survived: {pid}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn pid_list(value: &Value) -> Vec<i32> {
    value.as_array().unwrap().iter().map(|pid| pid.as_i64().unwrap() as i32).collect()
}

fn sessions() {
    let mut harness = Harness::new("kubernetes");
    harness.until("old-target-row");
    select_peer(&harness.root, "kubectl", "tui-session-management-forward");
    for mapping in ["8080:80", "9090:90"] {
        let command = format!(":port-forward --token session-credential-fixture pod/example {mapping}\r");
        harness.send(command.as_bytes(), &format!("FORWARD_READY:{mapping}"));
        harness.send(CONTROL_B, "old-target-row");
    }
    let pids: Vec<Vec<i32>> = fs::read_to_string(harness.root.join("session-pids"))
        .unwrap()
        .lines()
        .map(|line| pid_list(&serde_json::from_str(line).unwrap()))
        .collect();
    assert!(pids.len() == 2 && pids.iter().flatten().all(|&pid| process_alive(pid)), "{pids:?}");
    harness.send(CONTROL_S, "Sessions: Enter resumes");
    harness.until("port-forward pod/example 8080:80");
    harness.until("port-forward pod/example 9090:90");
    assert!(!harness.text().contains("session-credential-fixture"), "{}", harness.text());
    let preferences = fs::read_to_string(harness.root.join(".hamn/tui.json")).unwrap();
    assert!(!preferences.contains("session-credential-fixture"), "{preferences}");
    let calls: Vec<Vec<String>> = harness
        .calls()
        .into_iter()
        .map(|(_, args)| args)
        .filter(|args| args.iter().any(|arg| arg == "port-forward"))
        .collect();
    let token =
        |args: &Vec<String>| args.iter().position(|arg| arg == "--token").and_then(|at| args.get(at + 1)).cloned();
    assert!(
        calls.len() == 2 && calls.iter().all(|args| token(args).as_deref() == Some("session-credential-fixture")),
        "{calls:?}"
    );
    // The last detached session is selected; its PTY still accepts normal input.
    harness.send(b"\r", "FORWARD_READY:9090:90");
    harness.send(b"ordinary-input\r", "RECEIVED:ordinary-input");
    harness.send(CONTROL_S, "Sessions: Enter resumes");
    harness.write(b"d");
    for &pid in &pids[1] {
        wait_dead(pid);
    }
    assert!(pids[0].iter().all(|&pid| process_alive(pid)), "{pids:?}");
    harness.send(b"\x1b", "old-target-row");
    drop(harness);
    for &pid in &pids[0] {
        wait_dead(pid);
    }
}

fn query_timeout() {
    let mut harness = Harness::new("kubernetes");
    harness.until("old-target-row");
    harness.send(b":refresh-timeout 1\r", "old-target-row");
    select_peer(&harness.root, "kubectl", "tui-session-management-timeout");
    harness.write(b"R");
    harness.noticed("timeout-query-started");
    let pids = pid_list(&serde_json::from_str(&fs::read_to_string(harness.root.join("query-pids")).unwrap()).unwrap());
    harness.send(b":INPUT_RESPONSIVE", ":INPUT_RESPONSIVE");
    harness.write(b"\x1b");
    harness.until("Query exceeded 1 seconds");
    for pid in pids {
        wait_dead(pid);
    }
    harness.until("retry backoff");
    harness.send(b"p", "Paused");
}

fn fixture_root() -> PathBuf {
    PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"))
}

fn has(args: &[String], value: &str) -> bool {
    args.iter().any(|arg| arg == value)
}

/// The native peer whose `port-forward` (outside the config and context
/// branches) records its PID and a `sleep` helper's, then echoes input lines.
/// The helper is deliberately never waited for: the session's owner (Hamn)
/// must terminate it with the session's process group.
#[allow(clippy::zombie_processes)]
pub fn forward_peer(program: &str, args: &[String]) -> ExitCode {
    if has(args, "config") || has(args, "context") || !has(args, "port-forward") {
        return native::fixture(program, args);
    }
    let root = fixture_root();
    record(&root, program, args);
    let helper = Command::new("/bin/sleep").arg("600").spawn().expect("spawn sleep");
    let mut pids = OpenOptions::new().create(true).append(true).open(root.join("session-pids")).unwrap();
    pids.write_all(format!("{}\n", json!([std::process::id(), helper.id()])).as_bytes()).unwrap();
    println!("FORWARD_READY:{}", args.last().map_or("", String::as_str));
    let mut stdin = io::stdin().lock();
    loop {
        let mut value = String::new();
        if stdin.read_line(&mut value).expect("read stdin") == 0 {
            break;
        }
        println!("RECEIVED:{}", value.trim());
    }
    ExitCode::SUCCESS
}

/// The native peer whose `ps`/`get` query first records its PID and a
/// `sleep` helper's, reports `timeout-query-started` and waits for the gate.
/// As above, Hamn's deadline must terminate the helper with the query.
#[allow(clippy::zombie_processes)]
pub fn timeout_peer(program: &str, args: &[String]) -> ExitCode {
    let query = has(args, "ps") || has(args, "get");
    if has(args, "config") || has(args, "context") || has(args, "events") || !query {
        return native::fixture(program, args);
    }
    let root = fixture_root();
    record(&root, program, args);
    let helper = Command::new("/bin/sleep").arg("600").spawn().expect("spawn sleep");
    fs::write(root.join("query-pids"), json!([std::process::id(), helper.id()]).to_string()).unwrap();
    notify(&root, "timeout-query-started\n");
    wait_gate(&root);
    native_rows(&root, args);
    ExitCode::SUCCESS
}

/// The rest of the native `ps`/`get` branch.
fn native_rows(root: &Path, args: &[String]) {
    let changed = has(args, "new-cluster") || has(args, "external");
    if changed && !root.join("released").exists() {
        block_until_released(root);
    }
    let mut name = if changed { "new-target-row" } else { "old-target-row" };
    if has(args, "ps") {
        if has(args, "-n") || has(args, "-n5") {
            name = "last-five-row";
        }
        if has(args, "-s") {
            name = "size-row";
        }
        println!("{}", json!({"ID": "abc123", "Names": name, "State": "running"}));
    } else {
        println!("{}", json!({"items": [{"metadata": {"name": name, "namespace": "test", "uid": "uid-original"}}]}));
    }
}
