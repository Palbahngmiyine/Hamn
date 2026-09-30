//! Periodic refresh through a real PTY: an automatic refresh keeps the title
//! and rows of the last result while it runs and does not refuse actions on
//! them, and a Hamn profile whose VM is observed not running shows how to
//! start it instead of the Docker CLI error, then shows rows without retry
//! backoff. An unreadable profile keeps the CLI error.
use super::tui_native_regressions::{self as native, record};
use crate::runner::{self, Case, case};
use crate::support::harness_peers::{GateOnDrop, notify, select_peer, wait_gate};
use crate::support::tui::Harness;
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;
use std::process::ExitCode;

pub fn main(filters: &[String]) -> ExitCode {
    let cases: Vec<Case> = vec![
        case("automatic_refresh_keeps_the_screen", automatic_refresh_keeps_the_screen),
        case("stopped_vm_shows_start_guidance", stopped_vm_shows_start_guidance),
    ];
    runner::run(
        "tui-refresh",
        "automatic refresh keeps the screen and actions; a stopped Hamn VM shows start guidance and recovers without backoff",
        cases,
        filters,
    )
}

fn docker_queries(harness: &Harness) -> usize {
    harness.calls().iter().filter(|(program, args)| program == "docker" && args.iter().any(|arg| arg == "ps")).count()
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
    fs::write(harness.root.join("vm-stopped"), "").unwrap();
    // Without a readable default profile no VM state explains the failure,
    // so the CLI error stays visible.
    harness.until("cliError: docker exited");
    assert!(!harness.text().contains("Press s to start it"), "{}", harness.text());
    // An existing, stopped profile reports its state to the next refresh.
    let profile = harness.root.join(".hamn/default");
    fs::DirBuilder::new().mode(0o700).create(&profile).unwrap();
    let config = profile.join("config.yaml");
    fs::write(&config, "cpus: 2\nmemoryMiB: 2048\ndiskGiB: 60\n").unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    harness.until("Hamn VM default is not running (stopped). Press s to start it");
    let first = docker_queries(&harness);
    // Several failed queries in a row must not be reported as a CLI error or
    // back off: the list has to appear soon after the VM starts.
    harness.wait(|harness| docker_queries(harness) >= first + 3);
    let text = harness.text();
    for hidden in ["cliError", "Cannot connect", "old-target-row", "retry backoff", "previous data may be stale"] {
        assert!(!text.contains(hidden), "{hidden}: {text}");
    }
    assert!(text.contains("Waiting for the Hamn VM"), "{text}");
    // s offers the VM start; this test never confirms it.
    harness.send(b"s", "Confirm vm start");
    harness.write(b"n");
    harness.wait(|harness| !harness.text().contains("Confirm vm start"));
    // Docker answering again (a VM started elsewhere) needs no key press.
    fs::remove_file(harness.root.join("vm-stopped")).unwrap();
    harness.until("old-target-row");
    let text = harness.text();
    assert!(!text.contains("Hamn VM default is not running"), "{text}");
    assert!(!text.contains("Waiting for the Hamn VM"), "{text}");
}

/// The native peer, except for `ps`: while `vm-stopped` exists it fails as
/// the Docker CLI does without a daemon, and the first `ps` after
/// `hold-query` appears reports `query-held` and waits for the gate.
pub fn fixture(program: &str, args: &[String]) -> ExitCode {
    let root = PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"));
    if program != "docker" || !args.iter().any(|arg| arg == "ps") {
        return native::fixture(program, args);
    }
    if root.join("vm-stopped").exists() {
        record(&root, program, args);
        eprintln!(
            "Cannot connect to the Docker daemon at unix://{}/.hamn/default/docker.sock. Is the docker daemon running?",
            root.display()
        );
        return ExitCode::from(1);
    }
    // Removing the marker first lets exactly one query hold.
    if fs::remove_file(root.join("hold-query")).is_ok() {
        notify(&root, "query-held\n");
        wait_gate(&root);
    }
    native::fixture(program, args)
}
