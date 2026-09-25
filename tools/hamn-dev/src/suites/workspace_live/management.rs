//! Management review acceptance against an existing, workspace-owned kind
//! cluster; no VM or cluster is started here.
//!
//! Each run owns a random namespace and CRD labelled with its random token,
//! checks their API UIDs and label before cleanup (deleting with a UID
//! precondition), and preserves the caller's kubeconfig bytes. The PTY
//! screens, kubectl API results, process identities and HTTP bodies are
//! independent evidence, written to `management-review-TOKEN.json`.
use super::terminal::{Driver, Terminal, http_get};
use super::{
    Flags, Live, Must, PROFILE, panic_text, path_str, prepare, random_hex, repository, resolved, run_env, uuid4,
    write_json,
};
use crate::release::process::{self, Spec};
use crate::support::py_text;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

pub(crate) const OWNER: &str = "io.hamn.review-owner";
const BROWSER: &[u8] = b"\x1b\x02";
const SESSIONS: &[u8] = b"\x1b\x13";

const USAGE: &str = "usage: hamn-dev test workspace-live-management --root ROOT [--binary BINARY] [--cluster NAME]

Review UX acceptance against an existing, workspace-owned kind cluster.

  --root ROOT      an existing workspace-live ownership root
  --binary BINARY  the root's candidate executable (default build/hamn of this checkout)
  --cluster NAME   the kind cluster in the root's engine (default hamn-workspace-proof)";

pub fn main(args: &[String]) -> ExitCode {
    let flags = match Flags::parse(args, &["root", "binary", "cluster"], &[], USAGE) {
        Ok(Some(flags)) => flags,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("workspace-live-management: {message}");
            return ExitCode::from(2);
        }
    };
    let Some(root) = flags.value("root") else {
        eprintln!("workspace-live-management: --root is required\n{USAGE}");
        return ExitCode::from(2);
    };
    let binary = flags.value("binary").map_or_else(|| repository().join("build/hamn"), PathBuf::from);
    let cluster = flags.value("cluster").unwrap_or("hamn-workspace-proof").to_owned();
    super::catch_interrupts();
    let prepared = fs::canonicalize(&binary)
        .map_err(|error| format!("{}: {error}", binary.display()))
        .and_then(|binary| prepare(&binary, None, Some(Path::new(root))));
    let live = match prepared {
        Ok((root, runtime)) => Live { root, runtime },
        Err(message) => {
            eprintln!("workspace-live-management: {message}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        let config = live.root.join("kubeconfig");
        let socket = format!("unix://{}", live.profile().join("docker.sock").display());
        let environment = live.environment(&[("DOCKER_HOST", &socket), ("KUBECONFIG", path_str(&config))]);
        println!("{}", management_review(&live, &config, &environment, &cluster).display());
    }));
    super::finish("workspace-live-management", outcome.is_ok(), Ok(()))
}

/// Cleanup adopts a resource only while it keeps the UID the run observed
/// and the run's own ownership label.
pub(crate) fn assert_identity(value: &Value, uid: &str, token: &str) -> Result<(), String> {
    if value["metadata"]["uid"] != uid {
        return Err("resource was replaced; refusing cleanup".into());
    }
    if value["metadata"].get("labels").and_then(|labels| labels.get(OWNER)).and_then(Value::as_str) != Some(token) {
        return Err("resource ownership changed".into());
    }
    Ok(())
}

fn uid(value: &Value) -> String {
    value["metadata"]["uid"].to_string()
}

/// Independent oracles for the TUI's relationship queries: the selector
/// matches exactly the deployment's pod (not the decoy), and the events are
/// exactly those whose involved object is that pod's UID.
pub(crate) fn assert_relations(
    pods: &Value,
    events: &Value,
    pod: &Value,
    decoy: &Value,
    current_event: &Value,
    stale_event: &Value,
) -> Result<(), String> {
    let items = |list: &Value| list["items"].as_array().cloned().unwrap_or_default();
    let pod_ids: BTreeSet<String> = items(pods).iter().map(uid).collect();
    let event_ids: BTreeSet<String> = items(events).iter().map(uid).collect();
    if pod_ids != BTreeSet::from([uid(pod)]) || pod_ids.contains(&uid(decoy)) {
        return Err(format!("the selector matched {pod_ids:?}"));
    }
    if !event_ids.contains(&uid(current_event)) || event_ids.contains(&uid(stale_event)) {
        return Err(format!("the pod's events were {event_ids:?}"));
    }
    if !items(events).iter().all(|event| event["involvedObject"]["uid"] == pod["metadata"]["uid"]) {
        return Err("an event involves another object".into());
    }
    Ok(())
}

/// A process row from `ps`: its start time and command line, with its PID
/// and process group, identify it across a PID reuse.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Row {
    pub pid: i32,
    pub parent: i32,
    pub group: i32,
    pub started: String,
    pub command: String,
}

/// Python's `line.split(None, count)`: at most `count` splits at runs of
/// whitespace, the rest kept whole.
fn split_fields(line: &str, count: usize) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut rest = line.trim_start_matches(py_text::is_space);
    while !rest.is_empty() && fields.len() < count {
        let end = rest.find(py_text::is_space).unwrap_or(rest.len());
        fields.push(&rest[..end]);
        rest = rest[end..].trim_start_matches(py_text::is_space);
    }
    if !rest.is_empty() {
        fields.push(rest);
    }
    fields
}

/// Every process, by PID.
pub(crate) fn processes() -> BTreeMap<i32, Row> {
    let table = process::run(
        OsStr::new("/bin/ps"),
        &["-axo", "pid=,ppid=,pgid=,lstart=,args="],
        &Spec::default(),
        Duration::from_secs(30),
    )
    .must();
    let mut rows = BTreeMap::new();
    for line in table.lines() {
        let parts = split_fields(line, 8);
        if parts.len() == 9 {
            let number = |text: &str| text.parse::<i32>().unwrap_or_else(|error| panic!("{error}: {line}"));
            let row = Row {
                pid: number(parts[0]),
                parent: number(parts[1]),
                group: number(parts[2]),
                started: parts[3..8].join(" "),
                command: parts[8].to_owned(),
            };
            rows.insert(row.pid, row);
        }
    }
    rows
}

/// Whether `observed` is still the process `expected` recorded; a reused
/// PID has another start time, group or command.
pub(crate) fn same_process(expected: &Row, observed: Option<&Row>) -> bool {
    observed.is_some_and(|observed| {
        (observed.pid, observed.group, &observed.started, &observed.command)
            == (expected.pid, expected.group, &expected.started, &expected.command)
    })
}

/// Waits up to `timeout` until none of `owned` is alive, polling the
/// process table every 50 ms.
fn wait_gone(owned: &[Row], timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let current = processes();
        let alive: Vec<&Row> = owned.iter().filter(|row| same_process(row, current.get(&row.pid))).collect();
        if alive.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("owned CLI survived cleanup: {alive:?}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Ends leftover owned CLI sessions. Used only after the assertions and
/// the TUI's own cleanup; never signals by command name or an unverified
/// PID: each entry was a direct PTY child of the TUI leading its group.
fn cleanup_processes(owned: &[Row]) -> Result<(), String> {
    let current = processes();
    for row in owned {
        if same_process(row, current.get(&row.pid)) && row.pid == row.group {
            crate::support::pty::kill_group(row.group as u32, libc::SIGTERM);
        }
    }
    wait_gone(owned, Duration::from_secs(10))
}

#[track_caller]
fn assert_port_closed(port: u16) {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    assert!(TcpStream::connect_timeout(&address, Duration::from_secs(1)).is_err(), "forward listener survived: {port}");
}

/// Picks `option` in the list view titled `title`.
pub(crate) fn choose(terminal: &mut impl Driver, title: &str, option: &str) {
    terminal.until(title);
    let text = terminal.text();
    let rows: Vec<&str> = text.lines().map(|line| line.trim_matches(|c| c == ' ' || c == '│')).collect();
    // The title and footer rows are not options (Python's `rows[1:-1]`).
    let inner = if rows.len() > 2 { &rows[1..rows.len() - 1] } else { &[][..] };
    let options: Vec<&str> = inner
        .iter()
        .filter(|line| !py_text::strip(line).is_empty())
        .map(|line| py_text::strip(line.strip_prefix("> ").unwrap_or(line)))
        .collect();
    let index = options.iter().position(|candidate| *candidate == option);
    let index = index.unwrap_or_else(|| panic!("{option:?} is not in {options:?}"));
    let mut keys = b"\x1b[B".repeat(index);
    keys.push(b'\r');
    terminal.send(&keys, None);
}

/// Queries `kind name` (in `namespace`) and waits until it is selected and
/// loaded.
pub(crate) fn query(terminal: &mut impl Driver, kind: &str, name: &str, namespace: Option<&str>) {
    let suffix = namespace.map(|namespace| format!(" --namespace {namespace}")).unwrap_or_default();
    terminal.send(format!(":kubectl get {kind} {name}{suffix}\r").as_bytes(), None);
    let (heading, selected) = (format!("kubectl {kind}"), format!("> {name}"));
    terminal.wait_for(&|text| text.contains(&heading) && text.contains(&selected) && !text.contains("[loading]"));
}

struct Review<'a> {
    live: &'a Live,
    config: PathBuf,
    environment: &'a BTreeMap<String, String>,
    context: String,
    token: String,
    namespace: String,
    /// (kind, name, API UID once observed), in creation order.
    owned: Vec<(String, String, Option<String>)>,
    children: Vec<Row>,
    ports: Vec<TcpListener>,
    terminal: Option<Terminal>,
    evidence: Value,
}

impl Review<'_> {
    fn kubectl_within(&self, args: &[&str], timeout: Duration, input: Option<&[u8]>) -> Result<String, String> {
        let all = [&["--kubeconfig", path_str(&self.config), "--context", &self.context][..], args].concat();
        run_env("kubectl", &all, self.environment, timeout, input)
    }

    fn kubectl(&self, args: &[&str]) -> String {
        self.kubectl_within(args, Duration::from_secs(150), None).must()
    }

    fn get(&self, kind: &str, name: &str, namespace: Option<&str>) -> Value {
        let mut args = vec!["get", kind, name];
        if let Some(namespace) = namespace {
            args.extend(["--namespace", namespace]);
        }
        args.extend(["-o", "json"]);
        serde_json::from_str(&self.kubectl(&args)).expect("kubectl JSON")
    }

    fn create(&mut self, value: Value) -> Value {
        let output = self
            .kubectl_within(
                &["create", "-f", "-", "-o", "json"],
                Duration::from_secs(150),
                Some(value.to_string().as_bytes()),
            )
            .must();
        let result: Value = serde_json::from_str(&output).expect("kubectl JSON");
        let key = format!(
            "{}/{}",
            result["kind"].as_str().unwrap_or_default(),
            result["metadata"]["name"].as_str().unwrap_or_default()
        );
        self.evidence["resources"][key] = result.clone();
        result
    }

    fn http(&self, port: u16) -> String {
        let body = http_get(port);
        assert_eq!(body, format!("{}\n", self.token).as_bytes(), "{port}: {}", String::from_utf8_lossy(&body));
        String::from_utf8(body).expect("UTF-8 body")
    }

    fn terminal(&mut self) -> &mut Terminal {
        self.terminal.as_mut().expect("a running TUI")
    }

    /// The TUI's direct CLI children for this namespace.
    fn cli_children(&mut self, filter: impl Fn(&Row) -> bool) -> Vec<Row> {
        let parent = self.terminal().child.id() as i32;
        let namespace = self.namespace.clone();
        processes()
            .into_values()
            .filter(|row| row.parent == parent && row.command.contains(&namespace) && filter(row))
            .collect()
    }

    fn body(&mut self, before_config: &[u8]) {
        assert_eq!(py_text::strip(&self.kubectl(&["config", "current-context"])), self.context);
        let label = json!({OWNER: self.token});
        let namespace = self.namespace.clone();
        let group = format!("r{}.hamn.test", &self.token[..10]);
        let crd_name = format!("probes.{group}");
        self.owned.push(("namespace".into(), namespace.clone(), None));
        let created = self.create(json!({"apiVersion": "v1", "kind": "Namespace",
            "metadata": {"name": namespace, "labels": label}}));
        self.owned.last_mut().expect("owned namespace").2 = created["metadata"]["uid"].as_str().map(str::to_owned);
        self.owned.push(("customresourcedefinition".into(), crd_name.clone(), None));
        let crd = self.create(json!({"apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
            "metadata": {"name": crd_name, "labels": label}, "spec": {"group": group, "scope": "Namespaced",
            "names": {"plural": "probes", "singular": "probe", "kind": "ReviewProbe"},
            "versions": [{"name": "v1", "served": true, "storage": true, "schema": {"openAPIV3Schema": {
                "type": "object", "properties": {"spec": {"type": "object",
                    "properties": {"message": {"type": "string"}}}}}}}]}}));
        self.owned.last_mut().expect("owned CRD").2 = crd["metadata"]["uid"].as_str().map(str::to_owned);
        self.kubectl(&["wait", "--for=condition=Established", &format!("crd/{crd_name}"), "--timeout=60s"]);
        let custom = self.create(json!({"apiVersion": format!("{group}/v1"), "kind": "ReviewProbe",
            "metadata": {"name": "read-only", "namespace": namespace, "labels": label},
            "spec": {"message": self.token}}));
        let token = self.token.clone();
        let command =
            format!("mkdir -p /www; echo {token} > /www/index.html; echo LOG-{token}; exec httpd -f -p 8080 -h /www");
        let container = json!({"name": "web", "image": "busybox:1.37", "imagePullPolicy": "IfNotPresent",
            "command": ["sh", "-c", command]});
        let deployment = self.create(json!({"apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": "web", "namespace": namespace, "labels": label}, "spec": {"replicas": 1,
            "selector": {"matchLabels": {"review": token},
                "matchExpressions": [{"key": "tier", "operator": "In", "values": ["frontend"]}]},
            "template": {"metadata": {"labels": {"review": token, "tier": "frontend"}},
                "spec": {"containers": [container]}}}}));
        let decoy = self.create(json!({"apiVersion": "v1", "kind": "Pod", "metadata": {"name": "decoy",
            "namespace": namespace, "labels": {"review": token, "tier": "backend"}},
            "spec": {"containers": [container]}}));
        self.kubectl(&["rollout", "status", "deployment/web", "--namespace", &namespace, "--timeout=120s"]);
        let selector = format!("review={token},tier in (frontend)");
        let pods: Value = serde_json::from_str(&self.kubectl(&[
            "get",
            "pods",
            "--namespace",
            &namespace,
            "--selector",
            &selector,
            "-o",
            "json",
        ]))
        .expect("kubectl JSON");
        let items = pods["items"].as_array().expect("pod list");
        assert_eq!(items.len(), 1, "{pods}");
        let pod = items[0].clone();
        let pod_name = pod["metadata"]["name"].as_str().expect("pod name").to_owned();
        let pod_uid = pod["metadata"]["uid"].as_str().expect("pod UID").to_owned();
        self.kubectl(&[
            "wait",
            "--for=condition=Ready",
            &format!("pod/{pod_name}"),
            "--namespace",
            &namespace,
            "--timeout=120s",
        ]);
        let owner = pod["metadata"]["ownerReferences"][0]["name"].as_str().expect("pod owner").to_owned();
        let replica = self.get("replicaset", &owner, Some(&namespace));
        assert_eq!(replica["metadata"]["ownerReferences"][0]["uid"], deployment["metadata"]["uid"]);
        let mut current_event = Value::Null;
        let mut stale_event = Value::Null;
        for (name, involved) in [("selected-event", pod_uid.clone()), ("stale-event", uuid4())] {
            let selected = involved == pod_uid;
            let event = self.create(json!({"apiVersion": "v1", "kind": "Event",
                "metadata": {"name": name, "namespace": namespace},
                "involvedObject": {"apiVersion": "v1", "kind": "Pod", "name": pod_name, "namespace": namespace, "uid": involved},
                "reason": if selected { "ReviewSelected" } else { "ReviewStale" }, "message": token, "type": "Normal"}));
            if selected {
                current_event = event;
            } else {
                stale_event = event;
            }
        }
        let events: Value = serde_json::from_str(&self.kubectl(&[
            "get",
            "events",
            "--namespace",
            &namespace,
            "--field-selector",
            &format!("involvedObject.uid={pod_uid}"),
            "-o",
            "json",
        ]))
        .expect("kubectl JSON");
        assert_relations(&pods, &events, &pod, &decoy, &current_event, &stale_event).must();
        self.evidence["independentRelations"] = json!({"pods": pods, "events": events, "replicaSet": replica});
        let logs = self.kubectl(&["logs", &pod_name, "--namespace", &namespace, "--container", "web", "--tail=200"]);
        assert!(logs.contains(&format!("LOG-{token}")), "{logs}");

        let mut environment = self.environment.clone();
        environment.insert("KUBECONFIG".into(), path_str(&self.config).into());
        self.terminal = Some(Terminal::new(&self.live.runtime.binary, &environment, &self.live.root));
        let first_run = !self.live.runtime.home.join(".hamn/tui.json").exists();
        let terminal = self.terminal();
        if first_run {
            terminal.until("Choose your default workspace");
            terminal.send(b"1\r", None);
        }
        terminal.until(": command");
        // The unrelated pod sorts first in a namespace-wide list. Select the
        // intended starting object explicitly; the relationship queries
        // below must still prove that their selectors exclude the decoy.
        query(terminal, "pods", &pod_name, Some(&namespace));
        for (kind, name, namespace_value, expected) in [
            ("crds".to_owned(), crd_name.clone(), None, &crd),
            (format!("probes.{group}"), "read-only".to_owned(), Some(namespace.as_str()), &custom),
        ] {
            let terminal = self.terminal.as_mut().expect("a running TUI");
            query(terminal, &kind, &name, namespace_value);
            terminal.send(b"m", Some("Resource actions"));
            let menu = terminal.text();
            assert!(
                menu.contains("inspect")
                    && ["delete", "restart", "logs", "related-"].iter().all(|action| !menu.contains(action)),
                "{menu}"
            );
            terminal.send(b"\r", Some("Exit code 0"));
            let expected_uid = expected["metadata"]["uid"].as_str().expect("UID").to_owned();
            if !terminal.text().contains(&expected_uid) {
                terminal.send(b"\x1b[5~", Some(&expected_uid));
            }
            terminal.send(b"\r", None);
            query(terminal, &kind, &name, namespace_value);
            terminal.send(b"d:READ_ONLY_BARRIER", Some(":READ_ONLY_BARRIER"));
            assert!(!terminal.text().contains("Confirm delete"), "{}", terminal.text());
            terminal.send(b"\x1b", None);
            let observed = self.get(&kind, &name, namespace_value);
            assert!(
                observed["metadata"]["uid"] == expected["metadata"]["uid"] && observed["spec"] == expected["spec"],
                "a read-only resource changed: {observed}"
            );
        }
        let terminal = self.terminal();
        query(terminal, "deployments", "web", Some(&namespace));
        terminal.send(b"m", Some("related-pods"));
        choose(terminal, "Resource actions", "related-pods");
        terminal.until(&format!("> {pod_name}"));
        terminal.wait_for(&|text| !text.contains("[loading]"));
        assert!(!terminal.text().contains("decoy"), "{}", terminal.text());
        let related_pods = terminal.text();
        terminal.send(b"m", Some("related-events"));
        choose(terminal, "Resource actions", "related-events");
        terminal.until("selected-event");
        terminal.wait_for(&|text| !text.contains("[loading]"));
        assert!(!terminal.text().contains("stale-event"), "{}", terminal.text());
        let related_events = terminal.text();
        query(terminal, "pods", &pod_name, Some(&namespace));
        terminal.send(b"l", Some("Logs (Enter selects"));
        choose(terminal, "Logs (Enter selects", "web: follow latest 200 lines");
        terminal.until(&format!("LOG-{token}"));
        terminal.send(BROWSER, Some(&format!("> {pod_name}")));
        self.evidence["relatedPodsScreen"] = json!(related_pods);
        self.evidence["relatedEventsScreen"] = json!(related_events);
        for _ in 0..2 {
            self.ports.push(TcpListener::bind("127.0.0.1:0").expect("reserve a loopback port"));
        }
        let port_numbers: Vec<u16> = self.ports.iter().map(|listener| listener.local_addr().unwrap().port()).collect();
        for &port in &port_numbers {
            // Free the reserved port just before the forward binds it.
            self.ports.retain(|listener| listener.local_addr().is_ok_and(|address| address.port() != port));
            let terminal = self.terminal();
            terminal.send(
                format!(":kubectl port-forward --namespace {namespace} pod/{pod_name} {port}:8080\r").as_bytes(),
                Some(&format!("Forwarding from 127.0.0.1:{port}")),
            );
            self.http(port);
            self.terminal().send(BROWSER, Some(&format!("> {pod_name}")));
        }
        self.children =
            self.cli_children(|row| row.command.contains(" logs ") || row.command.contains(" port-forward "));
        assert!(
            self.children.len() == 3 && self.children.iter().all(|row| row.group == row.pid),
            "{:?}",
            self.children
        );
        self.evidence["ownedCliProcesses"] = json!(self.children);
        let terminal = self.terminal();
        terminal.send(SESSIONS, Some("Sessions: Enter resumes"));
        terminal.until(&format!("logs pods/{pod_name} container=web"));
        for port in &port_numbers {
            terminal.until(&format!("port-forward pod/{pod_name} {port}:8080"));
        }
        self.evidence["sessionsScreen"] = json!(self.terminal().text());
        let detached: BTreeMap<String, String> =
            port_numbers.iter().map(|&port| (port.to_string(), self.http(port))).collect();
        self.evidence["httpWhileDetached"] = json!(detached);
        // The last detached session is selected; closing it must spare the
        // first forward and the independent, still running log stream.
        self.terminal().send(b"d", None);
        let marker = format!("{}:8080", port_numbers[1]);
        let removed: Vec<Row> = self.children.iter().filter(|row| row.command.contains(&marker)).cloned().collect();
        assert_eq!(removed.len(), 1, "{:?}", self.children);
        wait_gone(&removed, Duration::from_secs(10)).must();
        assert_port_closed(port_numbers[1]);
        self.http(port_numbers[0]);
        let current = processes();
        assert!(
            self.children
                .iter()
                .filter(|row| !removed.contains(row))
                .all(|row| same_process(row, current.get(&row.pid))),
            "closing one session ended another"
        );
        self.terminal().send(b"\x1b", Some(&format!("> {pod_name}")));
        let mut terminal = self.terminal.take().expect("a running TUI");
        terminal.close();
        wait_gone(&self.children, Duration::from_secs(10)).must();
        for &port in &port_numbers {
            assert_port_closed(port);
        }
        assert_eq!(fs::read(&self.config).unwrap(), before_config, "the source kubeconfig changed");
        self.evidence["passed"] = json!(true);
        println!(
            "PASS: live CRD/CR read-only, selector/UID navigation, concurrent logs/two forwards and owned cleanup"
        );
    }

    /// Ends leftover sessions and deletes owned objects that still carry
    /// the observed UID and the run's label; returns every cleanup error.
    fn cleanup(&mut self, before_config: &[u8]) -> Vec<String> {
        let mut errors = Vec::new();
        if let Some(mut terminal) = self.terminal.take() {
            let parent = terminal.child.id() as i32;
            let namespace = self.namespace.clone();
            let leftover: Vec<Row> = processes()
                .into_values()
                .filter(|row| row.parent == parent && row.command.contains(&namespace) && row.group == row.pid)
                .collect();
            for row in leftover {
                if !self.children.contains(&row) {
                    self.children.push(row);
                }
            }
            if let Err(payload) = panic::catch_unwind(AssertUnwindSafe(|| terminal.close())) {
                errors.push(panic_text(&*payload));
            }
        }
        if let Err(error) = cleanup_processes(&self.children) {
            errors.push(error);
        }
        self.ports.clear();
        for (kind, name, uid) in self.owned.clone().into_iter().rev() {
            if let Err(error) = self.delete_owned(&kind, &name, uid.as_deref()) {
                errors.push(error);
            }
        }
        if fs::read(&self.config).map_or(true, |bytes| bytes != before_config) {
            errors.push("source kubeconfig changed".into());
        }
        errors
    }

    fn delete_owned(&self, kind: &str, name: &str, uid: Option<&str>) -> Result<(), String> {
        let found = self.kubectl_within(
            &["get", kind, name, "--ignore-not-found", "-o", "json"],
            Duration::from_secs(150),
            None,
        )?;
        if found.trim().is_empty() {
            return Ok(());
        }
        let value: Value = serde_json::from_str(&found).map_err(|error| error.to_string())?;
        // A timed-out create can have succeeded: adopt only the exact random
        // ownership label, with the API UID observed now.
        let observed = value["metadata"]["uid"].as_str().unwrap_or_default().to_owned();
        assert_identity(&value, uid.unwrap_or(&observed), &self.token)?;
        let prefix = if kind == "namespace" {
            "/api/v1/namespaces/"
        } else {
            "/apis/apiextensions.k8s.io/v1/customresourcedefinitions/"
        };
        let body = json!({"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": observed}});
        let raw = format!("{prefix}{name}");
        self.kubectl_within(
            &["delete", "--raw", &raw, "--filename", "-"],
            Duration::from_secs(90),
            Some(body.to_string().as_bytes()),
        )?;
        self.kubectl_within(
            &["wait", "--for=delete", &format!("{kind}/{name}"), "--timeout=90s"],
            Duration::from_secs(150),
            None,
        )?;
        Ok(())
    }
}

/// Runs the review against `cluster` in the root's engine with `config`
/// (in the root) and returns the evidence file.
pub(crate) fn management_review(
    live: &Live,
    config: &Path,
    environment: &BTreeMap<String, String>,
    cluster: &str,
) -> PathBuf {
    live.assert_owned();
    let config = resolved(config);
    let owner = crate::release::files::read_json(&live.root.join("ownership.json")).must();
    assert!(
        owner["profile"] == PROFILE
            && resolved(Path::new(owner["home"].as_str().unwrap_or_default())) == resolved(&live.runtime.home),
        "the root does not own this runtime"
    );
    assert_eq!(config.parent(), Some(resolved(&live.root).as_path()), "the kubeconfig is not the root's");
    let socket = format!("unix://{}", live.profile().join("docker.sock").display());
    assert_eq!(environment.get("DOCKER_HOST"), Some(&socket), "Docker is not the owned engine");
    let before_config = fs::read(&config).unwrap_or_else(|error| panic!("{}: {error}", config.display()));
    let inspected: Value =
        serde_json::from_str(&live.docker(&["inspect", &format!("{cluster}-control-plane")])).expect("inspect JSON");
    assert_eq!(inspected[0]["Config"]["Labels"]["io.x-k8s.kind.cluster"], cluster, "not the kind cluster {cluster}");
    let token = random_hex(16);
    let namespace = format!("review-{}", &token[..10]);
    let evidence_path = live.root.join(format!("management-review-{token}.json"));
    let candidate = crate::release::files::sha256_file(&live.runtime.binary).must();
    let kubeconfig: String = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&before_config).iter().map(|byte| format!("{byte:02x}")).collect()
    };
    let context = format!("kind-{cluster}");
    let mut review = Review {
        live,
        config,
        environment,
        evidence: json!({"runId": token, "cluster": cluster, "context": context, "namespace": namespace,
            "candidateSHA256": candidate, "kubeconfigSHA256": kubeconfig, "resources": {}}),
        context,
        token,
        namespace,
        owned: Vec::new(),
        children: Vec::new(),
        ports: Vec::new(),
        terminal: None,
    };
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| review.body(&before_config)));
    let errors = review.cleanup(&before_config);
    review.evidence["cleanupErrors"] = json!(errors);
    write_json(&evidence_path, &review.evidence);
    assert!(errors.is_empty(), "{}: {errors:?}", evidence_path.display());
    if let Err(payload) = outcome {
        panic::resume_unwind(payload);
    }
    evidence_path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_split_like_python_with_a_limit() {
        assert_eq!(
            split_fields("  1 2  3 Sat Sep 26 10:00:00 2026 /bin/x  -a  b ", 8),
            ["1", "2", "3", "Sat", "Sep", "26", "10:00:00", "2026", "/bin/x  -a  b "]
        );
        assert_eq!(split_fields("a b", 8), ["a", "b"]);
        assert!(split_fields("   ", 8).is_empty());
    }

    #[test]
    fn the_process_table_lists_this_process() {
        let rows = processes();
        let own = &rows[&(std::process::id() as i32)];
        assert_eq!(own.parent, std::os::unix::process::parent_id() as i32);
        assert!(same_process(own, processes().get(&own.pid)));
    }
}
