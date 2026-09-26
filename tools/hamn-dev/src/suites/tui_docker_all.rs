//! Compares the TUI all toggle with the installed Docker CLI against an
//! isolated Unix-socket Engine API: each flag combination's all/size/limit
//! query matches the direct CLI through three toggles, and grouped root host
//! options suppress Hamn's defaults.
use crate::runner::{self, case};
use crate::support::docker_engine;
use crate::support::http::{Reply, Request, Server};
use crate::support::py_text::{self, parse_qs, splitlines};
use crate::support::real_cli::{self, run};
use crate::support::tui::Harness;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    let Some(docker) = real_cli::which("docker") else {
        println!("SKIP: installed Docker unavailable; all-flag parsing is covered in Rust");
        return ExitCode::SUCCESS;
    };
    runner::run(
        "tui-docker-all",
        "grouped Docker root host options suppress defaults and preserve structured browsing",
        vec![case("main", move || docker_all(&docker))],
        filters,
    )
}

type Query = BTreeMap<String, Vec<String>>;

/// The Engine API: records each container list query and names its one
/// container after the query's `all` and `size` values.
fn engine(socket: &Path, requests: Arc<Mutex<Vec<Query>>>) -> Server {
    docker_engine::serve(socket, move |request: &Request| -> Reply {
        let (path, query) = py_text::urlsplit(&request.target);
        let body = if path.ends_with("/_ping") {
            b"OK".to_vec()
        } else if path.ends_with("/containers/json") {
            let query = parse_qs(query);
            let mut name = if is_one(&query, "all") { "all" } else { "running" }.to_owned();
            name += if is_one(&query, "size") { "-size" } else { "-plain" };
            requests.lock().unwrap().push(query);
            json!([{"Id": "a".repeat(64), "Names": [format!("/{name}")],
                "Image": "fixture", "ImageID": "b".repeat(64), "Command": "fixture",
                "Created": 0, "State": "running", "Status": "Up", "Ports": [],
                "Labels": {}, "SizeRw": 42, "SizeRootFs": 100}])
            .to_string()
            .into_bytes()
        } else {
            return docker_engine::not_found(request);
        };
        docker_engine::reply(request, &body, Some("application/json"))
    })
}

/// Whether the query's `filters` has `label` among its label filters
/// (`label in json.loads(query.get('filters', ['{}'])[0]).get('label', [])`).
fn has_label(query: &Query, label: &str) -> bool {
    let filters = query.get("filters").map_or("{}", |values| values[0].as_str());
    let filters: Value = serde_json::from_str(filters).unwrap();
    contains(filters.get("label").unwrap_or(&json!([])), label)
}

/// Python's `item in value` for a JSON object (its keys) or array.
fn contains(value: &Value, item: &str) -> bool {
    match value {
        Value::Object(map) => map.contains_key(item),
        Value::Array(items) => items.iter().any(|value| value.as_str() == Some(item)),
        other => panic!("cannot test membership in {other}"),
    }
}

fn is_one(query: &Query, name: &str) -> bool {
    query.get(name).map(Vec::as_slice) == Some(&["1".to_owned()][..])
}

fn docker_all(docker: &Path) {
    let requests: Arc<Mutex<Vec<Query>>> = Arc::default();
    // Declared before the harness so that it stops after Hamn does.
    let _server;
    let mut harness = Harness::new("containers");
    harness.until("old-target-row");
    let root = harness.root.clone();
    let profile = root.join(".hamn/default");
    match fs::DirBuilder::new().mode(0o700).create(&profile) {
        Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => panic!("{}: {error}", profile.display()),
        _ => {}
    }
    let endpoint = profile.join("docker.sock");
    _server = engine(&endpoint, Arc::clone(&requests));
    // Replace only this fixture's CLI peer; never inherit a user's Docker target.
    real_cli::wrap(&root, "docker", docker, "docker-api-1.47");
    let host = format!("unix://{}", endpoint.display());
    let direct = |args: &[&str]| {
        let mut command = Command::new(docker);
        real_cli::without_docker_variables(command.args(args).env("HOME", &root));
        run(&mut command, Duration::from_secs(10))
    };
    let last = || requests.lock().unwrap().last().cloned().expect("a container list request");
    let cases: [(&[&str], bool); 14] = [
        (&["-as"], true),
        (&["-sa=false"], false),
        (&["-a=true"], true),
        (&["--all=true", "--all=false"], false),
        (&["--all=false", "-as"], true),
        (&["-s", "-f", "label=app=api"], false),
        (&["-sf", "label=app=api"], false),
        (&["-asf", "label=app=api"], true),
        (&["--all=TRUE"], true),
        (&["-a=0"], false),
        (&["-asn5"], true),
        (&["--size", "--size=false"], false),
        (&["--size=false", "-s"], false),
        (&["-as=false"], true),
    ];
    for (case_index, (flags, all_value)) in cases.into_iter().enumerate() {
        let command = [&["ps"][..], flags].concat();
        let direct_marker = format!("direct={case_index}");
        let filter = format!("label={direct_marker}");
        let output =
            direct(&[&["--host", &host][..], &command, &["--filter", &filter, "--format", "{{json .}}"]].concat());
        assert_eq!(output.returncode, 0, "{command:?} {}", output.stderr());
        let baseline = requests
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|query| has_label(query, &direct_marker))
            .cloned()
            .expect("the direct query");
        assert_eq!(is_one(&baseline, "all"), all_value, "{command:?} {baseline:?}");
        let size = is_one(&baseline, "size");
        let displayed: Value = serde_json::from_str(&output.stdout()).unwrap();
        let displayed_size = displayed["Size"].as_str().expect("Size").to_owned();
        let plain = direct(&[&["--host", &host][..], &command].concat());
        assert_eq!(plain.returncode, 0, "{}", plain.stderr());
        // Docker's JSON formatter evaluates Size even without --size. The
        // ordinary CLI header establishes whether the user requested it.
        let plain_stdout = plain.stdout();
        let show_size = py_text::split(splitlines(&plain_stdout)[0]).contains(&"SIZE");
        // Include a sequence marker in the filter so a stale screen cannot pass.
        let marker = format!("label=case={case_index}");
        let text = format!("docker ps {} --filter {marker}", flags.join(" "));
        harness.write(format!(":{text}\r").as_bytes());
        let observed = |value: bool| {
            let query = last();
            is_one(&query, "all") == value && has_label(&query, &marker[6..])
        };
        harness.wait(|_| observed(all_value));
        let suffix = if size { "-size" } else { "-plain" };
        for value in [!all_value, all_value, !all_value] {
            harness.write(b"a");
            harness.wait(|_| observed(value));
            harness.until(&format!("{}{suffix}", if value { "all" } else { "running" }));
            let screen = harness.text();
            assert_eq!(screen.contains("Size"), show_size, "{command:?}\n{screen}");
            if show_size {
                assert!(screen.contains(&displayed_size), "{command:?} {displayed_size:?}\n{screen}");
            }
            let query = last();
            assert_eq!(is_one(&query, "size"), size, "{command:?} {query:?}");
            assert_eq!(query.get("limit"), baseline.get("limit"), "{command:?} {query:?}");
            if flags.contains(&"label=app=api") {
                let filters: Value = serde_json::from_str(&query["filters"][0]).unwrap();
                assert!(contains(&filters["label"], "app=api"), "{query:?}");
            }
        }
        println!("PASS: {} direct CLI and three TUI toggles", command.join(" "));
    }
    // A grouped root -H must suppress the UI default and remain a table even
    // when the socket name contains q (not a container-local quiet flag).
    let alias = root.join("query.sock");
    std::os::unix::fs::symlink(&endpoint, &alias).unwrap();
    let alias_host = format!("unix://{}", alias.display());
    for flag in [format!("-DH{alias_host}"), format!("-DH {alias_host}"), format!("-DH={alias_host}")] {
        let output = direct(&[&py_text::split(&flag)[..], &["ps"]].concat());
        assert_eq!(output.returncode, 0, "{}", output.stderr());
        let marker = format!("grouped-host={}", requests.lock().unwrap().len());
        harness.send(format!(":docker {flag} ps --filter label={marker}\r").as_bytes(), &alias.display().to_string());
        harness.wait(|_| has_label(&last(), &marker));
        harness.until("running-size");
        let screen = harness.text();
        assert!(!screen.contains("Hamn profile") && !screen.contains("docker terminal"), "{screen}");
        harness.send(b":ps\r", "Hamn profile default");
    }
}

/// The `docker` wrapper: execs the installed Docker without any inherited
/// `DOCKER_*` variable and with `DOCKER_API_VERSION=1.47`.
pub fn docker_api_1_47(_program: &str, args: &[String]) -> ExitCode {
    real_cli::exec(&mut real_cli::docker_command(&real_cli::real("docker"), args))
}
