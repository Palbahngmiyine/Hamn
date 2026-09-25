//! The real single-binary worker with an isolated HOME: request validation,
//! resource limits, diagnostics, headless envelopes and watch cancellation.
//! Never boots a VM.
//!
//! One flow, as in the script it replaces: each step works on the profile
//! state the previous steps left. The step for a legacy state record (CLI
//! contexts recorded by older releases) is the `legacy_*` function. The
//! script's managed-K3s steps (a `kubernetes:` configuration section,
//! `migration` status and `vm migrate`) are not ported: those features are
//! being removed.
use crate::runner::{self, case};
use crate::support::api_fixtures::{self, Completed, MkdTemp, parse, truthy, utf8};
use crate::support::{hamn, tui::install_fixture};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "core-worker",
        "core worker isolation and protocol",
        vec![case("core_worker_isolation_and_protocol", core_worker_isolation_and_protocol)],
        filters,
    )
}

const GIB: u64 = 1024 * 1024 * 1024;

/// The binary under test and the environment every call receives: the
/// test's own environment with HOME (and later PATH) replaced.
struct Worker {
    binary: PathBuf,
    home: PathBuf,
    env: Vec<(&'static str, OsString)>,
}

impl Worker {
    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(&self.binary);
        command.args(arguments).envs(self.env.iter().map(|(name, value)| (name, value)));
        command
    }

    fn set(&mut self, name: &'static str, value: impl Into<OsString>) {
        self.env.retain(|(existing, _)| *existing != name);
        self.env.push((name, value.into()));
    }

    /// One `__core-worker` request; the worker must exit 0 with a JSON result.
    fn call(&self, words: &str, arguments: Value) -> Value {
        let mut request = json!({"words": words.split_whitespace().collect::<Vec<_>>(), "timeout": 30, "tail": 200});
        for (name, value) in arguments.as_object().expect("arguments are an object") {
            request[name] = value.clone();
        }
        let input = api_fixtures::py_json(&request);
        let completed = api_fixtures::run(&mut self.command(&["__core-worker"]), Some(input.as_bytes()), Duration::from_secs(10));
        assert!(completed.success(), "{completed:?}");
        completed.json()
    }

    fn run(&self, arguments: &[&str]) -> Completed {
        api_fixtures::run(&mut self.command(arguments), None, Duration::from_secs(10))
    }

    fn hamn_dir(&self) -> PathBuf {
        self.home.join(".hamn")
    }
}

fn has(value: &Value, key: &str) -> bool {
    value.get(key).is_some()
}

fn core_worker_isolation_and_protocol() {
    let directory = MkdTemp::new("hamn-worker-");
    let home = directory.path().to_path_buf();
    let mut worker = Worker { binary: hamn(), home: home.clone(), env: vec![("HOME", home.clone().into())] };

    assert_eq!(worker.call("vm list", json!({})), json!({"Ok": []}));
    assert!(!worker.hamn_dir().exists());
    assert!(has(&worker.call("vm create", json!({"profile": "test"})), "Err"));
    assert!(!worker.hamn_dir().exists());
    let created = worker.call("vm create", json!({"profile": "test", "yes": true, "cpu": 2, "memory": 2}));
    assert_eq!(created["Ok"]["mountHome"], json!(true), "{created}");
    assert_eq!(created["Ok"]["homeReadOnly"], json!(false), "{created}");
    assert_eq!(created["Ok"]["mountInotify"], json!(false), "{created}");
    assert_eq!(created["Ok"]["rosetta"], json!(false), "{created}");
    assert_eq!(created["Ok"]["fileEvents"], json!("disabled"), "{created}");
    assert_eq!(created["Ok"]["cpus"], json!(2), "{created}");
    assert_eq!(created["Ok"]["memoryMiB"], json!(2048), "{created}");
    assert_eq!(worker.call("vm create", json!({"profile": "test", "yes": true, "cpu": 10}))["Err"]["code"], json!("conflict"));
    assert_eq!(worker.call("vm status", json!({"profile": "test"}))["Ok"]["cpus"], json!(2));

    let disk = home.join(".hamn/test/disk.img");
    File::create(&disk).unwrap().set_len(60 * GIB).unwrap();
    let config = home.join(".hamn/test/config.yaml");
    let config_before_shrink = fs::read(&config).unwrap();
    let rejected = worker.call("vm configure", json!({"profile": "test", "yes": true, "cpu": 3, "disk": 1}));
    assert!(
        has(&rejected, "Err") && rejected["Err"]["message"].as_str().is_some_and(|message| message.contains("cannot shrink")),
        "{rejected}"
    );
    assert_eq!(fs::read(&config).unwrap(), config_before_shrink);
    assert_eq!(fs::metadata(&disk).unwrap().len(), 60 * GIB);
    assert_eq!(worker.call("vm configure", json!({"profile": "test", "yes": true, "disk": 60}))["Ok"]["diskGiB"], json!(60));
    assert_eq!(worker.call("vm configure", json!({"profile": "test", "yes": true, "disk": 61}))["Ok"]["diskGiB"], json!(61));
    // Configuration records growth for the next start; it cannot shrink storage.
    assert_eq!(fs::metadata(&disk).unwrap().len(), 60 * GIB);
    assert!(has(&worker.call("vm stop", json!({"profile": "missing", "yes": true})), "Err"));
    assert!(!home.join(".hamn/missing").exists());
    assert_eq!(worker.call("vm status", json!({"profile": "test"}))["Ok"]["state"], json!("stopped"));
    assert!(has(&worker.call("vm configure", json!({"profile": "../escape", "yes": true})), "Err"));
    assert!(has(&worker.call("vm status", json!({"profile": "missing"})), "Err"));
    assert!(!home.join(".hamn").join("missing").exists());
    assert_eq!(worker.call("vm list", json!({}))["Ok"].as_array().map(Vec::len), Some(1));

    let archive = home.join("diagnostics.tar");
    let path = utf8(&archive);
    assert!(has(&worker.call("vm diagnostics", json!({"profile": "test", "path": path})), "Err"));
    assert!(!archive.exists());
    let diagnostic = worker.call("vm diagnostics", json!({"profile": "test", "path": path, "yes": true}));
    assert!(
        truthy(diagnostic["Ok"].get("redacted")) && diagnostic["Ok"]["bytes"].as_f64().is_some_and(|bytes| bytes > 0.0),
        "{diagnostic}"
    );
    let members = archive_members(&archive);
    assert!(members.len() >= 3, "{members:?}");
    assert!(!members.iter().any(|member| member.contains("ed25519")), "{members:?}");
    assert!(has(&worker.call("vm diagnostics", json!({"profile": "test", "path": path, "yes": true})), "Err"));
    assert!(has(&worker.call("system update", json!({"yes": true})), "Err"));
    assert!(has(&worker.call("system uninstall", json!({})), "Err"));

    legacy_context_records_do_not_run_external_clis(&mut worker);

    let result = worker.run(&["--headless", "vm", "status", "--profile", "test"]);
    assert!(result.success(), "{result:?}");
    let value = result.json();
    assert!(value["ok"] == json!(true) && value["data"]["cpus"] == json!(2), "{value}");
    let result = worker.run(&["--headless", "capabilities"]);
    assert!(result.success(), "{result:?}");
    assert_eq!(result.json()["data"]["formats"], json!(["json", "ndjson"]));
    let result = worker.run(&["--headless", "vm", "configure", "--profile", "test", "--yes", "--watch"]);
    assert!(!result.success(), "{result:?}");
    assert_eq!(result.json()["error"]["code"], json!("invalidRequest"));

    watch_is_cancelled_by_sigterm(&worker);

    assert!(has(&worker.call("vm create", json!({"profile": "deleted", "yes": true})), "Ok"));
    assert!(has(&worker.call("vm delete", json!({"profile": "deleted", "yes": true})), "Ok"));
    let rows = worker.call("vm list", json!({}));
    let rows = rows["Ok"].as_array().unwrap_or_else(|| panic!("vm list rows: {rows}"));
    assert!(rows.iter().all(|row| *row.get("name").expect("row name") != "deleted"), "{rows:?}");
    assert!(home.join(".hamn/deleted/config.yaml").exists());
    let result = worker.run(&[]);
    assert!(!result.success() && !result.stdout.contains('\x1b'), "{result:?}");
    let result = worker.run(&["vm", "start", "--profile", "test"]);
    assert_eq!(result.json()["error"]["code"], json!("invalidRequest"), "{result:?}");
}

/// Legacy context records must not cause either external CLI to run at stop.
/// `docker` and `kubectl` are fixtures that record a run and exit 99; the
/// PATH change stays in effect for the rest of the flow.
fn legacy_context_records_do_not_run_external_clis(worker: &mut Worker) {
    let fake_bin = worker.home.join("bin");
    fs::create_dir(&fake_bin).unwrap();
    let invoked = worker.home.join("external-cli-invoked");
    for tool in ["docker", "kubectl"] {
        install_fixture(&fake_bin, tool);
    }
    worker.set("PATH", format!("{}:/usr/bin:/bin", fake_bin.display()));
    worker.set("FIXTURE_ROOT", worker.home.clone());
    worker.set("HAMN_DEV_FIXTURE", "core-worker-external-cli");
    let state_file = worker.home.join(".hamn/test/state.json");
    let state = json!({"state": "stopped", "prev_docker_context": "outside", "prev_kube_context": "outside"});
    fs::write(&state_file, api_fixtures::py_json(&state)).unwrap();
    assert!(has(&worker.call("vm stop", json!({"profile": "test", "yes": true})), "Ok"));
    assert!(!invoked.exists());
    let state = parse(&fs::read_to_string(&state_file).unwrap());
    assert!(!has(&state, "prev_docker_context"), "{state}");
    assert!(!has(&state, "prev_kube_context"), "{state}");
}

/// `vm list --watch` emits a snapshot, and SIGTERM ends it with a
/// `cancelled` result for the same request and a later sequence number.
fn watch_is_cancelled_by_sigterm(worker: &Worker) {
    let mut command = worker.command(&["--headless", "vm", "list", "--watch"]);
    command.stdin(Stdio::inherit()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut watch = Reaped(command.spawn().expect("spawn vm list --watch"));
    let child = &mut watch.0;
    let (sender, lines) = mpsc::channel();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    std::thread::spawn(move || {
        for line in stdout.lines() {
            if sender.send(line.expect("watch output is UTF-8")).is_err() {
                return;
            }
        }
    });
    let mut stderr = child.stderr.take().unwrap();
    let errors = std::thread::spawn(move || {
        let mut data = Vec::new();
        stderr.read_to_end(&mut data).expect("read watch stderr");
        api_fixtures::text(data)
    });
    // The script blocked on this line without a bound; 10 seconds is the
    // bound of every other step here.
    let snapshot = parse(&lines.recv_timeout(Duration::from_secs(10)).expect("a snapshot line before end-of-file"));
    assert!(snapshot["type"] == "snapshot" && truthy(snapshot.get("ok")), "{snapshot}");
    crate::support::pty::kill(child.id(), libc::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut events = Vec::new();
    loop {
        match lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) => events.push(parse(&line)),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("watch output did not end within 10 seconds"),
        }
    }
    let status = api_fixtures::wait_until(child, deadline).expect("watch exits within 10 seconds");
    let error = errors.join().unwrap();
    let last = events.last().unwrap_or_else(|| panic!("no event after the snapshot; stderr: {error}"));
    assert!(!status.success() && last["error"]["code"] == "cancelled", "{events:?} {error}");
    assert_eq!(last.get("requestId").expect("result requestId"), snapshot.get("requestId").expect("snapshot requestId"));
    assert!(
        last["sequence"].as_u64().zip(snapshot["sequence"].as_u64()).is_some_and(|(last, first)| last > first),
        "{last} {snapshot}"
    );
}

/// Kills and reaps a child that is still running when dropped, like the
/// script's `finally` block.
struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// The member names of a tar archive, listed by the system tar (compression
/// detected as Python's `tarfile.open` does).
fn archive_members(archive: &Path) -> Vec<String> {
    let output = Command::new("/usr/bin/tar").arg("-tf").arg(archive).output().expect("run tar");
    assert!(output.status.success(), "tar -tf: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout).expect("UTF-8 member names").lines().map(str::to_owned).collect()
}

/// The `docker` and `kubectl` peers: record that an external CLI ran, then fail.
pub fn external_cli_fixture(_program: &str, _args: &[String]) -> ExitCode {
    let root = PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"));
    OpenOptions::new().create(true).append(true).open(root.join("external-cli-invoked")).unwrap();
    ExitCode::from(99)
}
