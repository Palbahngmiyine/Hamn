//! Bounded generated upgrade transactions (seed 20260924 + case) against the Hamn
//! under test. They complement the fixed fault and signal matrices and the
//! native property tests in control/install_support; they are not a proof
//! over all inputs or a guest boot test. All installs, network observations,
//! processes and files belong to a temporary fixture; no build output changes.
use crate::runner::{self, case};
use crate::support::tmp::TempDir;
use crate::support::upgrade::{
    self, Artifact, Group, await_ready, copy_release_support, digest, mkfifo, pack_release, ready_fifo, version_wrapper,
    write_executable, write_private_json,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "upgrade-properties",
        "generated profile trees survive no-op, repair and interrupted selection-only transactions",
        vec![
            case("network_tripwire_observes_absolute_curl_connections", network_tripwire_observes_absolute_curl_connections),
            case("generated_profile_trees_survive_noop_repair_and_recovery/PREPARED", || {
                generated_profile_trees_survive_noop_repair_and_recovery(0, "PREPARED")
            }),
            case("generated_profile_trees_survive_noop_repair_and_recovery/AFTER_GUEST_SELECTION", || {
                generated_profile_trees_survive_noop_repair_and_recovery(1, "AFTER_GUEST_SELECTION")
            }),
        ],
        filters,
    )
}

const SEED: u64 = 20260924;

/// A deterministic LCG, so every case is reproducible; each case records
/// its seed and index.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    /// A value in `low..high`.
    fn range(&mut self, low: u64, high: u64) -> u64 {
        low + self.next() % (high - low)
    }

    fn bytes(&mut self, count: u64) -> Vec<u8> {
        (0..count).map(|_| self.next() as u8).collect()
    }
}

/// Observes any TCP connection to a loopback port, independently of curl's
/// path, by creating `requests` when a connection arrives.
struct Tripwire {
    port: u16,
    requests: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Tripwire {
    fn new(root: &Path) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let requests = root.join("unexpected-request");
        let stop = Arc::new(AtomicBool::new(false));
        let (flag, record) = (Arc::clone(&stop), requests.clone());
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    return;
                }
                if stream.is_ok() {
                    fs::write(&record, "unexpected network connection").unwrap();
                }
            }
        });
        Self { port, requests, stop, thread: Some(thread) }
    }
}

impl Drop for Tripwire {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking accept; the flag makes it return.
        let _ = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], self.port)));
        if let Some(thread) = self.thread.take() {
            thread.join().expect("network observation thread");
        }
    }
}

fn network_tripwire_observes_absolute_curl_connections() {
    let directory = TempDir::new("hamn-network-tripwire-");
    let tripwire = Tripwire::new(directory.path());
    let result = upgrade::run(
        Command::new("/usr/bin/curl").args([
            "--disable",
            "--noproxy",
            "*",
            "--connect-timeout",
            "2",
            "--max-time",
            "3",
            &format!("https://127.0.0.1:{}/forbidden", tripwire.port),
        ]),
        Duration::from_secs(5),
    );
    assert_ne!(result.returncode, 0);
    assert!(tripwire.requests.exists(), "connection observer was insensitive");
}

/// Mode, inode, size, modification time and contents of every path below
/// and including `profile`.
type Snapshot = BTreeMap<PathBuf, (u32, u64, u64, i64, i64, Option<Vec<u8>>)>;

fn snapshot(profile: &Path) -> Snapshot {
    let mut result = Snapshot::new();
    let mut pending = vec![profile.to_path_buf()];
    while let Some(path) = pending.pop() {
        let info = fs::symlink_metadata(&path).unwrap();
        if info.is_dir() {
            pending.extend(fs::read_dir(&path).unwrap().map(|entry| entry.unwrap().path()));
        }
        let contents = info.is_file().then(|| fs::read(&path).unwrap());
        let key = path.strip_prefix(profile).unwrap().to_path_buf();
        result.insert(key, (info.mode(), info.ino(), info.size(), info.mtime(), info.mtime_nsec(), contents));
    }
    result
}

fn generated_profile_trees_survive_noop_repair_and_recovery(case: u64, point: &str) {
    // Each case draws from its own stream so one case can run alone.
    let mut random = Random(SEED + case);
    let directory = TempDir::new("hamn-property-transaction-");
    let root = fs::canonicalize(directory.path()).unwrap();
    let native = root.join("native-hamn");
    fs::copy(crate::support::hamn(), &native).unwrap();
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    let (bindir, datadir) = (home.join("bin"), home.join("source"));
    let release = root.join("release");
    fs::create_dir_all(release.join("bin")).unwrap();
    let version = format!("1.{}.{}", random.range(0, 10000), random.range(0, 10000));
    // The real-binary CLI suite owns frontend coverage. Only the version is
    // generated; every private operation runs the frozen real executable.
    write_executable(&release.join("bin/hamn"), &version_wrapper(&version, &native));
    copy_release_support(&release);
    fs::write(release.join("packaging/release/update-manifest-url"), "https://fixture.test/manifest-v3.json\n").unwrap();
    let archive = root.join("host.tar.gz");
    pack_release(&release, &archive);
    let guest = root.join("guest.img");
    let size = random.range(1, 8193);
    fs::write(&guest, random.bytes(size)).unwrap();
    let (host_artifact, guest_artifact) = (Artifact::local(&archive), Artifact::local(&guest));
    let mut value = upgrade::manifest(&format!("v{version}"), &host_artifact, &guest_artifact);
    let manifest_path = root.join("manifest.json");
    write_private_json(&manifest_path, &value);
    let command = bindir.join("hamn");
    let installed_updater = || {
        let target = fs::canonicalize(&command).unwrap();
        target.parent().and_then(Path::parent).unwrap().join("share/hamn/src/scripts/update-host.sh")
    };
    let updater = |path: &Path, bootstrap: bool, extra: &[(&str, &Path)]| {
        let script = if bootstrap { upgrade::checkout().join("scripts/update-host.sh") } else { installed_updater() };
        let mut process = Command::new("bash");
        process
            .arg(script)
            .arg("--bindir")
            .arg(&bindir)
            .arg("--datadir")
            .arg(&datadir)
            .arg("--manifest")
            .arg(path)
            .arg("--output-json")
            .env("HOME", &home)
            .env("TMPDIR", &root)
            .env("HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS", "1")
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1");
        if bootstrap {
            process.arg("--bootstrap");
        }
        for (name, value) in extra {
            process.env(name, value);
        }
        process
    };
    let invoke = |path: &Path, bootstrap: bool| -> Value {
        let result = upgrade::run(&mut updater(path, bootstrap, &[]), Duration::from_secs(30));
        assert_eq!(result.returncode, 0, "{}", result.stderr());
        serde_json::from_slice(&result.stdout).unwrap()
    };
    assert_eq!(invoke(&manifest_path, true)["status"], "updated");
    let active = fs::read_link(&command).unwrap();
    let cache = home.join(".hamn/cache");
    let selection = cache.join("guest-image.json");
    let desired = fs::read(&selection).unwrap();
    let mut profiles = Vec::new();
    for name in ["profile-", "profile space ", "profile-한글-"] {
        let profile = home.join(".hamn").join(name);
        fs::create_dir_all(profile.join("nested")).unwrap();
        for (file, size) in
            [("disk.img", random.range(1, 16385)), ("config.json", 0), ("nested/user-data.bin", random.range(1, 4097))]
        {
            let path = profile.join(file);
            fs::write(&path, random.bytes(size)).unwrap();
            let mode = [0o600, 0o640, 0o400][random.range(0, 3) as usize];
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        }
        profiles.push(profile);
    }
    let before: Vec<Snapshot> = profiles.iter().map(|profile| snapshot(profile)).collect();
    let assert_preserved = || {
        assert_eq!(fs::read_link(&command).unwrap(), active);
        let after: Vec<Snapshot> = profiles.iter().map(|profile| snapshot(profile)).collect();
        assert!(after == before, "seed {} case {case}: a profile tree changed", SEED + case);
    };
    let no_transactions = || {
        let names: Vec<String> = fs::read_dir(&cache)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".hamn-update-"))
            .collect();
        assert!(names.is_empty(), "{names:?}");
    };

    // Native curl has an absolute path. Observe the actual endpoint instead
    // of relying on PATH interception or reported counters.
    let tripwire = Tripwire::new(&root);
    for name in ["host", "guestImage"] {
        value["artifacts"][name]["url"] = format!("https://127.0.0.1:{}/forbidden/{name}", tripwire.port).into();
    }
    write_private_json(&manifest_path, &value);
    let result = invoke(&manifest_path, false);
    assert_eq!(result["status"], "up-to-date");
    assert_eq!(result["downloadedBytes"], 0);
    assert_eq!(result["reusedBytes"], host_artifact.size + guest_artifact.size);
    assert_eq!(fs::read(&selection).unwrap(), desired);
    no_transactions();
    assert_preserved();

    let previous = if case == 0 {
        fs::remove_file(&selection).unwrap();
        None
    } else {
        let size = random.range(1, 1025);
        let payload = random.bytes(size);
        let key = digest(&payload);
        let name = format!("hamn-guest-{key}.img");
        fs::write(cache.join(&name), &payload).unwrap();
        fs::write(cache.join(format!("{name}.verified")), format!("{key}\n")).unwrap();
        write_private_json(&selection, &serde_json::json!({"schemaVersion": 1, "file": name, "sha256": key}));
        Some(fs::read(&selection).unwrap())
    };
    {
        let (ready, ready_fd) = ready_fifo(&root, "ready");
        let release_fifo = root.join("release-fifo");
        mkfifo(&release_fifo);
        let ready_name = format!("HAMN_TEST_UPDATE_{point}_READY_FIFO");
        let release_name = format!("HAMN_TEST_UPDATE_{point}_RELEASE_FIFO");
        let child = Group::spawn(&mut updater(&manifest_path, false, &[(&ready_name, &ready), (&release_name, &release_fifo)]));
        await_ready(&ready_fd, Duration::from_secs(20), point);
        let journal = cache.join(".hamn-update-transaction");
        let state = fs::read_to_string(journal.join("state")).unwrap();
        assert!(state.contains("hostMutation=0"), "{state}");
        let visible = fs::read(&selection).ok();
        let expected = if point == "AFTER_GUEST_SELECTION" { Some(desired.clone()) } else { previous.clone() };
        assert_eq!(visible, expected, "{point}");
        assert_preserved();
        child.signal(libc::SIGKILL);
        let killed = child.finish(Duration::from_secs(5));
        assert_eq!(killed.returncode, -libc::SIGKILL);
        let invalid = root.join("invalid.json");
        fs::write(&invalid, "{").unwrap();
        let failed = upgrade::run(&mut updater(&invalid, false, &[]), Duration::from_secs(30));
        assert_ne!(failed.returncode, 0);
        assert!(!journal.exists());
        assert_eq!(fs::read(&selection).ok(), previous);
        assert_preserved();
        fs::remove_file(&ready).unwrap();
        fs::remove_file(&release_fifo).unwrap();
    }
    let repaired = invoke(&manifest_path, false);
    assert_eq!(repaired["status"], "repaired");
    assert_eq!(repaired["downloadedBytes"], 0);
    assert_eq!(fs::read(&selection).unwrap(), desired);
    assert_eq!(invoke(&manifest_path, false)["status"], "up-to-date");
    assert!(!tripwire.requests.exists(), "an up-to-date or repair transaction connected to the release server");
    no_transactions();
    assert_preserved();
}
