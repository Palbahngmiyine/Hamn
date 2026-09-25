//! Native CLI regressions using a real Hamn PTY and disposable, recorded CLI
//! peers: changed targets discard old rows, and Docker list flags, events and
//! installed kubectl plugins keep their CLI meaning.
use crate::runner::{self, Case, case};
use crate::support::tui::{Harness, install_fixture};
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub fn main(filters: &[String]) -> ExitCode {
    let mut cases: Vec<Case> = Vec::new();
    for (workspace, command) in
        [("kubernetes", "kubectl config use-context new-cluster"), ("containers", "docker context use external")]
    {
        cases.push(case(format!("changed_target_invalidates_previous_rows/{workspace}"), move || {
            changed_target_invalidates_previous_rows(workspace, command)
        }));
    }
    for (option, row) in [("-n 5", "last-five-row"), ("-n5", "last-five-row"), ("-s", "size-row")] {
        cases.push(case(format!("docker_list_options_do_not_become_action_connections/{option}"), move || {
            docker_list_options_do_not_become_action_connections(option, row)
        }));
    }
    for prefix in ["kubectl ", ""] {
        let label = if prefix.is_empty() { "bare" } else { "kubectl" };
        cases.push(case(format!("native_events_is_not_rewritten/{label}"), move || native_events_is_not_rewritten(prefix)));
        for alias in ["ns", "pods"] {
            cases.push(case(format!("installed_plugins_own_alias_names/{label}/{alias}"), move || {
                installed_plugins_own_alias_names(prefix, alias)
            }));
        }
    }
    runner::run(
        "tui-native-regressions",
        "changed targets discard old rows; Docker flags, events and installed plugins preserve CLI semantics",
        cases,
        filters,
    )
}

fn changed_target_invalidates_previous_rows(workspace: &str, command: &str) {
    let mut harness = Harness::new(workspace);
    harness.until("old-target-row");
    harness.send(format!(":{command}\r").as_bytes(), "CONFIG_DONE");
    harness.until("Exit code 0");
    harness.write(b"\r");
    harness.noticed("query-blocked");
    // The FIFO keeps the new target unresolved. The input marker establishes
    // that the delete key was processed without opening an old-row prompt.
    harness.write(b"d:REGRESSION_BARRIER");
    harness.wait(|harness| {
        let text = harness.text();
        text.contains("Confirm delete") || text.contains(":REGRESSION_BARRIER")
    });
    assert!(!harness.text().contains("Confirm delete"), "{}", harness.text());
    assert!(!harness.text().contains("old-target-row"), "{}", harness.text());
    assert!(!harness.calls().iter().any(|(_, args)| args.iter().any(|arg| arg == "delete" || arg == "rm")));
    harness.write(b"\x1b");
    File::create(harness.root.join("released")).unwrap();
    harness.release_gate();
    harness.until("new-target-row");
}

fn docker_list_options_do_not_become_action_connections(option: &str, row: &str) {
    let mut harness = Harness::new("containers");
    harness.until("old-target-row");
    harness.send(format!(":docker ps {option}\r").as_bytes(), row);
    harness.send(b"\r", "ACTION_DONE");
    harness.until("Exit code 0");
    let actions: Vec<Vec<String>> = harness
        .calls()
        .into_iter()
        .filter(|(program, args)| program == "docker" && args.iter().any(|arg| arg == "inspect"))
        .map(|(_, args)| args)
        .collect();
    assert_eq!(actions.len(), 1, "{actions:?}");
    let socket = format!("unix://{}/.hamn/default/docker.sock", harness.root.display());
    assert_eq!(actions[0], ["--host", &socket, "container", "inspect", "abc123"], "{actions:?}");
}

fn native_events_is_not_rewritten(prefix: &str) {
    let mut harness = Harness::new("kubernetes");
    harness.until("old-target-row");
    harness.send(format!(":{prefix}events --for pod/example --watch\r").as_bytes(), "EVENTS_DONE");
    harness.until("Exit code 0");
    let calls: Vec<Vec<String>> = harness
        .calls()
        .into_iter()
        .filter(|(program, args)| program == "kubectl" && args.iter().any(|arg| arg == "events"))
        .map(|(_, args)| args)
        .collect();
    assert_eq!(
        calls,
        [["--context", "old-cluster", "--namespace", "test", "events", "--for", "pod/example", "--watch"]],
        "{calls:?}"
    );
}

fn installed_plugins_own_alias_names(prefix: &str, alias: &str) {
    let mut harness = Harness::new("kubernetes");
    harness.until("old-target-row");
    install_fixture(&harness.root.join("bin"), &format!("kubectl-{alias}"));
    // The kubectl fixture hands `ALIAS ...` to the installed plugin.
    fs::write(harness.root.join("plugin-alias"), alias).unwrap();
    harness.send(format!(":{prefix}{alias} review-space\r").as_bytes(), "PLUGIN_RAN");
    harness.until("Exit code 0");
    let plugin_call: Value = serde_json::from_str(&fs::read_to_string(harness.root.join("plugin-call")).unwrap()).unwrap();
    assert_eq!(plugin_call, json!(["review-space"]));
    let calls: Vec<Vec<String>> = harness
        .calls()
        .into_iter()
        .filter(|(_, args)| args.iter().any(|arg| arg == "review-space"))
        .map(|(_, args)| args)
        .collect();
    assert_eq!(calls, [[alias, "review-space"]], "{calls:?}");
    assert!(harness.text().contains("Plugin-defined target"), "{}", harness.text());
}

/// The recorded `docker`/`kubectl` peer, and `kubectl-ALIAS` plugins.
pub fn fixture(program: &str, args: &[String]) -> ExitCode {
    let root = PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"));
    if program.starts_with("kubectl-") {
        fs::write(root.join("plugin-call"), json!(args).to_string()).unwrap();
        println!("PLUGIN_RAN");
        return ExitCode::SUCCESS;
    }
    record(&root, program, args);
    let has = |value: &str| args.iter().any(|arg| arg == value);
    if let Ok(alias) = fs::read_to_string(root.join("plugin-alias"))
        && args.first().map(String::as_str) == Some(alias.as_str())
    {
        let plugin = root.join("bin").join(format!("kubectl-{alias}"));
        let error = std::process::Command::new(&plugin).args(&args[1..]).exec();
        panic!("exec {}: {error}", plugin.display());
    }
    if has("config") {
        if has("view") {
            println!(
                "{}",
                json!({"current-context": "new-cluster", "contexts": [{"name": "new-cluster", "context": {"namespace": "test"}}]})
            );
        } else {
            println!("CONFIG_DONE");
        }
    } else if has("context") {
        if has("ls") {
            println!("{}", json!({"Name": "external", "Current": true, "DockerEndpoint": "unix:///external/docker.sock"}));
        } else {
            println!("CONFIG_DONE");
        }
    } else if has("events") {
        println!("EVENTS_DONE");
    } else if has("ps") || has("get") {
        let changed = has("new-cluster") || has("external");
        if changed && !root.join("released").exists() {
            block_until_released(&root);
        }
        let mut name = if changed { "new-target-row" } else { "old-target-row" };
        if has("ps") {
            if has("-n") || has("-n5") {
                name = "last-five-row";
            }
            if has("-s") {
                name = "size-row";
            }
            println!("{}", json!({"ID": "abc123", "Names": name, "State": "running"}));
        } else {
            println!("{}", json!({"items": [{"metadata": {"name": name, "namespace": "test", "uid": "uid-original"}}]}));
        }
    } else {
        println!("ACTION_DONE");
    }
    ExitCode::SUCCESS
}

/// Appends `[program, args]` to `root/calls`.
pub fn record(root: &Path, program: &str, args: &[String]) {
    let mut calls = OpenOptions::new().create(true).append(true).open(root.join("calls")).unwrap();
    writeln!(calls, "{}", json!([program, args])).unwrap();
}

/// Tells the test the query is blocked, then waits for its gate byte.
pub fn block_until_released(root: &Path) {
    let mut notice = OpenOptions::new().write(true).open(root.join("notice")).unwrap();
    notice.write_all(b"query-blocked\n").unwrap();
    drop(notice);
    let mut gate = File::open(root.join("gate")).unwrap();
    gate.read_exact(&mut [0u8]).unwrap();
}
