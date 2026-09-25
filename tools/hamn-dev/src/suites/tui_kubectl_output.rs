//! The installed kubectl keeps grouped output and live watch semantics in
//! the TUI: grouped scope flags select the same API path as the direct CLI,
//! output formats and an explicit server survive into the selected detail,
//! and a watch streams until SIGINT, then restores the list.
use super::tui_cluster_target::assert_hamn_directory_holds_preferences_only;
use crate::runner::{self, case};
use crate::support::http::{Options, Reply, Request, Response, Server};
use crate::support::kube_api;
use crate::support::pty;
use crate::support::py_http;
use crate::support::py_text::{self, parse_qs, splitlines, strip};
use crate::support::real_cli::{self, Reaped, communicate, read_until, run};
use crate::support::tui::{self, Harness};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    let Some(kubectl) = real_cli::which("kubectl") else {
        println!("SKIP: installed kubectl unavailable; grouped options are covered in Rust");
        return ExitCode::SUCCESS;
    };
    runner::run(
        "tui-kubectl-output",
        "grouped kubectl output/watch and explicit server retained by selected detail; SIGINT and restoration",
        vec![case("main", move || kubectl_output(&kubectl))],
        filters,
    )
}

/// (path, parsed query, server label) of every pod request either API saw.
type Requests = Arc<Mutex<Vec<(String, BTreeMap<String, Vec<String>>, String)>>>;

/// `threading.Event`: watch responses stay open until it is set.
#[derive(Default)]
struct Release {
    released: Mutex<bool>,
    changed: Condvar,
}

impl Release {
    fn set(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }

    fn wait(&self, timeout: Duration) {
        let released = self.released.lock().unwrap();
        drop(self.changed.wait_timeout_while(released, timeout, |released| !*released).unwrap());
    }
}

/// Sets the release when the test ends, before Hamn is stopped.
struct SetOnDrop(Arc<Release>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.set();
    }
}

fn pod(name: &str, namespace: &str) -> Value {
    json!({"apiVersion": "v1", "kind": "Pod", "metadata": {"name": name,
        "namespace": namespace, "uid": "fixture-uid", "resourceVersion": "1"},
        "status": {"phase": "Running"}})
}

/// `DiscoveryApi` whose pod requests are recorded and answered with
/// `<label>-fixture`; a watch gets one ADDED event and stays open until
/// `release` (at most 30 seconds).
fn api(label: Arc<Mutex<String>>, requests: Requests, release: Arc<Release>) -> Server {
    Server::tcp(Options::default(), move |request: &Request| {
        py_http::dispatch(request, &["GET"], || {
            let (path, query) = py_text::urlsplit(&request.target);
            if !path.contains("/pods") {
                return kube_api::discovery(&request.target).into();
            }
            let label = label.lock().unwrap().clone();
            let namespace = match path.split_once("/namespaces/") {
                Some((_, rest)) => rest.split('/').next().unwrap(),
                None => "test",
            };
            let query = parse_qs(query);
            let watching =
                matches!(query.get("watch").map(Vec::as_slice), Some([value]) if value == "true" || value == "1");
            requests.lock().unwrap().push((path.to_owned(), query, label.clone()));
            let mut value = if watching {
                json!({"type": "ADDED", "object": pod("live-watch-row", "test")})
            } else {
                json!({"apiVersion": "v1", "kind": "PodList", "metadata": {"resourceVersion": "1"},
                    "items": [pod(&format!("{label}-fixture"), namespace)]})
            };
            if path.contains("/pods/") {
                value = pod(&format!("{label}-fixture"), namespace);
            }
            let data = format!("{value}\n");
            if !watching {
                return Response::new(200, data).header("Content-Type", "application/json").into();
            }
            // An HTTP/1.0 body without a length: it ends when the
            // connection closes, after the release.
            let release = Arc::clone(&release);
            Reply::Raw(Box::new(move |stream| {
                let head = "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n";
                let _ = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(data.as_bytes()));
                let _ = stream.flush();
                release.wait(Duration::from_secs(30));
            }))
        })
    })
}

fn kubectl_output(kubectl: &Path) {
    let requests: Requests = Arc::default();
    let release = Arc::new(Release::default());
    // Dropped in reverse: the direct watch is killed, the release set and
    // Hamn stopped before the APIs, as in the Python test's `finally`.
    let mut servers: Vec<Server> = Vec::new();
    let mut harness = Harness::with(tui::Options { namespace: Some("ui-ns"), ..tui::Options::new("kubernetes") });
    let _release = SetOnDrop(Arc::clone(&release));
    let mut direct_watch = Reaped(None);
    harness.until("old-target-row");
    let label = Arc::new(Mutex::new("format".to_owned()));
    servers.push(api(Arc::clone(&label), Arc::clone(&requests), Arc::clone(&release)));
    let root = harness.root.clone();
    let config = root.join("kubeconfig");
    let mut content: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    content["clusters"][0]["cluster"]["server"] = json!(format!("http://127.0.0.1:{}", servers[0].port()));
    fs::write(&config, content.to_string()).unwrap();
    let before = fs::read(&config).unwrap();
    real_cli::wrap(&root, "kubectl", kubectl, "exec-real");
    let direct = |args: &[&str]| {
        run(Command::new(kubectl).args(args).env("HOME", &root).env("KUBECONFIG", &config), Duration::from_secs(10))
    };
    let last = || requests.lock().unwrap().last().cloned().expect("a pod request");

    let mut scopes: Vec<(String, bool)> = [
        ("--all-namespaces=false", false),
        ("-A=false", false),
        ("-A --all-namespaces=false", false),
        ("-AA=false", false),
        ("--all-namespaces=false -A", true),
        ("-A=0", false),
        ("-A=False", false),
        ("-A=TRUE", true),
    ]
    .into_iter()
    .map(|(flags, all)| (flags.to_owned(), all))
    .collect();
    for separator in [" ", "="] {
        for value in ["-nteam", "--context=other", "--kubeconfig=other", "-oyaml", "--watch", "--help"] {
            scopes.push((format!("--as reviewer --as-group{separator}{value}"), false));
        }
    }
    for (index, (flags, all_namespaces)) in scopes.iter().enumerate() {
        *label.lock().unwrap() = format!("scope-{index}");
        let mut args = vec!["--context", "old-cluster", "--namespace", "ui-ns", "get", "pods"];
        args.extend(py_text::split(flags));
        let output = direct(&args[..]);
        assert_eq!(output.returncode, 0, "{}", output.stderr());
        let expected = if *all_namespaces { "/api/v1/pods" } else { "/api/v1/namespaces/ui-ns/pods" };
        assert_eq!(last().0, expected, "{:?}", requests.lock().unwrap());
        harness.send(format!(":get pods {flags}\r").as_bytes(), &format!("scope-{index}-fixture"));
        assert_eq!(last().0, expected, "{:?}", requests.lock().unwrap());
        assert_eq!(harness.text().contains("all namespaces"), *all_namespaces, "{}", harness.text());
        assert!(!harness.text().contains("Exit code"), "consumed value changed query into PTY output");
    }
    *label.lock().unwrap() = "format".to_owned();
    harness.send(b":get pods\r", "format-fixture");
    // --cascade has an optional value. Its following namespace flag must
    // remain an override; client dry-run performs no server-side deletion.
    let args =
        ["delete", "pod", "format-fixture", "--cascade", "--namespace", "explicit", "--dry-run=client", "-o", "name"];
    let output = direct(&[&["--context", "old-cluster"][..], &args[..]].concat());
    assert_eq!(output.returncode, 0, "{}", output.stderr());
    harness.send(format!(":{}\r", args.join(" ")).as_bytes(), "Exit code 0");
    let screen = harness.text();
    assert!(splitlines(&output.stdout()).iter().all(|line| screen.contains(strip(line))), "{screen}");
    assert!(screen.contains("--namespace explicit"), "{screen}");
    harness.send(b"\r", "[Kubernetes]");
    harness.until("format-fixture");
    for flags in [&["-Aoyaml"][..], &["-Ao", "yaml"], &["-Aojson"]] {
        let args = [&["get", "pods"][..], flags].concat();
        let output = direct(&args[..]);
        assert_eq!(output.returncode, 0, "{}", output.stderr());
        harness.send(format!(":{}\r", args.join(" ")).as_bytes(), "Exit code 0");
        let screen = harness.text();
        // Complete line checks distinguish YAML from JSON and preserve values.
        for line in splitlines(&output.stdout()) {
            assert!(screen.contains(strip(line)), "{flags:?} {line:?}\n{screen}");
        }
        assert_eq!(last().0, "/api/v1/pods", "{:?}", requests.lock().unwrap());
        harness.send(b"\r", "[Kubernetes]");
        harness.until("format-fixture");
    }

    let alternate_label = Arc::new(Mutex::new("explicit".to_owned()));
    servers.push(api(alternate_label, Arc::clone(&requests), Arc::clone(&release)));
    let endpoint = format!("http://127.0.0.1:{}", servers[1].port());
    for flag in [format!("-As{endpoint}"), format!("-As {endpoint}"), format!("-As={endpoint}")] {
        let args = [&["get", "pods"][..], &py_text::split(&flag)].concat();
        let output = direct(&args[..]);
        assert!(output.returncode == 0 && output.stdout().contains("explicit-fixture"), "{output:?}");
        harness.send(format!(":get pods {flag}\r").as_bytes(), "explicit-fixture");
        let header = harness.text();
        harness.send(b"\r", "Exit code 0");
        assert!(harness.text().contains("name: explicit-fixture"), "{}", harness.text());
        let (path, _, served_by) = last();
        assert!(path.ends_with("/pods/explicit-fixture") && served_by == "explicit", "{:?}", requests.lock().unwrap());
        assert!(header.contains(&endpoint), "{header}");
        harness.send(b"\r", "[Kubernetes]");
        harness.until("explicit-fixture");
        harness.send(b":get pods\r", "format-fixture");
    }

    let child = Command::new(kubectl)
        .args(["get", "pods", "-Aw"])
        .env("HOME", &root)
        .env("KUBECONFIG", &config)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kubectl watch");
    let child = direct_watch.0.insert(child);
    read_until(child, b"live-watch-row", Duration::from_secs(10));
    assert!(child.try_wait().unwrap().is_none(), "watch must display events before exit");
    pty::kill(child.id(), libc::SIGINT);
    let code = communicate(direct_watch.0.take().unwrap(), Duration::from_secs(5)).returncode;
    let expected = if code >= 0 { code } else { 128 - code };
    harness.send(b":get pods -Aw\r", "live-watch-row");
    assert!(!harness.text().contains("Exit code"), "{}", harness.text());
    let (path, query, _) = last();
    assert_eq!(path, "/api/v1/pods", "{:?}", requests.lock().unwrap());
    assert!(
        matches!(query.get("watch").map(Vec::as_slice), Some([value]) if value == "true" || value == "1"),
        "{:?}",
        requests.lock().unwrap()
    );
    harness.send(b"\x03", &format!("Exit code {expected}"));
    harness.send(b"\r", "[Kubernetes]");
    harness.until("format-fixture");
    assert!(fs::read(&config).unwrap() == before, "query changed kubeconfig");
    assert_hamn_directory_holds_preferences_only(&root);
}
