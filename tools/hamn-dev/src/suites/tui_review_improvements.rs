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
    for workspace in ["containers", "kubernetes"] {
        cases.push(case(format!("row_keys/{workspace}"), move || row_keys(workspace)));
    }
    runner::run(
        "tui-review-improvements",
        "selection identity, removal, pasted filters, diagnostics, log options, relationships, Compose, favorites \
         and the k9s row keys",
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
    harness.send(b"\x04", expected); // Ctrl+D
    if kubernetes {
        assert!(harness.text().contains("selected namespace: west"), "{}", harness.text());
    }
    harness.send(b"n", "marker-reordered");
    write_rows(&harness, &[&row("marker-removed", "id-d", "work"), &alpha, &gamma]);
    harness.send(b"R", "marker-removed");
    harness.write(b"\x04:SELECTION_BARRIER");
    harness.until(":SELECTION_BARRIER");
    assert!(!harness.text().contains("Confirm delete"), "{}", harness.text());
    harness.write(b"\x1b");
    // An ESC and R read together are one Alt-R key, so the refresh must
    // wait until the command line has closed.
    harness.wait(|harness| !harness.text().contains(":SELECTION_BARRIER"));
    write_rows(&harness, &[&alpha, &beta, &row("marker-paste", "id-d", "work")]);
    harness.send(b"R", "marker-paste");
    harness.send(b"/\x1b[200~beta\x1b[201~\r", "beta");
    harness.write(b"\x04:FILTER_BARRIER");
    harness.until(":FILTER_BARRIER");
    assert!(!harness.text().contains("Confirm delete"), "{}", harness.text());
    harness.write(b"\x1b");
    harness.wait(|harness| !harness.text().contains(":FILTER_BARRIER"));
    harness.write(b"j");
    harness.send(b"\x04", expected); // Ctrl+D
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
        // After describe, yaml, json, logs, shell, port-forward, stats and delete.
        harness.send(&[b"\x1b[B".repeat(8), b"\r".to_vec()].concat(), "kubectl events");
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
    harness.send(b"B", "Favorite targets");
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

/// The shell that `s` opens, as the contract of `native_actions` states it.
const SHELL: &str = "command -v bash >/dev/null && exec bash || exec sh";

/// The arguments of the first call after `start` that holds `verb`; the list
/// queries that refresh meanwhile hold none of the verbs asked for.
fn action_call(harness: &Harness, start: usize, verb: &str) -> Option<Vec<String>> {
    harness.calls().into_iter().skip(start).map(|(_, args)| args).find(|args| has(args, verb))
}

/// The k9s row keys through the real key handler: `d` describe, `y` YAML,
/// `s` shell (a container menu for a Pod with several), `F` port-forward
/// with a validated prompt, and `t` start or stop; each keeps the displayed
/// connection and the row's namespace, and reads open without confirmation.
fn row_keys(workspace: &str) {
    let mut harness = Harness::new(workspace);
    harness.until("old-target-row");
    let kubernetes = workspace == "kubernetes";
    let row = if kubernetes {
        let mut pod = pod("beta", "id-b", "west");
        pod["spec"]["containers"] =
            json!([{"name": "app", "ports": [{"containerPort": 8080}]}, {"name": "sidecar"}]);
        pod
    } else {
        json!({"ID": "id-b", "Names": "beta", "State": "running"})
    };
    write_rows(&harness, &[&row]);
    select_peer(&harness.root, if kubernetes { "kubectl" } else { "docker" }, "tui-review-improvements");
    let (query, connection): (&[u8], &[&str]) = if kubernetes {
        (b":get pods -A --context old-cluster\r", &["--context", "old-cluster", "--namespace", "west"])
    } else {
        (b":docker --context explicit ps\r", &["--context", "explicit"])
    };
    harness.send(query, "beta");
    harness.wait(|harness| !harness.text().contains("[loading]"));
    let expect = |args: Vec<String>, command: &[&str]| {
        let expected: Vec<String> = connection.iter().chain(command).map(|arg| arg.to_string()).collect();
        assert_eq!(args, expected);
    };
    // Runs `keys` and returns the command holding `verb` that it opened in
    // the terminal, with nothing to confirm.
    let run = |harness: &mut Harness, keys: &[u8], output: &str, verb: &str| -> Vec<String> {
        let start = harness.calls().len();
        harness.write(keys);
        harness.until_all(&[output, "Exit code 0"]);
        assert!(!harness.text().contains("Hamn confirmation"), "{}", harness.text());
        let args = action_call(harness, start, verb).unwrap_or_else(|| panic!("no {verb}: {:?}", harness.calls()));
        // The output can name the row too, so the list is back only when the
        // terminal's exit line is gone and the reload it starts has ended;
        // a key pressed earlier meets a loading list and is refused.
        harness.write(b"\r");
        harness.wait(|harness| {
            let text = harness.text();
            !text.contains("Exit code") && !text.contains("[loading]") && text.contains("beta")
        });
        args
    };
    let (describe, detail, verbs) = if kubernetes {
        (vec!["describe", "pods", "beta"], vec!["get", "pods", "beta", "-o", "yaml"], ["describe", "yaml"])
    } else {
        (vec!["container", "inspect", "id-b"], vec!["container", "inspect", "id-b"], ["inspect", "inspect"])
    };
    expect(run(&mut harness, b"d", "ACTION_DONE", verbs[0]), &describe);
    // kubectl get prints the fixture rows; docker inspect is no list query.
    expect(run(&mut harness, b"y", if kubernetes { "{\"items\"" } else { "ACTION_DONE" }, verbs[1]), &detail);
    if kubernetes {
        harness.send(b"s", "Shell in container");
        assert!(harness.text().contains("sidecar"), "{}", harness.text());
        let args = run(&mut harness, b"\x1b[B\r", "ACTION_DONE", "exec");
        expect(args, &["exec", "--stdin", "--tty", "pods/beta", "--container", "sidecar", "--", "sh", "-c", SHELL]);
        // The prompt starts with the declared port and refuses an option.
        harness.send(b"F", "Port-forward pods/beta");
        harness.until("ports 8080:8080");
        let start = harness.calls().len();
        harness.send(&[b"\x7f".repeat(9), b"--address 0.0.0.0\r".to_vec()].concat(), "such as 8080:80");
        assert!(harness.text().contains("Port-forward pods/beta"), "{}", harness.text());
        assert!(action_call(&harness, start, "port-forward").is_none(), "{:?}", harness.calls());
        let typed = [b"\x7f".repeat(17), b"9090:80\r".to_vec()].concat();
        let args = run(&mut harness, &typed, "ACTION_DONE", "port-forward");
        expect(args, &["port-forward", "pods/beta", "9090:80"]);
        // Esc leaves the prompt without forwarding.
        harness.send(b"F", "Port-forward pods/beta");
        let start = harness.calls().len();
        harness.write(b"\x1b");
        harness.wait(|harness| !harness.text().contains("Port-forward pods/beta"));
        assert!(action_call(&harness, start, "port-forward").is_none(), "{:?}", harness.calls());
    } else {
        let args = run(&mut harness, b"s", "ACTION_DONE", "exec");
        expect(args, &["exec", "--interactive", "--tty", "id-b", "sh", "-c", SHELL]);
        harness.send(b"F", "Docker has no port-forward");
        // t stops the running container, after confirmation.
        harness.send(b"t", "Confirm stop containers id-b");
        harness.send(b"n", "beta");
    }
    assert!(no_deletes(&harness), "{:?}", harness.calls());
    assert!(!harness.calls().iter().any(|(_, args)| has(args, "stop")), "{:?}", harness.calls());
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
