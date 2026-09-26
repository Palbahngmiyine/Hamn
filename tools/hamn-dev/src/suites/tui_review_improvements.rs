//! Identity refresh, pasted filters, diagnostics and bounded log menus via a
//! real PTY: a selection follows its resource identity across reordered and
//! shrinking refreshes, pasted filters and log options reach the CLI, and
//! Compose projects navigate to their containers.
use super::tui_native_regressions::record;
use crate::runner::{self, Case, case};
use crate::support::harness_peers::select_peer;
use crate::support::tui::Harness;
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

pub fn main(filters: &[String]) -> ExitCode {
    let mut cases: Vec<Case> = Vec::new();
    for workspace in ["containers", "kubernetes"] {
        cases.push(case(format!("scenario/{workspace}"), move || scenario(workspace)));
    }
    cases.push(case("compose_navigation", compose_navigation));
    runner::run(
        "tui-review-improvements",
        "selection identity, removal, pasted filters, diagnostics, log options, relationships, Compose and favorites",
        cases,
        filters,
    )
}

fn pod(name: &str, uid: &str, namespace: &str) -> Value {
    json!({"apiVersion": "v1", "kind": "Pod", "metadata": {"name": name, "uid": uid,
        "namespace": namespace, "resourceVersion": "1"}, "spec": {"containers": [{"name": "app"}]},
        "status": {"phase": "Running", "containerStatuses": [{"name": "app", "ready": false,
            "restartCount": 42, "state": {"waiting": {"reason": "CrashLoopBackOff"}}}]}})
}

/// Replaces the peer's `rows.json` atomically.
fn write_rows(harness: &Harness, rows: &[&Value]) {
    let stage = harness.root.join("rows.tmp");
    fs::write(&stage, json!(rows).to_string()).unwrap();
    fs::rename(&stage, harness.root.join("rows.json")).unwrap();
}

fn logs_calls(harness: &Harness) -> Vec<Vec<String>> {
    harness.calls().into_iter().map(|(_, args)| args).filter(|args| args.iter().any(|arg| arg == "logs")).collect()
}

fn index(args: &[String], value: &str) -> usize {
    args.iter().position(|arg| arg == value).unwrap_or_else(|| panic!("{value} not in {args:?}"))
}

fn has(args: &[String], value: &str) -> bool {
    args.iter().any(|arg| arg == value)
}

fn no_deletes(harness: &Harness) -> bool {
    !harness.calls().iter().any(|(_, args)| has(args, "delete") || has(args, "rm"))
}

fn scenario(workspace: &str) {
    let mut harness = Harness::new(workspace);
    harness.until("old-target-row");
    let kubernetes = workspace == "kubernetes";
    let row = |name: &str, uid: &str, namespace: &str| {
        if kubernetes { pod(name, uid, namespace) } else { json!({"ID": uid, "Names": name, "State": "running"}) }
    };
    let (alpha, beta, gamma) =
        (row("alpha", "id-a", "work"), row("beta", "id-b", "west"), row("gamma", "id-c", "work"));
    write_rows(&harness, &[&alpha, &beta, &gamma]);
    select_peer(&harness.root, if kubernetes { "kubectl" } else { "docker" }, "tui-review-improvements");
    let query: &[u8] = if kubernetes { b":get pods -A\r" } else { b":ps\r" };
    harness.send(query, "beta");
    if kubernetes {
        harness.until("CrashLoopBackOff");
        harness.until("42");
    }
    harness.write(b"j");
    // A changed marker proves the reordered response has actually rendered.
    write_rows(&harness, &[&row("marker-reordered", "id-d", "work"), &gamma, &alpha, &beta]);
    harness.send(b"R", "marker-reordered");
    let expected = if kubernetes { "Confirm delete pods beta" } else { "Confirm delete containers id-b" };
    harness.send(b"d", expected);
    if kubernetes {
        assert!(harness.text().contains("selected namespace: west"), "{}", harness.text());
    }
    harness.send(b"n", "marker-reordered");
    write_rows(&harness, &[&row("marker-removed", "id-d", "work"), &alpha, &gamma]);
    harness.send(b"R", "marker-removed");
    harness.write(b"d:SELECTION_BARRIER");
    harness.until(":SELECTION_BARRIER");
    assert!(!harness.text().contains("Confirm delete"), "{}", harness.text());
    harness.write(b"\x1b");
    // An ESC and R read together are one Alt-R key, so the refresh must
    // wait until the command line has closed.
    harness.wait(|harness| !harness.text().contains(":SELECTION_BARRIER"));
    write_rows(&harness, &[&alpha, &beta, &row("marker-paste", "id-d", "work")]);
    harness.send(b"R", "marker-paste");
    harness.send(b"/\x1b[200~beta\x1b[201~\r", "beta");
    harness.write(b"d:FILTER_BARRIER");
    harness.until(":FILTER_BARRIER");
    assert!(!harness.text().contains("Confirm delete"), "{}", harness.text());
    harness.write(b"\x1b");
    harness.wait(|harness| !harness.text().contains(":FILTER_BARRIER"));
    harness.write(b"j");
    harness.send(b"d", expected);
    harness.send(b"n", "beta");
    harness.send(b"l", "Follow latest 200 lines");
    if kubernetes {
        harness.write(b"\x1b[B\x1b[B\x1b[B");
    }
    harness.send(b"\r", "LOG_OPTIONS_ACCEPTED");
    harness.until("Exit code 0");
    let commands = logs_calls(&harness);
    assert_eq!(commands.len(), 1, "{commands:?}");
    let args = &commands[0];
    assert!(has(args, "--tail=200") && has(args, "--timestamps"), "{args:?}");
    if kubernetes {
        assert!(has(args, "--previous") && !has(args, "--follow"), "{args:?}");
        assert_eq!(args[index(args, "--container") + 1], "app", "{args:?}");
    } else {
        assert!(has(args, "--follow") && args.last().map(String::as_str) == Some("id-b"), "{args:?}");
    }
    harness.send(b"\r", "beta");
    harness.wait(|harness| !harness.text().contains("[loading]"));
    if kubernetes {
        harness.send(b"m", "related-events");
        harness.send(b"\x1b[B\x1b[B\x1b[B\x1b[B\r", "kubectl events");
        harness.wait(|harness| !harness.text().contains("[loading]"));
        let related: Vec<Vec<String>> =
            harness.calls().into_iter().map(|(_, args)| args).filter(|args| has(args, "events")).collect();
        let last = related.last().unwrap_or_else(|| panic!("no events query: {related:?}"));
        let get = index(last, "get");
        assert_eq!(
            last[get..(get + 4).min(last.len())],
            ["get", "events", "--field-selector", "involvedObject.uid=id-b"],
            "{related:?}"
        );
        assert_eq!(last[index(last, "--namespace") + 1], "west", "{related:?}");
    }
    harness.send(b"f", "Favorite toggled");
    let saved: Value = serde_json::from_str(&fs::read_to_string(harness.root.join(".hamn/tui.json")).unwrap()).unwrap();
    assert_eq!(saved["favorites"].as_array().map(Vec::len), Some(1), "{saved}");
    harness.send(b"F", "Favorite targets");
    harness.send(b"\r", "beta");
    assert!(no_deletes(&harness), "{:?}", harness.calls());
}

fn compose_navigation() {
    let mut harness = Harness::new("containers");
    harness.until("old-target-row");
    write_rows(&harness, &[&json!({"ID": "service-id", "Names": "project-service", "State": "running"})]);
    select_peer(&harness.root, "docker", "tui-review-improvements-compose");
    harness.send(b":compose ls\r", "review-project");
    harness.send(b"m", "Resource actions");
    assert!(harness.text().contains("Project containers"), "{}", harness.text());
    assert!(!harness.text().contains("related-pods") && !harness.text().contains("inspect"), "{}", harness.text());
    harness.send(b"\x1b", "review-project");
    harness.send(b"\r", "project-service");
    let queries: Vec<Vec<String>> = harness
        .calls()
        .into_iter()
        .map(|(_, args)| args)
        .filter(|args| has(args, "ps") && has(args, "--filter"))
        .collect();
    let last = queries.last().unwrap_or_else(|| panic!("no filtered ps: {queries:?}"));
    assert_eq!(last[index(last, "--filter") + 1], "label=com.docker.compose.project=review-project", "{queries:?}");
    assert!(!harness.calls().iter().any(|(_, args)| has(args, "down")), "{:?}", harness.calls());
}

/// The peer that lists `rows.json` and records log options.
pub fn peer(program: &str, args: &[String]) -> ExitCode {
    serve(program, args, false)
}

/// The same peer that also lists one Compose project.
pub fn compose_peer(program: &str, args: &[String]) -> ExitCode {
    serve(program, args, true)
}

fn serve(program: &str, args: &[String], compose: bool) -> ExitCode {
    let root = PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"));
    record(&root, program, args);
    if compose && has(args, "compose") {
        println!("{}", json!([{"Name": "review-project", "Status": "running(1)", "ConfigFiles": "compose.yml"}]));
    } else if has(args, "get") || has(args, "ps") {
        let rows: Value = serde_json::from_str(&fs::read_to_string(root.join("rows.json")).unwrap()).unwrap();
        if has(args, "get") {
            println!("{}", json!({"items": rows}));
        } else {
            for row in rows.as_array().expect("rows.json is an array") {
                println!("{row}");
            }
        }
    } else if has(args, "logs") {
        println!("LOG_OPTIONS_ACCEPTED");
    } else {
        println!("ACTION_DONE");
    }
    ExitCode::SUCCESS
}
