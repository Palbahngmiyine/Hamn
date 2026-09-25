//! The real headless, worker and updater boundary of `hamn --headless system
//! upgrade`, observed through a controlled HTTPS transport: the first host
//! download blocks in the server until the test has seen download progress,
//! so progress must be written before completion. Native curl and TLS run
//! unchanged; no public network or VM is used. The loopback server speaks
//! only TLS 1.2 or later (rustls safe defaults), so every recorded request
//! was made over TLS.
use crate::runner::{self, case};
use crate::support::http::{self, Options, Response, Server};
use crate::support::pty::{self, Pty};
use crate::support::tmp::TempDir;
use crate::support::upgrade::{self, Artifact, Group, Output, write_json, write_executable};
use serde_json::{Value, json};
use std::fs;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub fn main(filters: &[String]) -> ExitCode {
    // The two modes share no state; CI runs them as separate gates.
    runner::run(
        "update-ux",
        "progress before completion, JSON, version summary, schema rejection, no-op identity and state preservation",
        vec![case("redirected", || check(false)), case("pty", || check(true))],
        filters,
    )
}

/// Holds the first host download until the test releases it.
#[derive(Default)]
struct Gate {
    /// (a download reached the gate, the gate was released)
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

impl Gate {
    /// Called by the server for a host download: blocks the first one (for
    /// at most 20 s) until `release`, like the former socket handshake.
    fn hold(&self) {
        let mut state = self.state.lock().unwrap();
        if state.1 {
            return;
        }
        state.0 = true;
        self.changed.notify_all();
        let (state, timeout) = self.changed.wait_timeout_while(state, Duration::from_secs(20), |state| !state.1).unwrap();
        assert!(state.1 && !timeout.timed_out(), "the test never released the blocked download");
    }

    fn await_reached(&self, timeout: Duration) {
        let state = self.state.lock().unwrap();
        let (state, _) = self.changed.wait_timeout_while(state, timeout, |state| !state.0).unwrap();
        assert!(state.0, "the host download did not reach the controlled server within {timeout:?}");
    }

    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
}

/// Releases the gate when dropped, so a failed case never leaves the
/// server's connection thread blocked.
struct Released<'a>(&'a Gate);

impl Drop for Released<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// The controlled HTTPS endpoint serving `root/host.tar.gz` and
/// `root/guest.img` as they are at request time.
struct Transport {
    base: String,
    cert: PathBuf,
    requests: Arc<Mutex<Vec<String>>>,
    gate: Arc<Gate>,
    _server: Server,
}

impl Transport {
    fn start(root: &Path) -> Self {
        let (cert, key) = http::certificate(root);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new(Gate::default());
        let (log, held, files) = (Arc::clone(&requests), Arc::clone(&gate), root.to_path_buf());
        let server = Server::tcp(Options { tls: Some((cert.clone(), key)), ..Options::default() }, move |request| {
            let name = match request.path() {
                "/host.tar.gz" => "host.tar.gz",
                "/guest.img" => "guest.img",
                _ => return Response::new(404, "").into(),
            };
            log.lock().unwrap().push(request.path().to_owned());
            if name == "host.tar.gz" {
                held.hold();
            }
            Response::new(200, fs::read(files.join(name)).unwrap()).into()
        });
        Self { base: server.url("https"), cert, requests, gate, _server: server }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn artifact(&self, path: &Path) -> Artifact {
        let name = path.file_name().unwrap().to_str().unwrap();
        Artifact { url: format!("{}/{name}", self.base), ..Artifact::local(path) }
    }
}

/// One owned managed install, its release inputs and its environment.
struct Fixture {
    work: PathBuf,
    home: PathBuf,
    bindir: PathBuf,
    datadir: PathBuf,
    artifact: PathBuf,
    archive: PathBuf,
    guest: PathBuf,
    manifest_path: PathBuf,
    transport_dir: PathBuf,
    transport: Transport,
}

impl Fixture {
    fn env(&self, command: &mut Command) {
        let cert = &self.transport.cert;
        let path = format!("{}:{}", self.transport_dir.display(), std::env::var("PATH").unwrap_or_default());
        command
            .env("CURL_CA_BUNDLE", cert)
            .env("SSL_CERT_FILE", cert)
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .env("HOME", &self.home)
            .env("PATH", path)
            // The updater ignores the caller PATH; faults use this seam.
            .env("HAMN_TEST_UPDATE_TOOL_DIR", &self.transport_dir)
            .env("HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS", "1");
    }

    fn upgrade_command(&self) -> Command {
        let mut command = Command::new(self.bindir.join("hamn"));
        command.args(["--headless", "system", "upgrade", "--yes", "--manifest"]).arg(&self.manifest_path);
        self.env(&mut command);
        command
    }

    fn upgrade(&self, timeout: Duration) -> Output {
        upgrade::run(self.upgrade_command().stdin(Stdio::null()), timeout)
    }

    fn active(&self) -> PathBuf {
        fs::read_link(self.bindir.join("hamn")).unwrap()
    }

    fn selection(&self) -> PathBuf {
        self.home.join(".hamn/cache/guest-image.json")
    }

    fn journal(&self) -> PathBuf {
        self.home.join(".hamn/cache/.hamn-update-transaction")
    }

    /// Repacks the release directory and returns a manifest naming it.
    fn repack(&self, manifest: &Value) -> Value {
        upgrade::pack_release(&self.artifact, &self.archive);
        let host = self.transport.artifact(&self.archive);
        let mut manifest = manifest.clone();
        manifest["artifacts"]["host"]["sha256"] = host.sha256.into();
        manifest["artifacts"]["host"]["size"] = host.size.into();
        manifest
    }

    /// Asserts the link, image selection and journal are as `active` and
    /// `saved` describe, with no pending transaction.
    fn assert_unchanged(&self, active: &Path, saved: &[u8], what: &str) {
        assert_eq!(self.active(), active, "{what}");
        assert_eq!(fs::read(self.selection()).unwrap(), saved, "{what}");
        assert!(!self.journal().exists(), "{what}");
    }

    /// This invocation starts in the interrupted new generation. Recovery
    /// restores its predecessor, so continuing the stale frontend would
    /// bypass the version and identity check under the lock.
    fn recover_changed_generation(&self, active: &Path, saved: &[u8]) {
        let recovered = self.upgrade(Duration::from_secs(30));
        assert_ne!(recovered.returncode, 0, "{}", recovered.stdout());
        assert!(
            recovered.stderr().contains("recovered the previous binary and guest image selection"),
            "{}",
            recovered.stderr()
        );
        let message = error_message(&recovered);
        assert!(message.contains("managed generation changed while waiting or recovering"), "{message}");
        self.assert_unchanged(active, saved, "recovered");
    }
}

/// The single failure reason in the headless JSON envelope.
fn error_message(output: &Output) -> String {
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| panic!("{error}: {output:?}"));
    assert_eq!(value["ok"], false, "{}", output.stdout());
    value["error"]["message"].as_str().expect("error message").to_owned()
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle.as_bytes())
}

fn check(terminal: bool) {
    let directory = TempDir::new("hamn-update-ux-");
    let work = fs::canonicalize(directory.path()).unwrap();
    let hamn = fs::canonicalize(crate::support::hamn()).unwrap();
    let version = upgrade::run(Command::new(&hamn).arg("--version"), Duration::from_secs(10)).stdout();
    let version = version.split_whitespace().nth(1).unwrap_or_else(|| panic!("--version: {version:?}")).to_owned();
    let artifact = work.join("release");
    fs::create_dir_all(artifact.join("bin")).unwrap();
    fs::copy(&hamn, artifact.join("bin/hamn")).unwrap();
    upgrade::copy_release_support(&artifact);
    fs::write(artifact.join("packaging/release/update-manifest-url"), "https://example.test/manifest\n").unwrap();
    let fixture = Fixture {
        home: work.join("home"),
        bindir: work.join("bin"),
        datadir: work.join("src"),
        archive: work.join("host.tar.gz"),
        guest: work.join("guest.img"),
        manifest_path: work.join("manifest.json"),
        transport_dir: work.join("transport"),
        transport: Transport::start(&work),
        artifact,
        work,
    };
    fs::create_dir(&fixture.home).unwrap();
    fs::create_dir(&fixture.transport_dir).unwrap();
    // Resolving support from the checkout's scripts would select the shared
    // build/hamn even when HAMN names a frozen binary. Own every installer
    // dependency.
    let installed = upgrade::run(
        Command::new("bash")
            .arg(fixture.artifact.join("scripts/install-host.sh"))
            .arg(fixture.artifact.join("bin/hamn"))
            .arg(&fixture.bindir)
            .arg(&fixture.datadir)
            .env("HOME", &fixture.home),
        Duration::from_secs(60),
    );
    assert_eq!(installed.returncode, 0, "{}", installed.stderr());
    let original = fixture.active();
    upgrade::pack_release(&fixture.artifact, &fixture.archive);
    fs::write(&fixture.guest, "controlled guest fixture\n").unwrap();
    let manifest = upgrade::manifest(
        &format!("v{version}"),
        &fixture.transport.artifact(&fixture.archive),
        &fixture.transport.artifact(&fixture.guest),
    );
    write_json(&fixture.manifest_path, &manifest);

    let (received, first) = first_upgrade(&fixture, terminal);
    assert_eq!(first.returncode, 0, "{}\n{first:?}", String::from_utf8_lossy(&received));
    assert_eq!(serde_json::from_slice::<Value>(&first.stdout).unwrap()["data"]["completed"], true, "{}", first.stdout());
    let text = String::from_utf8_lossy(&received).into_owned();
    assert!(text.contains(&format!("Reinstalled Hamn {version}.")), "{text}");
    assert!(text.contains("Existing VMs were not restarted"), "{text}");
    // One heading and one result line; no internal jargon or repeats.
    for jargon in ["hamn update:", "hamn upgrade:", "verified cache", "atomically", "update completed", ".hamn-generations/", "file://"] {
        assert!(!text.contains(jargon), "{jargon}: {text}");
    }
    let mut requests = fixture.transport.requests();
    requests.sort();
    assert_eq!(requests, ["/guest.img", "/host.tar.gz"], "each payload is fetched once, over TLS");
    let mut active = fixture.active();
    assert_ne!(active, original);
    let selection = fixture.selection();
    let saved = fs::read(&selection).unwrap();
    let selected: Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(selected["sha256"], upgrade::file_digest(&fixture.guest));
    let upgrade_within = |seconds| fixture.upgrade(Duration::from_secs(seconds));

    // The identical release must not fetch either payload or rewrite state.
    let before_calls = fixture.transport.requests();
    let before_mtime = fs::metadata(&selection).unwrap().modified().unwrap();
    let repeated = upgrade_within(30);
    assert_eq!(repeated.returncode, 0, "{repeated:?}");
    assert_eq!(serde_json::from_slice::<Value>(&repeated.stdout).unwrap()["data"]["completed"], true);
    // A no-op is exactly two human lines.
    assert_eq!(repeated.stderr(), format!("Checking for updates...\nHamn {version} is up to date.\n"));
    assert_eq!(fixture.transport.requests(), before_calls);
    assert_eq!(fs::metadata(&selection).unwrap().modified().unwrap(), before_mtime);
    fixture.assert_unchanged(&active, &saved, "no-op");

    // Version equality is insufficient. A missing or corrupt receipt, changed
    // updater files or image selection must take the verified install path.
    for damage in ["missing receipt", "malformed receipt", "source changed", "selection changed"] {
        let generation = fs::canonicalize(fixture.bindir.join(&active)).unwrap();
        let generation = generation.parent().and_then(Path::parent).unwrap();
        let receipt = generation.join(".hamn-release.json");
        match damage {
            "missing receipt" => fs::remove_file(&receipt).unwrap(),
            "malformed receipt" => fs::write(&receipt, "{").unwrap(),
            "source changed" => fs::write(generation.join("share/hamn/src/packaging/changed.txt"), "changed").unwrap(),
            _ => fs::write(&selection, "{}").unwrap(),
        }
        let calls = fixture.transport.requests().len();
        let result = upgrade_within(30);
        assert_eq!(result.returncode, 0, "{damage}: {result:?}");
        assert!(!result.stderr().contains("is up to date"), "{damage}");
        // Both payloads are cached. Only damaged host integrity requires a
        // generation reinstall; guest selection repair preserves it.
        assert_eq!(fixture.transport.requests().len(), calls, "{damage}");
        assert_eq!(fixture.active() == active, damage == "selection changed", "{damage}");
        active = fixture.active();
        assert_eq!(fs::read(&selection).unwrap(), saved, "{damage}");
    }

    // A selected image with corrupted bytes cannot authorize a no-op.
    let cached_guest = fixture.home.join(".hamn/cache").join(selected["file"].as_str().unwrap());
    fs::write(&cached_guest, "corrupted").unwrap();
    let result = upgrade_within(30);
    assert_eq!(result.returncode, 0, "{result:?}");
    assert_eq!(serde_json::from_slice::<Value>(&result.stdout).unwrap()["data"]["status"], "repaired", "{}", result.stderr());
    assert!(!result.stderr().contains("is up to date"));
    assert!(result.stderr().contains(&format!("Repaired the Hamn {version} guest image.")), "{}", result.stderr());
    fixture.assert_unchanged(&active, &saved, "guest repair");
    fs::copy(&fixture.guest, &cached_guest).unwrap();

    // A new archive with the same version must still be installed.
    fs::write(fixture.artifact.join("packaging/same-version-change.txt"), "new artifact identity").unwrap();
    let manifest = fixture.repack(&manifest);
    write_json(&fixture.manifest_path, &manifest);
    let result = upgrade_within(30);
    assert!(result.returncode == 0 && result.stderr().contains("Reinstalled Hamn "), "{result:?}");
    assert_ne!(fixture.active(), active);
    active = fixture.active();

    // Strict metadata rejection: retired schema v2 fields and versions,
    // and unknown fields, whatever their values.
    for (key, value) in [
        ("repository", json!("example/hamn")),
        ("repository", json!(7)),
        ("repository", json!("bad/name/extra")),
        ("unexpected", json!(true)),
        ("schemaVersion", json!(2)),
    ] {
        let mut bad = manifest.clone();
        bad[key] = value.clone();
        write_json(&fixture.manifest_path, &bad);
        let result = upgrade_within(15);
        assert_ne!(result.returncode, 0, "{key}={value}");
        let message = error_message(&result);
        assert!(message.contains("is not usable by this Hamn"), "{message}");
        assert!(message.contains("https://github.com/Palbahngmiyine/Hamn#install"), "{message}");
        // The reason is reported once (JSON), not repeated on stderr.
        let stderr = result.stderr();
        assert!(!stderr.contains(&message) && !stderr.contains("update failed"), "{stderr}");
        for retry in ["retry with hamn --headless system update --yes", "retry with hamn --headless system upgrade --yes"] {
            assert!(!stderr.contains(retry), "{stderr}");
        }
        fixture.assert_unchanged(&active, &saved, key);
    }
    for failure in ["version", "checksum"] {
        let mut bad = manifest.clone();
        if failure == "version" {
            bad["version"] = "v999.0.0".into();
        } else {
            bad["artifacts"]["guestImage"]["sha256"] = "0".repeat(64).into();
        }
        write_json(&fixture.manifest_path, &bad);
        let result = upgrade_within(15);
        assert_ne!(result.returncode, 0, "{failure}");
        let message = error_message(&result);
        let expected = if failure == "version" {
            "host binary version does not match"
        } else {
            "could not obtain the guest image: artifact size or SHA-256 mismatch"
        };
        assert!(message.contains(expected), "{message}");
        assert!(!result.stderr().contains("Updated Hamn") && !result.stderr().contains("Reinstalled Hamn"), "{}", result.stderr());
        assert_eq!(fixture.active(), active, "{failure}");
        assert_eq!(fs::read(&selection).unwrap(), saved, "{failure}");
    }

    // Reinstalling through bootstrap must preserve the managed binary,
    // including SIGKILL recovery and a later failed installation attempt.
    if !terminal {
        fs::write(fixture.artifact.join("packaging/same-version-change.txt"), "next candidate").unwrap();
        write_json(&fixture.manifest_path, &fixture.repack(&manifest));
        for termination in [libc::SIGTERM, libc::SIGKILL] {
            interrupted_bootstrap(&fixture, termination, &active, &saved);
        }
    }

    // A logging failure must not bypass rollback after an installer failure.
    let installer = fixture.artifact.join("scripts/install-host.sh");
    let installer_source = fs::read_to_string(&installer).unwrap();
    fs::write(&installer, "#!/bin/bash\necho installer-failed >&2\nexit 77\n").unwrap();
    write_json(&fixture.manifest_path, &fixture.repack(&manifest));
    write_executable(
        &fixture.transport_dir.join("cat"),
        "#!/bin/bash\ncase \"$1\" in */host-install.log) exit 73;; esac\nexec /bin/cat \"$@\"\n",
    );
    let result = upgrade_within(15);
    assert_ne!(result.returncode, 0);
    let message = error_message(&result);
    assert!(message.contains("host install failed; prior binary and guest image selection were restored"), "{result:?}");
    fixture.assert_unchanged(&active, &saved, "installer failure");
    write_json(&fixture.manifest_path, &manifest);
    let repeated = upgrade_within(30);
    assert!(repeated.returncode == 0 && repeated.stderr().contains("is up to date"), "{repeated:?}");
    fixture.assert_unchanged(&active, &saved, "after installer failure");

    // Receipt publication is part of the transaction. Inject an existing
    // receipt and a rollback rename failure; a retry must recover the old
    // generation and its valid receipt before considering a no-op.
    if !terminal {
        let move_tool = fixture.transport_dir.join("mv");
        fs::write(
            &installer,
            format!(
                "{installer_source}\ninstalled=$(readlink \"$BINDIR/hamn\")\n: >\"${{installed%/bin/hamn}}/.hamn-release.json\"\n"
            ),
        )
        .unwrap();
        write_json(&fixture.manifest_path, &fixture.repack(&manifest));
        write_executable(
            &move_tool,
            "#!/bin/bash\nfor arg in \"$@\"; do\ncase \"$arg\" in */.hamn-update-rollback.*/hamn) exit 74;; esac\ndone\nexec /bin/mv \"$@\"\n",
        );
        let result = upgrade_within(30);
        assert_ne!(result.returncode, 0, "{}", result.stderr());
        let message = error_message(&result);
        assert!(message.contains("release receipt failed and recovery could not be applied"), "{message}");
        assert!(!message.contains("were restored") && !result.stderr().contains("were restored"), "{result:?}");
        assert_ne!(fixture.active(), active);
        assert_eq!(fs::read(&selection).unwrap(), saved);
        assert!(fixture.journal().is_dir());
        fs::remove_file(&move_tool).unwrap();
        write_json(&fixture.manifest_path, &manifest);
        fixture.recover_changed_generation(&active, &saved);
        let recovered = upgrade_within(30);
        assert_eq!(recovered.returncode, 0, "{recovered:?}");
        assert!(recovered.stderr().contains("is up to date"), "{recovered:?}");
        fixture.assert_unchanged(&active, &saved, "receipt failure recovered");

        // If both completed and recovered journal retirement fail, the
        // remaining recovery instruction preserves the original manifest.
        fs::write(&installer, &installer_source).unwrap();
        write_json(&fixture.manifest_path, &fixture.repack(&manifest));
        write_executable(
            &move_tool,
            "#!/bin/bash\nfor arg in \"$@\"; do\n\
             case \"$arg\" in */.hamn-update-completed.*|*/.hamn-update-recovered.*) exit 75;; esac\n\
             done\nexec /bin/mv \"$@\"\n",
        );
        let result = upgrade_within(30);
        assert_ne!(result.returncode, 0, "{}", result.stderr());
        let message = error_message(&result);
        assert!(
            message.contains(
                "could not clear its recovery journal; retry the same command with all original options (including --manifest)"
            ),
            "{message}"
        );
        let all = [result.stdout.as_slice(), result.stderr.as_slice()].concat();
        for retry in ["run hamn --headless system update --yes again", "run hamn --headless system upgrade --yes again"] {
            assert!(!contains(&all, retry), "{result:?}");
        }
        assert_eq!(fixture.active(), active);
        assert_eq!(fs::read(&selection).unwrap(), saved);
        assert!(fixture.journal().is_dir());
        fs::remove_file(&move_tool).unwrap();
        write_json(&fixture.manifest_path, &manifest);
        let recovered = upgrade_within(30);
        assert!(recovered.returncode == 0 && recovered.stderr().contains("is up to date"), "{recovered:?}");
        fixture.assert_unchanged(&active, &saved, "journal retirement recovered");
    }
}

/// Starts the first upgrade with stderr on a PTY (`terminal`) or a pipe,
/// waits until the controlled server holds the host download, requires
/// download progress while the process is still blocked, then releases the
/// download. Returns everything written to stderr and the result.
fn first_upgrade(fixture: &Fixture, terminal: bool) -> (Vec<u8>, Output) {
    // Keep the slave open until the queued output is drained: macOS may
    // discard unread PTY bytes when the final slave descriptor closes.
    let pty = terminal.then(|| Pty::open(24, 80));
    let stderr = match &pty {
        Some(pty) => Stdio::from(pty.slave.try_clone().unwrap()),
        None => Stdio::piped(),
    };
    let _released = Released(&fixture.transport.gate);
    let mut child = Group::spawn_with_stderr(&mut fixture.upgrade_command(), stderr);
    let reader = match &pty {
        Some(pty) => pty.master.as_raw_fd(),
        None => child.stderr_fd(),
    };
    fixture.transport.gate.await_reached(Duration::from_secs(15));
    let mut received = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !contains(&received, "Downloading Hamn ") {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!pty::readable(&[reader], remaining).is_empty(), "progress was buffered until completion");
        let chunk = pty::read_some(reader);
        assert!(!chunk.is_empty(), "stderr ended before download progress");
        received.extend(chunk);
    }
    assert!(child.running(), "the download fixture must still be blocked");
    fixture.transport.gate.release();
    let output = child.finish(Duration::from_secs(30));
    if let Some(pty) = &pty {
        let master = pty.master.as_raw_fd();
        while !pty::readable(&[master], Duration::ZERO).is_empty() {
            let chunk = pty::read_some(master);
            if chunk.is_empty() {
                break;
            }
            received.extend(chunk);
        }
    } else {
        received.extend_from_slice(&output.stderr);
    }
    (received, output)
}

/// Interrupts a bootstrap reinstall right after the new host generation was
/// installed. SIGTERM rolls back in the updater's trap; SIGKILL leaves the
/// journal, which the next managed command recovers.
fn interrupted_bootstrap(fixture: &Fixture, termination: i32, active: &Path, saved: &[u8]) {
    let (ready, ready_fd) = upgrade::ready_fifo(&fixture.work, &format!("ready-{termination}"));
    let release = fixture.work.join(format!("release-{termination}"));
    upgrade::mkfifo(&release);
    let mut command = Command::new("bash");
    command
        .arg(fixture.artifact.join("scripts/update-host.sh"))
        .arg("--bootstrap")
        .arg("--bindir")
        .arg(&fixture.bindir)
        .arg("--datadir")
        .arg(&fixture.datadir)
        .arg("--manifest")
        .arg(&fixture.manifest_path)
        .env("HAMN_TEST_UPDATE_AFTER_HOST_INSTALL_READY_FIFO", &ready)
        .env("HAMN_TEST_UPDATE_AFTER_HOST_INSTALL_RELEASE_FIFO", &release);
    fixture.env(&mut command);
    let child = Group::spawn(command.stdin(Stdio::null()));
    if pty::readable(&[ready_fd.as_raw_fd()], Duration::from_secs(30)).is_empty() {
        panic!("bootstrap did not reach host cutover: {:?}", child.finish(Duration::from_secs(10)));
    }
    assert_eq!(pty::read_some(ready_fd.as_raw_fd()), b"ready\n");
    assert_ne!(fixture.active(), active);
    child.signal(termination);
    let output = child.finish(Duration::from_secs(15));
    if termination == libc::SIGTERM {
        assert_eq!(output.returncode, 143, "{}", output.stderr());
        assert_eq!(fixture.active(), active);
        assert!(!fixture.journal().exists());
    } else {
        assert_eq!(output.returncode, -libc::SIGKILL, "{}", output.stderr());
        assert!(fixture.journal().exists());
        let old_target = fs::read_to_string(fixture.journal().join("old-target")).unwrap();
        assert_eq!(Path::new(old_target.trim()), active);
        fixture.recover_changed_generation(active, saved);
    }
    assert_eq!(fs::read(fixture.selection()).unwrap(), saved);
}
