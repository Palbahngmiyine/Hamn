//! Actual kubectl endpoint overrides remain visible in the list, the PTY
//! and the delete confirmation: both `--cluster` spellings select the
//! alternate API, and the kubeconfig stays unchanged.
use crate::runner::{self, case};
use crate::support::http::{Options, Reply, Request, Server};
use crate::support::kube_api;
use crate::support::py_http;
use crate::support::py_text;
use crate::support::real_cli::{self, run};
use crate::support::tui::Harness;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    let Some(kubectl) = real_cli::which("kubectl") else {
        println!("SKIP: installed kubectl unavailable for cluster target comparison");
        return ExitCode::SUCCESS;
    };
    runner::run(
        "tui-cluster-target",
        "both cluster override spellings select the alternate API and appear in list, confirmation and detail PTY; \
         kubeconfig unchanged",
        vec![case("main", move || cluster_target(&kubectl))],
        filters,
    )
}

/// (server label, request target) for every request either API served.
type Requests = Arc<Mutex<Vec<(String, String)>>>;

/// `DiscoveryApi` extended with a per-server label and pod name.
fn api(label: &'static str, pod_name: Arc<Mutex<String>>, requests: Requests) -> Server {
    Server::tcp(Options::default(), move |request: &Request| {
        py_http::dispatch(request, &["GET"], || {
            requests.lock().unwrap().push((label.to_owned(), request.target.clone()));
            if !request.target.contains("/pods") {
                return kube_api::discovery(&request.target).into();
            }
            let pod = json!({"apiVersion": "v1", "kind": "Pod", "metadata": {
                "name": *pod_name.lock().unwrap(), "namespace": "test",
                "uid": "fixture-uid", "resourceVersion": "1"}});
            let value = if request.target.contains("/pods/") {
                pod
            } else {
                json!({"apiVersion": "v1", "kind": "PodList", "items": [pod]})
            };
            Reply::from(kube_api::json_response(&value))
        })
    })
}

fn cluster_target(kubectl: &Path) {
    let requests: Requests = Arc::default();
    // Declared before the harness so that they stop after Hamn does.
    let mut servers: Vec<Server> = Vec::new();
    let mut harness = Harness::new("kubernetes");
    harness.until("old-target-row");
    let root = harness.root.clone();
    let mut pod_names = Vec::new();
    for label in ["default", "alternate"] {
        let pod_name = Arc::new(Mutex::new("initial".to_owned()));
        servers.push(api(label, Arc::clone(&pod_name), Arc::clone(&requests)));
        pod_names.push(pod_name);
    }
    let config = root.join("kubeconfig");
    let mut content: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    content["clusters"][0]["cluster"]["server"] = json!(format!("http://127.0.0.1:{}", servers[0].port()));
    content["clusters"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "alternate", "cluster": {"server": format!("http://127.0.0.1:{}", servers[1].port())}}));
    fs::write(&config, content.to_string()).unwrap();
    let before = fs::read(&config).unwrap();
    real_cli::wrap(&root, "kubectl", kubectl, "exec-real");
    let last = || requests.lock().unwrap().last().cloned().expect("a request");
    for (index, flag) in ["--cluster alternate", "--cluster=alternate"].into_iter().enumerate() {
        // Unique response names prevent a previous rendered query passing a new case.
        let name = format!("cluster-case-{index}");
        *pod_names[1].lock().unwrap() = name.clone();
        let direct = run(
            Command::new(kubectl)
                .args(["--context", "old-cluster", "--namespace", "test", "get", "pods"])
                .args(py_text::split(flag))
                .env("HOME", &root)
                .env("KUBECONFIG", &config),
            Duration::from_secs(10),
        );
        assert!(direct.returncode == 0 && direct.stdout().contains(&name), "{direct:?}");
        assert_eq!(last().0, "alternate", "{:?}", requests.lock().unwrap());
        // The Python test waited for the name alone. But the previous list
        // refreshes from the alternate API too (on the return from the detail
        // and every 2 seconds), and with this fast wrapper that refresh shows
        // the new name before the command is even typed; Python's slower
        // wrapper startup only hid the race. The command line echoes the flag
        // while it is typed, so the new list is identified by its target line,
        // which shows the flag once the new query has succeeded.
        harness.write(format!(":get pods {flag}\r").as_bytes());
        let loaded = format!("{flag}: Available");
        harness.wait(|harness| {
            let text = harness.text();
            text.contains(&name) && text.contains(&loaded)
        });
        assert!(harness.text().contains(flag), "{}", harness.text());
        assert_eq!(last().0, "alternate", "{:?}", requests.lock().unwrap());
        harness.send(b"d", "Confirm delete");
        assert!(harness.text().contains(flag), "{}", harness.text());
        harness.send(b"n", "[Kubernetes]"); // Review confirmation without changing resources.
        harness.until(&name);
        harness.send(b"\r", "Exit code 0");
        let text = harness.text();
        let header: Vec<&str> = py_text::splitlines(&text).into_iter().take(2).collect();
        assert!(header.join("\n").contains(flag), "{text}");
        let (label, target) = last();
        assert!(label == "alternate" && target.contains(&format!("/pods/{name}")), "{:?}", requests.lock().unwrap());
        harness.send(b"\r", "[Kubernetes]");
    }
    assert_eq!(fs::read(&config).unwrap(), before);
    assert_hamn_directory_holds_preferences_only(&root);
}

/// Only the preferences (and their lock) remain in `root/.hamn`.
pub fn assert_hamn_directory_holds_preferences_only(root: &Path) {
    let mut names: Vec<String> = fs::read_dir(root.join(".hamn"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert!(names == ["tui.json"] || names == ["tui.json", "tui.lock"], "{names:?}");
}
