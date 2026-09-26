//! `workspace-live-checks`: the live harness's guards, oracles and fixtures,
//! checked without a VM, user configuration or network. Guards are fed the
//! failures they exist to reject; the generated guest scripts and the
//! transport fixtures run for real.
use super::boundaries::{HELPERS, LOCK, Programs, gate_source, wait_source, wrapper_source};
use super::cancellation::read_line;
use super::contexts::{Proxy, births, certificates, provider, roots, wrapper};
use super::kubernetes::assert_deployment_preserved;
use super::management::{OWNER, Row, assert_identity, assert_relations, choose, query, same_process};
use super::processes::{Identity, Table, birth, gone};
use super::terminal::Driver;
use super::transport::owned_ssh;
use super::{Headless, PROFILE, finally, prepare, repository, start_isolated};
use crate::runner::{self, case};
use crate::support::exec::{self, output_within, wait_timeout};
use crate::support::harness_peers::select_peer;
use crate::support::py_text::shlex_join;
use crate::support::tmp::TempDir;
use crate::support::tui::{Harness, install_fixture};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{IpAddr, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, mpsc};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "workspace-live-checks",
        "live workspace guards, oracles, guest barrier scripts and transport fixtures",
        vec![
            case("transport/ssh_ownership_ancestry_and_argument_guards", ssh_ownership_guards),
            case("boundaries/wrapper_dispatches_the_holder_or_queued_dispatch_once", wrapper_dispatch),
            case("boundaries/gate_runs_the_dispatch_only_after_release", gate_waits_for_release),
            case(
                "prepare/profile_is_created_with_home_sharing_disabled_before_start",
                home_sharing_disabled_before_start,
            ),
            case("prepare/running_profile_with_home_sharing_is_not_modified", running_profile_is_not_modified),
            case("prepare/resume_reuses_a_read_only_candidate_and_rejects_changed_bytes", resume_reuses_the_candidate),
            case("management/cleanup_rejects_replacement_or_changed_owner", cleanup_rejects_replacement),
            case("management/selector_and_uid_oracles_reject_unrelated_rows", relation_oracles),
            case("management/reused_pid_is_not_treated_as_the_owned_cli", reused_pid),
            case("management/live_menu_helpers_navigate_custom_inspect_and_relationships", live_menu_helpers),
            case("contexts/mtls_forwards_exact_bytes_and_rejects_wrong_ca_and_missing_client", mtls_forwarding),
            case("contexts/stall_has_a_synchronized_entry_and_releases_owned_threads", stalled_transport),
            case("contexts/wrapper_executes_real_program_and_observes_its_exact_birth", witnessed_wrapper),
            case("kubernetes/controller_status_and_revision_do_not_imply_a_restart", controller_changes_pass),
            case("kubernetes/replacement_spec_change_or_lost_concurrent_edit_fails", lost_changes_fail),
        ],
        filters,
    )
}

/// A process table given as data, for the SSH ownership guards.
struct Graph {
    processes: BTreeMap<i32, Identity>,
    /// Successive `argv` answers; the last one repeats.
    argv: RefCell<Vec<Vec<String>>>,
}

impl Table for Graph {
    fn identity(&self, pid: i32) -> Result<Identity, String> {
        self.processes.get(&pid).cloned().ok_or_else(|| format!("no process {pid}"))
    }

    fn children(&self, pid: i32) -> Result<Vec<i32>, String> {
        Ok(self.processes.values().filter(|process| process.ppid == pid).map(|process| process.pid).collect())
    }

    fn argv(&self, _pid: i32) -> Result<Vec<String>, String> {
        let mut answers = self.argv.borrow_mut();
        Ok(if answers.len() > 1 { answers.remove(0) } else { answers[0].clone() })
    }
}

fn ssh_ownership_guards() {
    let binary = Path::new("/tmp/owned/hamn");
    let profile = Path::new("/tmp/owned/home/.hamn/verify");
    let worker = Identity {
        pid: 20,
        ppid: 10,
        start_sec: 100,
        start_usec: 2,
        executable_uuid: "ab".repeat(16),
        path: binary.display().to_string(),
    };
    let supervisor = Identity { pid: 30, ppid: 20, ..worker.clone() };
    let ssh = Identity { pid: 40, ppid: 30, path: "/usr/bin/ssh".into(), ..worker.clone() };
    let token = "a".repeat(32);
    let transaction = format!("{HELPERS}/guest-deployment-transaction");
    let args: Vec<String> = vec![
        "ssh".into(),
        "-i".into(),
        profile.join("id_ed25519").display().to_string(),
        "-o".into(),
        format!("ControlPath={}", profile.join("ssh.sock").display()),
        shlex_join(&["sudo", "flock", LOCK, &transaction, "begin", &token]),
    ];
    let record = serde_json::to_value(&worker).unwrap();
    for fault in [
        None,
        Some("ppid"),
        Some("startSec"),
        Some("startUsec"),
        Some("executableUuid"),
        Some("key"),
        Some("action"),
        Some("changed"),
        Some("ambiguous"),
    ] {
        let mut processes = BTreeMap::from([(20, worker.clone()), (30, supervisor.clone()), (40, ssh.clone())]);
        let mut actual = args.clone();
        let changed = processes.get_mut(&20).unwrap();
        match fault {
            Some("ppid") => changed.ppid = 999,
            Some("startSec") => changed.start_sec = 999,
            Some("startUsec") => changed.start_usec = 999,
            Some("executableUuid") => changed.executable_uuid = "different".into(),
            Some("key") => actual[2] = "/tmp/unowned/id_ed25519".into(),
            Some("action") => actual[5] = actual[5].replace("begin", "commit"),
            Some("ambiguous") => {
                processes.insert(41, Identity { pid: 41, ..ssh.clone() });
            }
            _ => {}
        }
        let mut answers = vec![actual.clone()];
        if fault == Some("changed") {
            answers.push([&args[..], &["changed".to_owned()]].concat());
        }
        let graph = Graph { processes, argv: RefCell::new(answers) };
        match (fault, owned_ssh(&graph, 10, &record, binary, profile)) {
            (None, Ok(found)) => assert_eq!(found, ssh),
            (None, Err(error)) => panic!("valid owned process rejected: {error}"),
            (Some(fault), Ok(_)) => panic!("unsafe process accepted: {fault}"),
            (Some(_), Err(_)) => {}
        }
    }
}

fn argv_programs(root: &Path) -> (PathBuf, PathBuf) {
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    install_fixture(&bin, "flock");
    install_fixture(&bin, "bash");
    (bin.join("flock"), bin.join("bash"))
}

/// Runs `program args` with the argument recorder selected and returns the
/// program and arguments it last recorded.
fn recorded_run(root: &Path, program: &Path, args: &[&str]) -> (String, Vec<String>) {
    let output = output_within(
        Command::new(program)
            .args(args)
            .env("FIXTURE_ROOT", root)
            .env("HAMN_DEV_FIXTURE", "workspace-live-argv")
            .stdin(Stdio::null()),
        Duration::from_secs(10),
    );
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let calls = fs::read_to_string(root.join("calls")).unwrap();
    let last: (String, Vec<String>) = serde_json::from_str(calls.lines().last().unwrap()).unwrap();
    last
}

fn wrapper_dispatch() {
    let token = "a".repeat(32);
    let transaction = format!("{HELPERS}/guest-deployment-transaction");
    for action in ["begin", "commit"] {
        for queued in [false, true] {
            let temporary = TempDir::new("hamn-flock-fixture-");
            let root = temporary.path();
            let (flock, shell) = argv_programs(root);
            let (flock, shell) = (flock.to_str().unwrap(), shell.to_str().unwrap());
            let barrier = root.join("barrier");
            fs::create_dir(&barrier).unwrap();
            let barrier_text = barrier.to_str().unwrap();
            let script = root.join("flock-wrapper");
            fs::write(&script, wrapper_source(barrier_text, action, queued, &Programs { flock, shell })).unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
            let args = ["--wait", "120", LOCK, "bash", &transaction, action, &token];
            // Another transaction step neither claims the barrier nor
            // changes on its way to flock.
            let other = ["--wait", "120", LOCK, "bash", &transaction, "rollback", &token];
            assert_eq!(recorded_run(root, &script, &other), ("flock".into(), other.map(String::from).to_vec()));
            let unlocked = ["--wait", "120", "/run/other.lock", "bash", &transaction, action, &token];
            assert_eq!(recorded_run(root, &script, &unlocked), ("flock".into(), unlocked.map(String::from).to_vec()));
            assert!(!barrier.join("claimed").exists());
            let gate = format!("{barrier_text}/gate.sh");
            let expected: (String, Vec<&str>) = if queued {
                ("bash".into(), [&[gate.as_str(), flock][..], &args].concat())
            } else {
                ("flock".into(), [&args[..3], &[shell, gate.as_str()], &args[3..]].concat())
            };
            let first = recorded_run(root, &script, &args);
            assert_eq!(
                (first.0.as_str(), first.1.iter().map(String::as_str).collect::<Vec<_>>()),
                (expected.0.as_str(), expected.1),
                "{action} queued={queued}"
            );
            assert!(barrier.join("claimed").is_dir());
            // One shot: the next dispatch reaches flock unchanged.
            assert_eq!(recorded_run(root, &script, &args), ("flock".into(), args.map(String::from).to_vec()));
        }
    }
}

/// The argument recorder that stands in for flock and bash.
pub fn argv_recorder(program: &str, args: &[String]) -> ExitCode {
    let root = PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"));
    crate::suites::tui_native_regressions::record(&root, program, args);
    ExitCode::SUCCESS
}

fn gate_waits_for_release() {
    let temporary = TempDir::new("hamn-gate-fixture-");
    let root = temporary.path();
    let directory = root.to_str().unwrap();
    let programs = Programs { flock: "/usr/bin/false", shell: "/bin/bash" };
    fs::write(root.join("gate.sh"), gate_source(directory, &programs)).unwrap();
    fs::write(root.join("wait.sh"), wait_source(directory, 10)).unwrap();
    fs::write(root.join("wait-short.sh"), wait_source(directory, 1)).unwrap();
    let ran = root.join("ran");
    let mut gate = exec::Session(
        Command::new("/bin/bash")
            .arg(root.join("gate.sh"))
            .args(["/bin/sh", "-c", "touch \"$0\"; exit 130"])
            .arg(&ran)
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let ready =
        output_within(Command::new("/bin/bash").arg(root.join("wait.sh")).arg("ready"), Duration::from_secs(15));
    assert!(ready.status.success() && ready.stdout == b"LOCK_READY\n", "{ready:?}");
    // A cancelled frontend's session ending must not end the gated dispatch.
    for signal in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
        crate::support::pty::kill(gate.id(), signal);
    }
    assert!(!ran.exists() && !root.join("done").exists(), "the dispatch ran before release");
    fs::write(root.join("released"), "").unwrap();
    let status = wait_timeout(&mut gate.0, Duration::from_secs(15)).expect("the gate ends after release");
    assert_eq!(status.code(), Some(130), "{status:?}");
    assert!(ran.exists());
    assert_eq!(fs::read_to_string(root.join("done")).unwrap(), "130");
    let released =
        output_within(Command::new("/bin/bash").arg(root.join("wait.sh")).arg("released"), Duration::from_secs(15));
    assert_eq!(released.stdout, b"RELEASED\n");
    let missing =
        output_within(Command::new("/bin/bash").arg(root.join("wait-short.sh")).arg("never"), Duration::from_secs(15));
    assert!(
        !missing.status.success() && String::from_utf8_lossy(&missing.stderr).contains("deadline exceeded: never"),
        "{missing:?}"
    );
}

/// A headless runtime that records the words of each call and serves the
/// profile configuration from its HOME.
struct Recorded {
    home: PathBuf,
    calls: RefCell<Vec<String>>,
    disabled_at_start: Cell<Option<bool>>,
}

impl Headless for Recorded {
    fn home(&self) -> &Path {
        &self.home
    }

    fn call(&self, words: &[&str], profile: &str, _flags: &[&str]) -> Result<Value, String> {
        assert_eq!(profile, PROFILE);
        let words = words.join(" ");
        self.calls.borrow_mut().push(words.clone());
        let config = self.home.join(".hamn/verify/config.yaml");
        if words == "vm create" {
            fs::create_dir_all(config.parent().unwrap()).unwrap();
            fs::write(&config, "cpu: 4\nmountHome: true\n").unwrap();
        }
        let text = fs::read_to_string(&config).unwrap();
        if words == "vm start" {
            self.disabled_at_start.set(Some(text.contains("mountHome: false")));
            return Ok(json!({"state": "running"}));
        }
        Ok(json!({"state": "stopped", "mountHome": text.contains("mountHome: true")}))
    }
}

fn home_sharing_disabled_before_start() {
    let temporary = TempDir::new("hamn-live-sharing-");
    let runtime =
        Recorded { home: temporary.path().into(), calls: RefCell::default(), disabled_at_start: Cell::new(None) };
    start_isolated(&runtime).unwrap();
    assert_eq!(runtime.disabled_at_start.get(), Some(true));
    assert_eq!(*runtime.calls.borrow(), ["vm create", "vm status", "vm start", "vm status"]);
}

/// Answers only `vm status`, with a running VM.
struct Running(PathBuf);

impl Headless for Running {
    fn home(&self) -> &Path {
        &self.0
    }

    fn call(&self, words: &[&str], _profile: &str, _flags: &[&str]) -> Result<Value, String> {
        if words != ["vm", "status"] {
            return Err(format!("unexpected call {words:?}"));
        }
        Ok(json!({"state": "running"}))
    }
}

fn running_profile_is_not_modified() {
    let temporary = TempDir::new("hamn-live-sharing-");
    let runtime = Running(temporary.path().into());
    let config = runtime.0.join(".hamn/verify/config.yaml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "mountHome: true\n").unwrap();
    let error = start_isolated(&runtime).unwrap_err();
    assert!(error.contains("running validation profile"), "{error}");
    assert_eq!(fs::read_to_string(&config).unwrap(), "mountHome: true\n");
}

fn resume_reuses_the_candidate() {
    let temporary = TempDir::new("hamn-live-prepare-");
    let root = temporary.path();
    fs::create_dir(root.join("home")).unwrap();
    let owner = json!({"workspace": repository(), "profile": PROFILE, "home": root.join("home")});
    fs::write(root.join("ownership.json"), owner.to_string()).unwrap();
    let source = root.join("candidate");
    fs::write(&source, "fixed candidate bytes").unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
    let frozen = root.join("hamn-under-test");
    fs::write(&frozen, "fixed candidate bytes").unwrap();
    fs::set_permissions(&frozen, fs::Permissions::from_mode(0o555)).unwrap();
    let before = fs::metadata(&frozen).unwrap();
    let mut nothing = ();
    finally(
        &mut nothing,
        |_| {
            let (_, runtime) = prepare(&source, Some(&root.join("unused-cache")), Some(root)).unwrap();
            assert_eq!(runtime.binary, frozen);
            assert_eq!(fs::read(&frozen).unwrap(), fs::read(&source).unwrap());
            let after = fs::metadata(&frozen).unwrap();
            assert_eq!(
                (after.ino(), after.mtime(), after.mtime_nsec(), after.mode()),
                (before.ino(), before.mtime(), before.mtime_nsec(), before.mode())
            );
            fs::write(&source, "different candidate bytes").unwrap();
            let error = prepare(&source, Some(&root.join("unused-cache")), Some(root)).err().expect("a refusal");
            assert!(error.contains("different candidate"), "{error}");
            assert_eq!(fs::read(&frozen).unwrap(), b"fixed candidate bytes");
        },
        |_| fs::set_permissions(&frozen, fs::Permissions::from_mode(0o755)).unwrap(),
    );
}

fn cleanup_rejects_replacement() {
    let value = json!({"metadata": {"uid": "owned", "labels": {OWNER: "token"}}});
    assert_identity(&value, "owned", "token").unwrap();
    for (uid, owner) in [("replacement", "token"), ("owned", "foreign")] {
        let changed = json!({"metadata": {"uid": uid, "labels": {OWNER: owner}}});
        assert!(assert_identity(&changed, "owned", "token").is_err(), "{changed}");
    }
    let unlabelled = json!({"metadata": {"uid": "owned"}});
    assert!(assert_identity(&unlabelled, "owned", "token").is_err());
}

fn relation_oracles() {
    let pod = json!({"metadata": {"uid": "pod"}});
    let decoy = json!({"metadata": {"uid": "decoy"}});
    let selected = json!({"metadata": {"uid": "selected"}, "involvedObject": {"uid": "pod"}});
    let stale = json!({"metadata": {"uid": "stale"}, "involvedObject": {"uid": "old-pod"}});
    let check = |pods: &[&Value], events: &[&Value]| {
        assert_relations(&json!({"items": pods}), &json!({"items": events}), &pod, &decoy, &selected, &stale)
    };
    check(&[&pod], &[&selected]).unwrap();
    for (pods, events) in [
        (vec![&pod, &decoy], vec![&selected]),
        (vec![&pod], vec![&selected, &stale]),
        (vec![], vec![&selected]),
        (vec![&pod], vec![]),
    ] {
        assert!(check(&pods, &events).is_err(), "{pods:?} {events:?}");
    }
}

fn reused_pid() {
    let owned =
        Row { pid: 123, parent: 100, group: 123, started: "original".into(), command: "kubectl logs pod".into() };
    assert!(same_process(&owned, Some(&Row { parent: 1, ..owned.clone() })));
    for changed in [
        Row { pid: 124, ..owned.clone() },
        Row { group: 124, ..owned.clone() },
        Row { started: "reused".into(), ..owned.clone() },
        Row { command: "unrelated".into(), ..owned.clone() },
    ] {
        assert!(!same_process(&owned, Some(&changed)), "{changed:?}");
    }
    assert!(!same_process(&owned, None));
}

/// The recorded-peer TUI harness as a [`Driver`].
struct HarnessDriver<'a>(&'a mut Harness);

impl Driver for HarnessDriver<'_> {
    fn text(&self) -> String {
        self.0.text()
    }

    fn wait_for(&mut self, predicate: &dyn Fn(&str) -> bool) {
        self.0.wait(|harness| predicate(&harness.text()));
    }

    fn write(&mut self, keys: &[u8]) {
        self.0.write(keys);
    }
}

fn live_menu_helpers() {
    let mut harness = Harness::new("kubernetes");
    harness.until("old-target-row");
    let row = |name: &str, kind: &str, uid: &str| {
        json!({"apiVersion": "v1", "kind": kind,
            "metadata": {"name": name, "namespace": "review", "uid": uid, "resourceVersion": "1"}})
    };
    let mut deployment = row("web", "Deployment", "deployment-uid");
    deployment["spec"] = json!({"selector": {"matchLabels": {"app": "selected"}}});
    let objects = json!({"probes.example.test": row("read-only", "ReviewProbe", "custom-uid"),
        "deployments": deployment, "pods": row("web-pod", "Pod", "pod-uid"),
        "events": row("selected-event", "Event", "event-uid")});
    fs::write(harness.root.join("objects"), objects.to_string()).unwrap();
    select_peer(&harness.root, "kubectl", "workspace-management-kubectl");
    {
        let terminal = &mut HarnessDriver(&mut harness);
        query(terminal, "probes.example.test", "read-only", Some("review"));
        terminal.send(b"m", Some("Resource actions"));
        assert!(terminal.text().contains("inspect") && !terminal.text().contains("delete"), "{}", terminal.text());
        choose(terminal, "Resource actions", "inspect");
        terminal.until("custom-uid");
        terminal.until("Exit code 0");
        terminal.send(b"\r", None);
        query(terminal, "deployments", "web", Some("review"));
        terminal.send(b"m", Some("related-pods"));
        choose(terminal, "Resource actions", "related-pods");
        terminal.until("> web-pod");
        terminal.wait_for(&|text| !text.contains("[loading]"));
        terminal.send(b"m", Some("related-events"));
        choose(terminal, "Resource actions", "related-events");
        terminal.until("selected-event");
    }
    let commands: Vec<Vec<String>> = harness.calls().into_iter().map(|(_, args)| args).collect();
    let has = |args: &[String], word: &str| args.iter().any(|arg| arg == word);
    assert!(commands.iter().any(|args| has(args, "--selector") && has(args, "app=selected")), "{commands:?}");
    assert!(
        commands.iter().any(|args| has(args, "--field-selector") && has(args, "involvedObject.uid=pod-uid")),
        "{commands:?}"
    );
}

/// The kubectl peer of the management helpers' check: records its call and
/// answers `get KIND` from `$FIXTURE_ROOT/objects`, as YAML with the UID
/// when asked for YAML.
pub fn management_kubectl(_program: &str, args: &[String]) -> ExitCode {
    exec::python_exit(|| {
        let root = PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"));
        crate::suites::tui_native_regressions::record(&root, "kubectl", args);
        let get = args.iter().position(|arg| arg == "get").expect("a get query");
        let kind = &args[get + 1];
        let objects: Value = serde_json::from_str(&fs::read_to_string(root.join("objects")).unwrap()).unwrap();
        let value = objects.get(kind).unwrap_or_else(|| panic!("no fixture object for {kind}"));
        if args.iter().any(|arg| arg == "yaml") {
            println!("uid: {}", value["metadata"]["uid"].as_str().unwrap());
        } else {
            println!("{}", json!({"items": [value]}));
        }
        ExitCode::SUCCESS
    })
}

fn client(root: &Path, authority: &str, certificate: bool) -> Arc<rustls::ClientConfig> {
    let builder = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots(&root.join(format!("{authority}.pem"))));
    let config = if certificate {
        let chain: Vec<CertificateDer<'static>> =
            CertificateDer::pem_file_iter(root.join("client.pem")).unwrap().map(Result::unwrap).collect();
        builder.with_client_auth_cert(chain, PrivateKeyDer::from_pem_file(root.join("client.key")).unwrap()).unwrap()
    } else {
        builder.with_no_client_auth()
    };
    Arc::new(config)
}

/// Sends a request over TLS and reads the reply to a clean TLS end.
fn exchange(port: u16, config: Arc<rustls::ClientConfig>) -> io::Result<Vec<u8>> {
    let socket = TcpStream::connect(("127.0.0.1", port))?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    socket.set_write_timeout(Some(Duration::from_secs(5)))?;
    let session = rustls::ClientConnection::new(config, ServerName::from(IpAddr::from([127, 0, 0, 1])))
        .map_err(io::Error::other)?;
    let mut stream = rustls::StreamOwned::new(session, socket);
    stream.write_all(b"owned-request\n")?;
    stream.flush()?;
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply)?;
    Ok(reply)
}

fn mtls_forwarding() {
    let temporary = TempDir::new("hamn-context-observers-");
    let root = temporary.path();
    let server = certificates(root);
    let target = root.join("engine.sock");
    let listener = UnixListener::bind(&target).unwrap();
    let data = b"Engine response ".repeat(65536);
    let (sender, received) = mpsc::channel();
    let response = data.clone();
    std::thread::spawn(move || {
        // A proxy that never connects fails the check instead of hanging it.
        if super::readable(&[listener.as_raw_fd()], Duration::from_secs(5)).is_empty() {
            return;
        }
        let (mut connection, _) = listener.accept().unwrap();
        let mut request = vec![0u8; 1024];
        let count = connection.read(&mut request).unwrap();
        request.truncate(count);
        connection.write_all(&response).unwrap();
        let _ = sender.send(request);
    });
    let mut proxy = Proxy::new(Some(&target), Some(server));
    finally(
        &mut proxy,
        |proxy| {
            assert!(exchange(proxy.port, client(root, "ca", true)).unwrap() == data, "the reply differs");
            let request = received.recv_timeout(Duration::from_secs(5)).expect("the engine answered");
            assert_eq!(request, b"owned-request\n");
            for config in [client(root, "wrong-ca", true), client(root, "ca", false)] {
                assert!(exchange(proxy.port, config).is_err(), "an unauthenticated exchange succeeded");
            }
            assert_eq!(proxy.forwarded(), 1);
        },
        |proxy| {
            proxy.close();
            let _ = fs::remove_file(&target);
        },
    );
    assert_eq!(proxy.tls_failures(), 2);
}

fn stalled_transport() {
    let temporary = TempDir::new("hamn-context-observers-");
    let root = temporary.path();
    let server = certificates(root);
    let mut proxy = Proxy::new(Some(&root.join("must-not-be-contacted")), Some(server));
    proxy.set_stall(true);
    let (sender, received) = mpsc::channel();
    let (port, config) = (proxy.port, client(root, "ca", true));
    std::thread::spawn(move || {
        let _ = sender.send(exchange(port, config).map_err(|error| error.to_string()));
    });
    finally(
        &mut proxy,
        |proxy| {
            assert!(proxy.entered(Duration::from_secs(5)), "the stalled request never arrived");
            assert!(
                matches!(received.try_recv(), Err(mpsc::TryRecvError::Empty)),
                "the client finished before release"
            );
            assert_eq!(proxy.forwarded(), 0);
            proxy.release();
            let reply = received.recv_timeout(Duration::from_secs(5)).expect("the client finished after release");
            assert_eq!(reply, Ok(Vec::new()));
        },
        Proxy::close,
    );
}

fn witnessed_wrapper() {
    let temporary = TempDir::new("hamn-context-observers-");
    let root = temporary.path();
    let (witness, program) = (root.join("births.jsonl"), root.join("program"));
    wrapper(&program, Path::new("/bin/cat"), &[], &witness);
    let mut child = exec::Session(Command::new(&program).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap());
    // The birth is written before exec; the echoed bytes prove the wrapper
    // reached the real executable.
    child.0.stdin.as_mut().unwrap().write_all(b"observed\n").unwrap();
    assert_eq!(read_line(child.0.stdout.as_mut().unwrap(), Duration::from_secs(5)), "observed\n");
    let recorded = births(&witness);
    assert_eq!(recorded, [birth(child.id() as i32).expect("the running wrapper")]);
    let error = gone(&recorded, Duration::ZERO).unwrap_err();
    assert!(error.contains("survived"), "{error}");
    drop(child.0.stdin.take());
    let status = wait_timeout(&mut child.0, Duration::from_secs(5)).expect("cat exits at end of input");
    assert!(status.success(), "{status:?}");
    gone(&recorded, Duration::from_secs(5)).unwrap();
}

fn deployment_before() -> Value {
    json!({"metadata": {"uid": "selected", "resourceVersion": "before",
        "annotations": {"guard-proof": "changed"}}, "spec": {"replicas": 0,
        "template": {"metadata": {"annotations": {"keep": "preserved"}}}}})
}

fn controller_changes_pass() {
    let before = deployment_before();
    let mut after = before.clone();
    after["metadata"]["resourceVersion"] = json!("opaque-new-version");
    after["metadata"]["annotations"]["deployment.kubernetes.io/revision"] = json!("1");
    after["status"] = json!({"observedGeneration": 1});
    assert_deployment_preserved(&before, &after).unwrap();
}

type Mutation = (&'static str, fn(&mut Value));

fn lost_changes_fail() {
    let before = deployment_before();
    let mutations: [Mutation; 5] = [
        ("replacement", |value| value["metadata"]["uid"] = json!("replacement")),
        ("replicas", |value| value["spec"]["replicas"] = json!(1)),
        ("restart", |value| {
            value["spec"]["template"]["metadata"]["annotations"]["kubectl.kubernetes.io/restartedAt"] = json!("now")
        }),
        ("template", |value| value["spec"]["template"]["metadata"]["annotations"]["keep"] = json!("lost")),
        ("concurrent edit", |value| value["metadata"]["annotations"]["guard-proof"] = json!("lost")),
    ];
    for (name, mutate) in mutations {
        let mut after = before.clone();
        mutate(&mut after);
        assert!(assert_deployment_preserved(&before, &after).is_err(), "{name} was accepted");
    }
}
