//! Picker cancellation in real Hamn PTYs with disposable, recorded CLI
//! peers: cancelling a picker restores the query, filter and selection
//! across workspace switches, while an actual choice or a new query commits
//! the navigation.
use super::tui_native_regressions::record;
use crate::runner::{self, Case, case};
use crate::support::harness_peers::select_peer;
use crate::support::http::{Options as HttpOptions, Response, Server};
use crate::support::tui::Harness;
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

pub fn main(filters: &[String]) -> ExitCode {
    let mut cases: Vec<Case> = Vec::new();
    for (label, command) in [
        ("host", "docker --host unix:///explicit.sock ps --filter label=app=x"),
        ("context", "docker --context explicit ps --filter label=app=x"),
    ] {
        cases.push(case(format!("cancel_preserves_query/containers/{label}"), move || {
            cancel_preserves_query("containers", command, b"e", "Container environments", true)
        }));
    }
    for (key, title) in [(b"e", "k8s contexts list"), (b"n", "k8s namespaces list")] {
        let label = key_label(key);
        cases.push(case(format!("cancel_preserves_query/kubernetes/{label}"), move || {
            cancel_preserves_query(
                "kubernetes",
                "get pods --context explicit --namespace chosen -l app=x",
                key,
                title,
                true,
            )
        }));
    }
    for (workspace, key, title) in [
        ("containers", b"e", "Container environments"),
        ("kubernetes", b"e", "k8s contexts list"),
        ("kubernetes", b"n", "k8s namespaces list"),
    ] {
        for typed in [false, true] {
            let label = key_label(key);
            cases.push(case(
                format!("committed_navigation_is_not_undone/{workspace}/{label}/typed={typed}"),
                move || committed_navigation_is_not_undone(workspace, key, title, typed),
            ));
        }
    }
    cases.push(case("committed_navigation_is_not_undone/containers/v/typed=true", || {
        committed_navigation_is_not_undone("containers", b"v", "vm list", true)
    }));
    runner::run(
        "tui-picker-restore",
        "picker cancellation preserves query/filter/selection across workspace switches; actual choices and new queries commit navigation",
        cases,
        filters,
    )
}

fn key_label(key: &[u8; 1]) -> char {
    char::from(key[0])
}

/// Replaces both CLI peers with the picker peer.
fn prepare(harness: &mut Harness) {
    harness.until("old-target-row");
    for program in ["docker", "kubectl"] {
        select_peer(&harness.root, program, "tui-picker-restore");
    }
}

fn has(args: &[String], value: &str) -> bool {
    args.iter().any(|arg| arg == value)
}

fn cancel_preserves_query(workspace: &str, command: &str, key: &[u8; 1], title: &str, switch: bool) {
    let mut harness = Harness::new(workspace);
    prepare(&mut harness);
    let before_config = fs::read(harness.root.join("kubeconfig")).unwrap();
    harness.send(format!(":{command}\r").as_bytes(), "row-two");
    let original = harness.calls().pop().expect("the query was recorded");
    harness.send(b"/row\rj:SELECTION_BARRIER", ":SELECTION_BARRIER");
    harness.write(b"\x1b");
    harness.wait(|harness| !harness.text().contains(":SELECTION_BARRIER"));
    harness.send(key, title);
    assert!(!harness.text().contains("row-two") || key == b"n", "{}", harness.text());
    if switch {
        let other = if workspace == "containers" { "[Kubernetes]" } else { "[Containers]" };
        harness.send(b"\t", other);
        harness.send(b"\t", title);
        // Reopening a selector also keeps the first return destination.
        harness.send(&[&key[..], b":REOPEN_BARRIER"].concat(), ":REOPEN_BARRIER");
        harness.write(b"\x1b");
        harness.wait(|harness| !harness.text().contains(":REOPEN_BARRIER"));
    }
    let start = harness.calls().len();
    harness.write(b"\x1b");
    harness.wait(|harness| !harness.text().contains(title));
    harness.until("row-two");
    harness.wait(|harness| harness.calls().iter().skip(start).any(|call| *call == original));
    harness.wait(|harness| !harness.text().contains("[loading]"));
    assert!(!harness.text().contains(title), "{}", harness.text());
    assert!(!harness.text().contains("outside"), "{}", harness.text());
    // The filter and selected row must survive, including after the fresh query.
    harness.send(b"d", "Confirm delete");
    assert!(harness.text().contains("row-two"), "{}", harness.text());
    assert!(harness.text().contains("explicit"), "{}", harness.text());
    assert!(!harness.calls().iter().any(|(_, args)| has(args, "delete") || has(args, "rm")), "{:?}", harness.calls());
    harness.send(b"n", if workspace == "containers" { "[Containers]" } else { "[Kubernetes]" });
    assert_eq!(fs::read(harness.root.join("kubeconfig")).unwrap(), before_config);
}

fn committed_navigation_is_not_undone(workspace: &str, key: &[u8; 1], title: &str, typed: bool) {
    // Declared before the harness so that the harness closes first.
    let server: Option<Server>;
    let mut harness = Harness::new(workspace);
    prepare(&mut harness);
    if key == b"n" {
        let namespaces = Server::tcp(HttpOptions::default(), |request| {
            assert!(request.target.starts_with("/api/v1/namespaces"), "{}", request.target);
            let items: Vec<Value> = ["row-one", "row-two"]
                .into_iter()
                .map(|name| {
                    json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {
                        "name": name, "uid": format!("namespace-{name}"), "resourceVersion": "1"}})
                })
                .collect();
            Response::json(200, &json!({"apiVersion": "v1", "kind": "NamespaceList", "items": items})).into()
        });
        let config = harness.root.join("kubeconfig");
        let mut content: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        content["clusters"][0]["cluster"]["server"] = json!(namespaces.url("http"));
        fs::write(&config, content.to_string()).unwrap();
        server = Some(namespaces);
    } else {
        server = None;
    }
    let original_config = fs::read(harness.root.join("kubeconfig")).unwrap();
    let mut command = if workspace == "containers" {
        "docker --context explicit ps --filter label=old"
    } else {
        "get pods --context explicit --namespace chosen -l app=old"
    };
    if key == b"v" {
        command = "ps --filter label=old";
    }
    harness.send(format!(":{command}\r").as_bytes(), "row-two");
    harness.send(key, title);
    if typed {
        let query = if workspace == "containers" {
            "docker --context replacement ps --filter label=new"
        } else {
            "get pods --context replacement --namespace replacement -l app=new"
        };
        harness.send(format!(":{query}\r").as_bytes(), "--context replacement");
    } else {
        let choice = if workspace == "containers" {
            "external"
        } else if key == b"e" {
            "old-cluster"
        } else {
            "row-two"
        };
        harness.until(choice);
        harness.wait(|harness| !harness.text().contains("[loading]"));
        // Start filtering only after the picker response has rendered. A
        // hidden selection must clear, not move to the remaining row.
        harness.send(format!("/{choice}\r:PICKER_FILTER_BARRIER").as_bytes(), ":PICKER_FILTER_BARRIER");
        harness.write(b"\x1b");
        harness.wait(|harness| !harness.text().contains(":PICKER_FILTER_BARRIER"));
        if key == b"n" {
            assert!(!harness.text().contains("> row-two"), "{}", harness.text());
        }
        // Filtering is not a selection action. Explicitly select the visible
        // choice and wait for its completed frame before committing it.
        harness.write(b"j");
        harness.until(&format!("> {choice}"));
        harness.send(b"\r", if workspace == "containers" { "docker containers" } else { "kubectl pods" });
    }
    harness.until("outside");
    harness.wait(|harness| !harness.text().contains("[loading]"));
    let (_, current) = harness.calls().pop().expect("a query was recorded");
    assert!(!has(&current, "explicit"), "{current:?}");
    assert!(!["label=old", "app=old"].iter().any(|value| has(&current, value)), "{current:?}");
    if !typed && key == b"n" {
        let namespace = current.iter().position(|arg| arg == "--namespace").map(|at| current[at + 1].as_str());
        assert_eq!(namespace, Some("row-two"), "{current:?}");
    }
    harness.send(b"/outside\r", "outside");
    harness.wait(|harness| !harness.text().contains("row-one"));
    harness.send(b"\x1b", "row-one");
    let text = harness.text();
    assert!(text.contains("outside") && !text.contains(title), "{text}");
    assert!(!text.contains("--context explicit"), "{text}");
    if typed {
        assert!(text.contains("--context replacement"), "{text}");
    }
    assert_eq!(fs::read(harness.root.join("kubeconfig")).unwrap(), original_config);
    drop(harness);
    drop(server);
}

/// The recorded peer that lists three rows for any query and one Docker
/// context.
pub fn peer(program: &str, args: &[String]) -> ExitCode {
    let root = PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"));
    record(&root, program, args);
    let names = ["row-one", "row-two", "outside"];
    if has(args, "context") {
        println!("{}", json!({"Name": "external", "DockerEndpoint": "unix:///external.sock"}));
    } else if has(args, "ps") {
        for name in names {
            println!("{}", json!({"ID": name, "Names": name, "State": "running"}));
        }
    } else if has(args, "get") {
        let items: Vec<Value> = names
            .into_iter()
            .map(|name| {
                json!({"apiVersion": "v1", "kind": "Pod", "metadata": {
                    "name": name, "namespace": "chosen", "resourceVersion": "1", "uid": format!("uid-{name}")}})
            })
            .collect();
        println!("{}", json!({"items": items}));
    } else {
        println!("ACTION_DONE");
    }
    ExitCode::SUCCESS
}
