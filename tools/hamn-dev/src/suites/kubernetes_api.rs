//! Explicit kubeconfig context routing and mutation preconditions against a
//! fake API server, without a cluster: KUBECONFIG merging, delete identity
//! (UID and resourceVersion), log streams, error classification, a repeated
//! continue token, and the unmodified source kubeconfig.
//!
//! One flow, as in the script it replaces. The script's final step (the
//! retired managed-K3s context: `.kube-contexts` markers, `available` and
//! `managedK3sRemoved`) is not ported: that feature is being removed.
use crate::runner::{self, case};
use crate::support::api_fixtures::{self, Completed, MkdTemp, py_json, reason, respond, truthy, utf8};
use crate::support::hamn;
use crate::support::http::{Options, Reply, Server};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "kubernetes-api",
        "Kubernetes context routing and mutation identity",
        vec![case("kubernetes_context_routing_and_mutation_identity", kubernetes_context_routing_and_mutation_identity)],
        filters,
    )
}

#[derive(Clone, Copy, PartialEq)]
enum List {
    Normal,
    Empty,
    /// Every page names the same continue token.
    Repeat,
}

#[derive(Clone, Copy)]
struct Mode {
    uid: bool,
    delete: u16,
    list: List,
}

/// The recorded requests (method, target, DELETE body) and the behavior the
/// test switches.
struct Cluster {
    requests: Mutex<Vec<(String, String, Option<Value>)>>,
    mode: Mutex<Mode>,
}

impl Cluster {
    fn set(&self, change: impl FnOnce(&mut Mode)) {
        change(&mut self.mode.lock().unwrap());
    }

    fn requests(&self) -> Vec<(String, String, Option<Value>)> {
        self.requests.lock().unwrap().clone()
    }

    fn len(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn deletes_since(&self, start: usize) -> usize {
        self.requests.lock().unwrap()[start..].iter().filter(|(method, _, _)| method == "DELETE").count()
    }

    /// Python's `http.server` with its default HTTP/1.0: one request per
    /// connection, closed after the response.
    fn answer(&self, method: &str, target: &str, body: &[u8], stream: &mut dyn crate::support::http::Stream) {
        let mode = *self.mode.lock().unwrap();
        let json_response = |stream: &mut dyn crate::support::http::Stream, data: &Value, status: u16| {
            let body = py_json(data).into_bytes();
            let headers = [("Content-Type", "application/json".to_owned()), ("Content-Length", body.len().to_string())];
            respond(stream, &format!("HTTP/1.0 {status} {}", reason(status)), &headers, &body);
        };
        match method {
            "GET" => {
                self.requests.lock().unwrap().push(("GET".into(), target.into(), None));
                let mut pod = json!({"apiVersion": "v1", "kind": "Pod", "metadata": {
                    "name": "sample", "namespace": "default", "uid": "pod-original", "resourceVersion": "10"}});
                if !mode.uid {
                    pod["metadata"].as_object_mut().unwrap().remove("uid");
                }
                if target.split('?').next().unwrap_or("").ends_with("/log") {
                    let body = "한글 Pod log\nlast line".as_bytes();
                    let headers = [("Content-Type", "text/plain".to_owned()), ("Content-Length", body.len().to_string())];
                    respond(stream, "HTTP/1.0 200 OK", &headers, body);
                } else if target.contains('?') {
                    let metadata = if mode.list == List::Repeat { json!({"continue": "same"}) } else { json!({}) };
                    let items = if mode.list == List::Normal { json!([pod]) } else { json!([]) };
                    json_response(stream, &json!({"apiVersion": "v1", "kind": "PodList", "metadata": metadata, "items": items}), 200);
                } else {
                    json_response(stream, &pod, 200);
                }
            }
            "DELETE" => {
                let body: Value = serde_json::from_slice(body).expect("DELETE carries a JSON body");
                self.requests.lock().unwrap().push(("DELETE".into(), target.into(), Some(body)));
                let status = if mode.delete == 200 { "Success" } else { "Failure" };
                let data = json!({"apiVersion": "v1", "kind": "Status", "status": status,
                    "message": "fixture", "reason": "Fixture", "code": mode.delete});
                json_response(stream, &data, mode.delete);
            }
            _ => respond(stream, "HTTP/1.0 501 Not Implemented", &[("Connection", "close".into())], b""),
        }
    }
}

fn serve(cluster: &Arc<Cluster>) -> Server {
    let cluster = Arc::clone(cluster);
    Server::tcp(Options::default(), move |request| {
        let (cluster, request) = (Arc::clone(&cluster), request.clone());
        Reply::Raw(Box::new(move |stream| cluster.answer(&request.method, &request.target, &request.body, stream)))
    })
}

struct Hamn {
    binary: PathBuf,
    home: PathBuf,
    config: PathBuf,
}

impl Hamn {
    fn headless(&self, arguments: &[&str], kubeconfig: Option<&str>) -> Completed {
        let mut command = Command::new(&self.binary);
        command.arg("--headless").args(arguments).env("HOME", &self.home).env("PATH", "/usr/bin:/bin");
        if let Some(paths) = kubeconfig {
            command.env("KUBECONFIG", paths);
        }
        api_fixtures::run(&mut command, None, Duration::from_secs(15))
    }

    /// `ARGUMENTS --kubeconfig CONFIG`: the exit status and JSON envelope.
    fn run(&self, arguments: &[&str]) -> (bool, Value) {
        let mut arguments = arguments.to_vec();
        arguments.extend(["--kubeconfig", utf8(&self.config)]);
        let completed = self.headless(&arguments, None);
        (completed.success(), completed.json())
    }
}

fn names(value: &Value) -> Vec<String> {
    let rows = value["data"].as_array().unwrap_or_else(|| panic!("context rows: {value}"));
    rows.iter().map(|row| row.get("name").and_then(Value::as_str).expect("context name").to_owned()).collect()
}

fn kubernetes_context_routing_and_mutation_identity() {
    let directory = MkdTemp::new("hamn-kubernetes-");
    let home = directory.path();
    let config_path = home.join("config");
    let cluster = Arc::new(Cluster {
        requests: Mutex::new(Vec::new()),
        mode: Mutex::new(Mode { uid: true, delete: 200, list: List::Normal }),
    });
    let server = serve(&cluster);
    let config = json!({"apiVersion": "v1", "kind": "Config", "current-context": "production",
        "clusters": [{"name": "fixture", "cluster": {"server": format!("http://127.0.0.1:{}", server.port())}}],
        "contexts": [{"name": "dev", "context": {"cluster": "fixture", "namespace": "default"}}]});
    fs::write(&config_path, py_json(&config)).unwrap();
    let original = fs::read(&config_path).unwrap();
    let hamn = Hamn { binary: hamn(), home: home.to_path_buf(), config: config_path.clone() };

    let missing = hamn.headless(&["k8s", "contexts", "list"], Some(""));
    assert!(missing.success() && missing.json()["data"] == json!([]), "{missing:?}");
    let (ok, value) = hamn.run(&["k8s", "contexts", "list"]);
    assert!(ok && value["data"][0]["name"] == "dev", "{value}");
    assert!(cluster.requests().is_empty());
    let absent = home.join("absent");
    let malformed = home.join("malformed");
    fs::write(&malformed, "contexts: [invalid yaml").unwrap();
    let (absent_text, config_text) = (utf8(&absent), utf8(&config_path));
    for paths in [format!("{absent_text}:{config_text}"), format!("{config_text}:{absent_text}"), absent_text.to_owned()] {
        let merged = hamn.headless(&["k8s", "contexts", "list"], Some(&paths));
        let expected: Vec<String> = if paths == absent_text { vec![] } else { vec!["dev".into()] };
        assert!(merged.success(), "{merged:?}");
        assert_eq!(names(&merged.json()), expected);
    }
    // Missing environment entries are optional; explicit or malformed input is not.
    for (paths, flags) in [
        (config_text.to_owned(), vec!["--kubeconfig", absent_text]),
        (format!("{}:{config_text}", utf8(&malformed)), vec![]),
    ] {
        let mut arguments = vec!["k8s", "contexts", "list"];
        arguments.extend(flags);
        let rejected = hamn.headless(&arguments, Some(&paths));
        assert!(!rejected.success(), "{rejected:?}");
        assert_eq!(rejected.json()["error"]["code"], "configurationInvalid");
    }
    assert!(cluster.requests().is_empty());

    let (ok, value) = hamn.run(&["k8s", "pods", "list", "--context", "dev"]);
    assert!(ok && value["data"][0]["metadata"]["name"] == "sample", "{value}");
    let delete = ["k8s", "pods", "delete", "sample", "--context", "dev", "--namespace", "default"];
    let (ok, value) = hamn.run(&[&delete[..], &["--uid", "replaced", "--yes"]].concat());
    assert!(!ok && value["error"]["code"] == "conflict", "{value}");
    assert_eq!(cluster.deletes_since(0), 0);
    let (ok, value) = hamn.run(&[&delete[..], &["--uid", "pod-original", "--yes"]].concat());
    assert!(ok, "{value}");
    let requests = cluster.requests();
    let body = requests.last().and_then(|(_, _, body)| body.as_ref()).expect("the last request is a DELETE");
    assert_eq!(body["preconditions"], json!({"uid": "pod-original", "resourceVersion": "10"}));
    for flags in [&[][..], &["--follow"][..]] {
        let mut arguments = vec!["k8s", "pods", "logs", "sample", "--context", "dev", "--namespace", "default", "--kubeconfig", config_text];
        arguments.extend_from_slice(flags);
        let logs = hamn.headless(&arguments, None);
        let records: Vec<Value> = logs.stdout.lines().map(api_fixtures::parse).collect();
        assert!(logs.success() && records.len() == 3, "{records:?} {}", logs.stderr);
        assert_eq!(records[0]["data"]["text"], "한글 Pod log\n");
        assert_eq!(records[1]["data"]["text"], "last line");
        let last = records.last().unwrap();
        assert!(last["type"] == "result" && truthy(last.get("ok")), "{last}");
    }
    cluster.set(|mode| mode.delete = 503);
    let before = cluster.len();
    let (ok, value) = hamn.run(&[&delete[..], &["--yes"]].concat());
    assert!(!ok && value["error"]["code"] == "outcomeUnknown", "{value}");
    assert_eq!(cluster.len(), before + 2); // one identity GET and exactly one DELETE
    cluster.set(|mode| mode.delete = 403);
    let (ok, value) = hamn.run(&[&delete[..], &["--yes"]].concat());
    assert!(!ok && value["error"]["code"] == "permissionDenied", "{value}");
    cluster.set(|mode| mode.uid = false);
    let before = cluster.len();
    let (ok, value) = hamn.run(&[&delete[..], &["--yes"]].concat());
    assert!(!ok && value["error"]["code"] == "invalidResponse", "{value}");
    assert_eq!(cluster.deletes_since(before), 0);
    cluster.set(|mode| mode.list = List::Empty);
    assert_eq!(hamn.run(&["k8s", "pods", "list", "--context", "dev"]).1["data"], json!([]));
    cluster.set(|mode| mode.list = List::Repeat);
    let before = cluster.len();
    let (ok, value) = hamn.run(&["k8s", "pods", "list", "--context", "dev"]);
    assert!(!ok && value["error"]["code"] == "invalidResponse", "{value}");
    assert_eq!(cluster.len(), before + 2);
    assert_eq!(fs::read(&config_path).unwrap(), original);
    assert!(!home.join(".hamn").exists());
    drop(server);
}
