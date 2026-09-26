//! A delayed connection reload must not block TUI input or survive
//! navigation: a newer invocation cancels the held reload, whose late
//! response never applies its target.
use super::tui_native_regressions::{self as native, record};
use crate::runner::{self, Case, case};
use crate::support::harness_peers::{GateOnDrop, notify, select_peer, wait_gate};
use crate::support::tui::Harness;
use serde_json::json;
use std::path::PathBuf;
use std::process::ExitCode;

pub fn main(filters: &[String]) -> ExitCode {
    let cases: Vec<Case> = ["containers", "kubernetes"]
        .into_iter()
        .map(|workspace| case(format!("exercise/{workspace}"), move || exercise(workspace)))
        .collect();
    runner::run("tui-reload", "delayed target reload remains responsive and navigation cancels it", cases, filters)
}

fn exercise(workspace: &str) {
    let mut harness = Harness::new(workspace);
    let _release = GateOnDrop::new(&harness.gate);
    harness.until("old-target-row");
    let containers = workspace == "containers";
    select_peer(&harness.root, if containers { "docker" } else { "kubectl" }, "tui-reload");
    let command = if containers { "docker context show" } else { "kubectl config use-context new-cluster" };
    harness.send(format!(":{command}\r").as_bytes(), "CONFIG_DONE");
    harness.until("Exit code 0");
    harness.write(b"\r");
    harness.noticed("reload-blocked");
    harness.send(b":version", ":version");
    harness.send(b"\r", "ACTION_DONE");
    harness.until("Exit code 0");
    harness.send(b"\r", "old-target-row");
    // The completed newer invocation is an observable cancellation barrier.
    // Releasing the old response must not apply its external context later.
    harness.release_gate();
    let query: &[u8] = if containers { b":ps\r" } else { b":get pods\r" };
    harness.send(query, "old-target-row");
    let target = if containers { "external" } else { "new-cluster" };
    assert!(!harness.text().contains(target), "{}", harness.text());
    let calls = harness.calls();
    let queried = calls.iter().any(|(_, args)| {
        let query = args.iter().any(|arg| arg == "ps" || arg == "get");
        query && args.iter().any(|arg| arg == target)
    });
    assert!(!queried, "{calls:?}");
}

/// The native peer whose target reload (`docker context ls` or `kubectl
/// config view`) reports `reload-blocked` and waits for the gate first.
pub fn fixture(program: &str, args: &[String]) -> ExitCode {
    let has = |value: &str| args.iter().any(|arg| arg == value);
    let reload = match program {
        "docker" => !has("config") && has("context") && has("ls"),
        "kubectl" => has("config") && has("view"),
        _ => false,
    };
    if !reload {
        return native::fixture(program, args);
    }
    let root = PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"));
    record(&root, program, args);
    notify(&root, "reload-blocked\n");
    wait_gate(&root);
    // The native reload responses.
    if has("config") {
        println!(
            "{}",
            json!({"current-context": "new-cluster", "contexts": [{"name": "new-cluster", "context": {"namespace": "test"}}]})
        );
    } else {
        println!("{}", json!({"Name": "external", "Current": true, "DockerEndpoint": "unix:///external/docker.sock"}));
    }
    ExitCode::SUCCESS
}
