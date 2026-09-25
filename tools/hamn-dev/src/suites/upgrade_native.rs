//! The private native upgrade operations (`hamn __install-support upgrade
//! ...`) against expectations derived from the documented manifest and
//! result contracts (docs/INSTALLATION.md), not from a second implementation.
//! No release network, shared build or VM operation; local artifacts are
//! allowed only for these child processes and their private files.
use crate::runner::{self, case};
use crate::support::tmp::TempDir;
use crate::support::upgrade::{self, Artifact, Output, digest, write_private_json};
use serde_json::{Value, json};
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "upgrade-native",
        "native manifest, version, acquisition, accounting and automatic-check contracts",
        vec![
            case("manifest_v3_and_rejections_follow_the_published_contract", manifest_v3_and_rejections_follow_the_published_contract),
            case("canonical_versions_compare_numerically_and_reject_overflow", canonical_versions_compare_numerically_and_reject_overflow),
            case("local_acquisition_reuse_and_checked_accounting", local_acquisition_reuse_and_checked_accounting),
            case(
                "unsupported_check_is_read_only_and_does_not_validate_network",
                unsupported_check_is_read_only_and_does_not_validate_network,
            ),
            case("automatic_ttl_and_cross_process_lock_without_network", automatic_ttl_and_cross_process_lock_without_network),
        ],
        filters,
    )
}

/// Manifests are limited to 256 KiB.
const MANIFEST_LIMIT: usize = 256 * 1024;
/// Host archives are limited to 128 MiB.
const HOST_LIMIT: u64 = 128 * 1024 * 1024;
const COUNTERS: [&str; 3] = ["downloadedBytes", "resumedBytes", "reusedBytes"];

/// A valid stable schema v3 release naming `payload` for both artifacts.
pub fn manifest(payload: &[u8], version: &str) -> Value {
    let artifact =
        Artifact { url: "https://fixture.test/artifact".into(), sha256: digest(payload), size: payload.len() as u64 };
    upgrade::manifest(version, &artifact, &artifact)
}

/// The documented result object: per-source records plus checked totals.
fn expected_result(current: &str, value: &Value, status: &str, counts: &serde_json::Map<String, Value>) -> Value {
    let empty = json!({"downloadedBytes": 0, "resumedBytes": 0, "reusedBytes": 0, "source": "none"});
    let artifacts: serde_json::Map<String, Value> = ["manifest", "host", "guestImage"]
        .into_iter()
        .map(|name| (name.to_owned(), counts.get(name).cloned().unwrap_or_else(|| empty.clone())))
        .collect();
    let mut result = json!({"schemaVersion": 1, "currentVersion": current.trim_start_matches('v'),
        "latestVersion": value["version"].as_str().unwrap().trim_start_matches('v'), "status": status,
        "artifacts": artifacts, "profileDisksChanged": false, "completed": true});
    for field in COUNTERS {
        result[field] = artifacts.values().map(|item| item[field].as_u64().unwrap()).sum::<u64>().into();
    }
    result
}

struct Native {
    root: PathBuf,
    path: PathBuf,
    _directory: TempDir,
}

impl Native {
    fn new() -> Self {
        let directory = TempDir::new("hamn-native-upgrade-");
        let root = directory.path().to_path_buf();
        let path = root.join("manifest.json");
        write_private_json(&path, &manifest(b"release", "v1.2.3"));
        Self { root, path, _directory: directory }
    }

    /// `hamn __install-support upgrade ARGS...` with HOME at the root.
    fn call(&self, args: &[&dyn AsRef<std::ffi::OsStr>], success: bool) -> Output {
        let mut command = Command::new(crate::support::hamn());
        command
            .args(["__install-support", "upgrade"])
            .args(args.iter().map(|arg| arg.as_ref()))
            .env("HOME", &self.root)
            .env("HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS", "1");
        let result = upgrade::run(&mut command, Duration::from_secs(8));
        assert_eq!(result.returncode == 0, success, "{:?}: {}", args.iter().map(|a| a.as_ref()).collect::<Vec<_>>(), result.stderr());
        result
    }

    /// Validates `text` as the manifest through `upgrade manifest` into
    /// `parsed.json`; on success the record must equal `expected`.
    fn metadata(&self, text: &str, expected: Option<&Value>) -> (PathBuf, Output) {
        fs::write(&self.path, text).unwrap();
        let output = self.root.join("parsed.json");
        let uri = format!("file://{}", self.path.display());
        let result = self.call(
            &[&"manifest", &"--manifest", &uri, &"--macos", &"13.0", &"--architecture", &"arm64", &"--output", &output],
            expected.is_some(),
        );
        if let Some(expected) = expected {
            // Local manifests transfer no network bytes; the accepted
            // manifest is recorded unchanged.
            assert_eq!(result.stdout(), "0\n");
            let recorded: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
            assert_eq!(&recorded, expected);
        }
        (output, result)
    }
}

fn manifest_v3_and_rejections_follow_the_published_contract() {
    let native = Native::new();
    let valid = manifest(b"release", "v1.2.3");
    let (output, _) = native.metadata(&valid.to_string(), Some(&valid));
    let original = fs::read(&output).unwrap();
    let mut invalid: Vec<String> = Vec::new();
    for (key, value) in [
        ("size", json!(true)),
        ("size", json!(0)),
        ("size", json!(HOST_LIMIT + 1)),
        ("size", Value::Null),
        ("url", json!("http://example.test/host")),
        ("sha256", json!("A".repeat(64))),
    ] {
        let mut item = valid.clone();
        item["artifacts"]["host"][key] = value;
        invalid.push(item.to_string());
    }
    let mut unsized_host = valid.clone();
    unsized_host["artifacts"]["host"].as_object_mut().unwrap().remove("size");
    invalid.push(unsized_host.to_string());
    for (key, value) in [
        ("schemaVersion", json!(true)),
        ("schemaVersion", json!(2)),
        ("version", json!("v01.2.3")),
        ("repository", json!("legacy/hamn")),
        ("unexpected", json!(1)),
    ] {
        let mut item = valid.clone();
        item[key] = value;
        invalid.push(item.to_string());
    }
    invalid.push(r#"{"schemaVersion":3,"schemaVersion":3}"#.into());
    invalid.push("NaN".into());
    invalid.push(format!("{{{}", " ".repeat(MANIFEST_LIMIT)));
    for item in &invalid {
        native.metadata(item, None);
        assert_eq!(fs::read(&output).unwrap(), original, "{}", &item[..item.len().min(120)]);
    }
    // A retired schema v2 manifest (no sizes, `repository`) is refused by its
    // schema, with that reason.
    let mut v2 = valid.clone();
    v2["schemaVersion"] = 2.into();
    v2["repository"] = "owner/hamn".into();
    for name in ["host", "guestImage"] {
        v2["artifacts"][name].as_object_mut().unwrap().retain(|key, _| key == "url" || key == "sha256");
    }
    let (_, refused) = native.metadata(&v2.to_string(), None);
    assert!(refused.stderr().contains("manifest schema v2 is not supported; this Hamn reads only schema v3"), "{}", refused.stderr());
    assert_eq!(fs::read(&output).unwrap(), original);
}

/// A deterministic LCG, so every case is reproducible (seed recorded per case).
fn generator(seed: u64) -> impl FnMut() -> u64 {
    let mut state = seed;
    move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        state >> 33
    }
}

fn canonical_versions_compare_numerically_and_reject_overflow() {
    // Property 1 (stable semantic version ordering), seed 20260921: 100
    // pairs of components below 100, so equal leading parts are common.
    let native = Native::new();
    let mut next = generator(20260921);
    for case in 0..100 {
        let mut component = || next() % 100;
        let current = [component(), component(), component()];
        let latest = [component(), component(), component()];
        let text = |v: [u64; 3]| format!("{}.{}.{}", v[0], v[1], v[2]);
        write_private_json(&native.path, &manifest(b"release", &format!("v{}", text(latest))));
        let actual = native.call(&[&"status", &native.path, &text(current), &native.root], true).stdout();
        let expected = if latest > current {
            "update-available"
        } else if latest < current {
            "ahead"
        } else {
            "repair-required"
        };
        assert_eq!(actual.trim(), expected, "seed 20260921 case {case}: {current:?} {latest:?}");
    }
    for value in ["1.2.3", "v4294967295.0.0"] {
        native.call(&[&"version", &value], true);
    }
    for value in ["1.2", "01.2.3", "1.2.3-rc.1", "1.2.3+build", "4294967296.0.0"] {
        native.call(&[&"version", &value], false);
    }
}

fn local_acquisition_reuse_and_checked_accounting() {
    let native = Native::new();
    let payload = native.root.join("payload");
    fs::write(&payload, "owned artifact").unwrap();
    let bytes = fs::read(&payload).unwrap();
    let mut value = manifest(&bytes, "v1.2.3");
    for name in ["host", "guestImage"] {
        value["artifacts"][name]["url"] = format!("file://{}", payload.display()).into();
    }
    fs::write(&native.path, value.to_string()).unwrap();
    let cache = native.root.join("cache");
    fs::DirBuilder::new().mode(0o755).create(&cache).unwrap();
    let counts = native.root.join("counts");
    fs::DirBuilder::new().mode(0o700).create(&counts).unwrap();
    for name in ["host", "guestImage"] {
        let record = counts.join(format!("{name}.json"));
        let output = native.call(&[&"acquire", &native.path, &name, &cache, &record], true);
        assert_eq!(fs::read(output.stdout().trim()).unwrap(), bytes);
    }
    let recorded: serde_json::Map<String, Value> = fs::read_dir(&counts)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_stem().unwrap().to_str().unwrap().to_owned();
            (name, serde_json::from_slice(&fs::read(&path).unwrap()).unwrap())
        })
        .collect();
    assert_eq!(recorded.keys().collect::<Vec<_>>(), ["guestImage", "host"]);
    // A local copy transfers no network bytes. Both artifacts name the same
    // digest, so the second acquisition reuses the content-addressed file.
    for (name, source) in [("host", "local"), ("guestImage", "cache")] {
        assert_eq!(
            recorded[name],
            json!({"downloadedBytes": 0, "resumedBytes": 0, "reusedBytes": bytes.len(), "source": source}),
            "{name}"
        );
    }
    let actual: Value = serde_json::from_slice(&native.call(&[&"result", &native.path, &"1.0.0", &"updated", &counts], true).stdout).unwrap();
    assert_eq!(actual, expected_result("1.0.0", &value, "updated", &recorded));
    native.call(&[&"reuse-counts", &native.path, &counts, &"both"], true);
    let host: Value = serde_json::from_slice(&fs::read(counts.join("host.json")).unwrap()).unwrap();
    assert_eq!(host["reusedBytes"], bytes.len());
    fs::write(counts.join("host.json"), json!({"downloadedBytes": u64::MAX, "resumedBytes": 0, "reusedBytes": 0, "source": "network"}).to_string()).unwrap();
    fs::write(counts.join("guestImage.json"), json!({"downloadedBytes": 1, "resumedBytes": 0, "reusedBytes": 0, "source": "network"}).to_string()).unwrap();
    native.call(&[&"result", &native.path, &"1.0.0", &"updated", &counts], false);
}

fn unsupported_check_is_read_only_and_does_not_validate_network() {
    let native = Native::new();
    let listing = |root: &Path| {
        let mut names: Vec<_> = fs::read_dir(root).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        names.sort();
        names
    };
    let before = listing(&native.root);
    let output = native.call(
        &[&"check", &"--current-version", &"1.0.0-dev", &"--manifest", &"invalid-url", &"--macos", &"13", &"--architecture", &"arm64", &"--home", &native.root],
        true,
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let empty = json!({"downloadedBytes": 0, "resumedBytes": 0, "reusedBytes": 0, "source": "none"});
    assert_eq!(
        value,
        json!({"schemaVersion": 1, "currentVersion": "1.0.0-dev", "latestVersion": null,
            "status": "unsupported-install", "downloadedBytes": 0, "resumedBytes": 0, "reusedBytes": 0,
            "artifacts": {"manifest": empty, "host": empty, "guestImage": empty},
            "profileDisksChanged": false, "completed": true})
    );
    assert_eq!(listing(&native.root), before);
}

fn automatic_ttl_and_cross_process_lock_without_network() {
    let native = Native::new();
    fs::DirBuilder::new().mode(0o700).create(native.root.join(".hamn")).unwrap();
    let cache = native.root.join(".hamn/cache");
    fs::DirBuilder::new().mode(0o755).create(&cache).unwrap();
    let path = cache.join("update-check-v1.json");
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let mut record = json!({"schemaVersion": 1, "checkedAt": now, "ok": true, "latestVersion": "1.3.0"});
    write_private_json(&path, &record);
    let args: [&dyn AsRef<std::ffi::OsStr>; 11] = [
        &"automatic", &"--manifest", &"invalid-offline-url", &"--current-version", &"1.0.0", &"--home", &native.root,
        &"--macos", &"13", &"--architecture", &"arm64",
    ];
    let read = || fs::read(&path).unwrap();
    // A fresh record suppresses a check.
    let before = read();
    native.call(&args, true);
    assert_eq!(read(), before);
    // A stale record with the single-flight lock held by another process
    // (here the test): the checker fails without touching the record.
    record["checkedAt"] = (now - 86460).into();
    write_private_json(&path, &record);
    {
        let lock = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(cache.join(".update-check.lock")).unwrap();
        fs::set_permissions(cache.join(".update-check.lock"), fs::Permissions::from_mode(0o600)).unwrap();
        // SAFETY: flock only locks the open file description of `lock`.
        assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
        native.call(&args, false);
        assert_eq!(serde_json::from_slice::<Value>(&read()).unwrap(), record);
    }
    // Unlocked: the offline check fails and records a failure, keeping the
    // last known latest version; a fresh failure then backs off.
    native.call(&args, true);
    let updated: Value = serde_json::from_slice(&read()).unwrap();
    assert_eq!((updated["ok"].clone(), updated["latestVersion"].clone()), (json!(false), json!("1.3.0")));
    let before = read();
    native.call(&args, true);
    assert_eq!(read(), before);
    let names: Vec<_> = fs::read_dir(native.root.join(".hamn")).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert_eq!(names, ["cache"]);
}
