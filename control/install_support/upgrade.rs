//! Native upgrade metadata/check policy; never installs or reads profile data.
//! Counters are bytes with checked u64 totals. Cache records and locks are
//! private, atomic and single-flight. Automatic transfer has a five-second
//! download-layer deadline; scheduling occurs only in the fresh helper process.
use super::{
    ReportedError, Result,
    download::{self, Counts},
    manifest::{self, Manifest},
    receipt, require,
};
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Operation,
}
#[derive(Subcommand)]
enum Operation {
    Version {
        current: String,
    },
    Manifest(Metadata),
    Check(Metadata),
    Automatic(Automatic),
    Schedule(Automatic),
    Fields {
        manifest: PathBuf,
    },
    Acquire {
        manifest: PathBuf,
        name: String,
        cache: PathBuf,
        counts: PathBuf,
    },
    Receipt {
        manifest: PathBuf,
        mode: String,
        target: String,
        cache: String,
    },
    Status {
        manifest: PathBuf,
        current: String,
        cache: PathBuf,
        target: Option<String>,
    },
    Result {
        manifest: PathBuf,
        current: String,
        status: String,
        counts: PathBuf,
    },
    ReuseCounts {
        manifest: PathBuf,
        counts: PathBuf,
        name: String,
    },
}
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}
#[derive(Args)]
struct Metadata {
    #[arg(long)]
    manifest: String,
    #[arg(long)]
    macos: String,
    #[arg(long)]
    architecture: String,
    #[arg(long, default_value_os_t = home())]
    home: PathBuf,
    #[arg(long, default_value = "0.0.0")]
    current_version: String,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    target: Option<String>,
}
#[derive(Args)]
struct Automatic {
    #[arg(long)]
    manifest: String,
    #[arg(long)]
    macos: Option<String>,
    #[arg(long)]
    architecture: Option<String>,
    #[arg(long, default_value_os_t = home())]
    home: PathBuf,
    #[arg(long, default_value = "0.0.0")]
    current_version: String,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    target: Option<String>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CheckRecord {
    schema_version: u32,
    checked_at: u64,
    ok: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    latest_version: Option<String>,
}
fn read_check(cache: &Path, now: u64) -> Option<CheckRecord> {
    let bytes = download::read_file(&cache.join("update-check-v1.json"), 4096, true).ok()?;
    let value: CheckRecord = serde_json::from_slice(&bytes).ok()?;
    if value.schema_version != 1
        || value.checked_at > now
        || (value.ok && value.latest_version.is_none())
        || value
            .latest_version
            .as_deref()
            .is_some_and(|version| manifest::stable_version(version).is_err())
    {
        return None;
    }
    Some(value)
}
fn automatic_at(
    args: &Automatic,
    now: u64,
    fetch: impl FnOnce() -> Result<Manifest>,
) -> Result<()> {
    manifest::stable_version(&args.current_version)?;
    let cache = download::cache_root(&args.home)?;
    let _lock = download::lock(&cache.join(".update-check.lock"), false)?;
    let existing = read_check(&cache, now);
    if existing
        .as_ref()
        .is_some_and(|record| now - record.checked_at < if record.ok { 86400 } else { 21600 })
    {
        return Ok(());
    }
    let mut record = CheckRecord {
        schema_version: 1,
        checked_at: now,
        ok: false,
        latest_version: None,
    };
    match fetch() {
        Ok(manifest) => {
            record.ok = true;
            record.latest_version = Some(manifest.version.trim_start_matches('v').into());
        }
        Err(_) => {
            if let Some(previous) = existing.filter(|record| record.ok) {
                record.latest_version = previous.latest_version;
            }
        }
    }
    download::atomic_json(&cache.join("update-check-v1.json"), &record)
}
fn system_macos() -> Result<String> {
    let output = Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()?;
    require(output.status.success(), "cannot determine macOS version")?;
    Ok(String::from_utf8(output.stdout)?.trim().into())
}
fn automatic(args: Automatic) -> Result<()> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    automatic_at(&args, now, || {
        let os = args
            .macos
            .as_deref()
            .map(String::from)
            .map(Ok)
            .unwrap_or_else(system_macos)?;
        let architecture =
            args.architecture
                .as_deref()
                .unwrap_or(if cfg!(target_arch = "aarch64") {
                    "arm64"
                } else {
                    std::env::consts::ARCH
                });
        let (bytes, _) = download::fetch_manifest(&args.manifest, true)?;
        manifest::parse(&bytes, &os, architecture)
    })
}
fn detach() -> Result<bool> {
    // main dispatches this private operation before threads/async initialization.
    // No pointers escape fork/setsid; the intermediate child only detaches and
    // exits, and its exact PID is reaped. The OS owns the final child's lifetime.
    let first = unsafe { libc::fork() };
    if first < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if first > 0 {
        let mut status = 0;
        loop {
            if unsafe { libc::waitpid(first, &mut status, 0) } >= 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
        require(
            libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
            "cannot detach update checker",
        )?;
        return Ok(false);
    }
    if unsafe { libc::setsid() } < 0 {
        unsafe { libc::_exit(1) };
    }
    let second = unsafe { libc::fork() };
    if second < 0 {
        unsafe { libc::_exit(1) };
    }
    if second > 0 {
        unsafe { libc::_exit(0) };
    }
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    // A direct private-helper caller may have captured pipes. The detached
    // worker must not keep them or a terminal alive after the launcher exits.
    use std::os::fd::AsRawFd;
    let null = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")?;
    for descriptor in 0..=2 {
        if unsafe { libc::dup2(null.as_raw_fd(), descriptor) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(true)
}
/// The selected guest image record (`guest-image.json`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Selection {
    schema_version: u32,
    file: String,
    sha256: String,
}
fn guest_healthy(cache: &Path, artifact: &manifest::Artifact) -> bool {
    (|| -> Result<bool> {
        let name = format!("hamn-guest-{}.img", artifact.sha256);
        let selection: Selection = serde_json::from_slice(&download::read_file(
            &cache.join("guest-image.json"),
            4096,
            false,
        )?)?;
        let marker = String::from_utf8(download::read_file(
            &cache.join(format!("{name}.verified")),
            128,
            false,
        )?)?;
        Ok(selection.schema_version == 1
            && selection.file == name
            && selection.sha256 == artifact.sha256
            && marker.trim() == artifact.sha256
            && download::verified(
                &cache.join(name),
                &artifact.acquisition(),
                download::GUEST_LIMIT,
            )?)
    })()
    .unwrap_or(false)
}
fn receipt_run(mode: &str, target: &str, manifest: &Manifest, cache: &Path) -> Result<()> {
    // The receipt binds digests, not byte sizes. `check` also validates the
    // selected guest image against the manifest's exact size, as check-only
    // status does, before it may authorize a no-op or report reused bytes.
    receipt::run(
        if mode == "check" { "host-check" } else { mode },
        target,
        &manifest.version,
        &manifest.artifacts.host.sha256,
        &manifest.artifacts.guest_image.sha256,
    )?;
    if mode == "check" {
        require(
            guest_healthy(cache, &manifest.artifacts.guest_image),
            "cached image does not match manifest",
        )?;
    }
    Ok(())
}
fn version_status(
    current: &str,
    value: &Manifest,
    cache: &Path,
    target: Option<&str>,
) -> Result<&'static str> {
    let latest = manifest::stable_version(&value.version)?;
    let current = manifest::stable_version(current)?;
    if latest > current {
        return Ok("update-available");
    }
    if latest < current {
        return Ok("ahead");
    }
    let healthy = fs::symlink_metadata(cache.join(".hamn-update-transaction"))
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        && guest_healthy(cache, &value.artifacts.guest_image)
        && target.is_some_and(|target| receipt_run("host-check", target, value, cache).is_ok());
    Ok(if healthy {
        "up-to-date"
    } else {
        "repair-required"
    })
}
fn empty_counts() -> Counts {
    Counts {
        source: "none".into(),
        ..Counts::default()
    }
}
fn result(
    current: &str,
    value: &Manifest,
    status: &str,
    mut counts: BTreeMap<String, Counts>,
) -> Result<Value> {
    require(
        counts
            .keys()
            .all(|name| ["manifest", "host", "guestImage"].contains(&name.as_str())),
        "unknown transfer accounting source",
    )?;
    for name in ["manifest", "host", "guestImage"] {
        counts.entry(name.into()).or_insert_with(empty_counts);
    }
    let mut totals = [0_u64; 3];
    for item in counts.values() {
        for (total, value) in
            totals
                .iter_mut()
                .zip([item.downloaded_bytes, item.resumed_bytes, item.reused_bytes])
        {
            *total = total
                .checked_add(value)
                .ok_or("transfer accounting overflow")?;
        }
    }
    Ok(
        json!({"schemaVersion":1,"currentVersion":current.strip_prefix('v').unwrap_or(current),"latestVersion":value.version.strip_prefix('v').unwrap_or(&value.version),"status":status,
        "downloadedBytes":totals[0],"resumedBytes":totals[1],"reusedBytes":totals[2],"artifacts":counts,"profileDisksChanged":false,"completed":true}),
    )
}
fn unsupported(current: &str) -> Value {
    json!({"schemaVersion":1,"currentVersion":current,"latestVersion":null,"status":"unsupported-install","downloadedBytes":0,"resumedBytes":0,"reusedBytes":0,
    "artifacts":{"manifest":empty_counts(),"host":empty_counts(),"guestImage":empty_counts()},"profileDisksChanged":false,"completed":true})
}

pub(super) fn run(args: &[String]) -> Result<()> {
    let operation = Cli::try_parse_from(
        std::iter::once("upgrade-support".to_owned()).chain(args.iter().cloned()),
    )?
    .command;
    match operation {
        Operation::Version { current } => {
            manifest::stable_version(&current)?;
        }
        Operation::Schedule(args) => {
            if detach()? {
                automatic(args)?;
            }
        }
        Operation::Automatic(args) => automatic(args)?,
        Operation::Manifest(args) => {
            manifest::stable_version(&args.current_version)?;
            let (bytes, amount) = download::fetch_manifest(&args.manifest, false)?;
            let value = manifest::parse(&bytes, &args.macos, &args.architecture)?;
            download::atomic_json(
                args.output.as_deref().ok_or("manifest requires --output")?,
                &value,
            )?;
            println!("{amount}");
        }
        Operation::Check(args) => {
            if manifest::stable_version(&args.current_version).is_err() {
                println!("{}", unsupported(&args.current_version));
                return Ok(());
            }
            let (bytes, amount) = download::fetch_manifest(&args.manifest, false)?;
            let value = manifest::parse(&bytes, &args.macos, &args.architecture)?;
            let status = version_status(
                &args.current_version,
                &value,
                &args.home.join(".hamn/cache"),
                args.target.as_deref(),
            )?;
            let counts = Counts {
                downloaded_bytes: amount,
                source: if amount == 0 { "local" } else { "network" }.into(),
                ..Counts::default()
            };
            println!(
                "{}",
                result(
                    &args.current_version,
                    &value,
                    status,
                    BTreeMap::from([("manifest".into(), counts)])
                )?
            );
        }
        Operation::Fields { manifest: path } => manifest::load(&path)?.print_fields(),
        Operation::Status {
            manifest: path,
            current,
            cache,
            target,
        } => println!(
            "{}",
            version_status(&current, &manifest::load(&path)?, &cache, target.as_deref())?
        ),
        Operation::Receipt {
            manifest: path,
            mode,
            target,
            cache,
        } => receipt_run(&mode, &target, &manifest::load(&path)?, Path::new(&cache))?,
        Operation::Acquire {
            manifest: path,
            name,
            cache,
            counts,
        } => {
            let acquired = (|| -> Result<PathBuf> {
                let value = manifest::load(&path)?;
                let label = match name.as_str() {
                    "host" => format!(
                        "Downloading Hamn {}",
                        value.version.strip_prefix('v').unwrap_or(&value.version)
                    ),
                    _ => "Downloading guest image".to_owned(),
                };
                let (path, amount) = download::acquire(
                    &cache,
                    &value.artifact(&name)?.acquisition(),
                    &name,
                    &label,
                )?;
                download::atomic_json(&counts, &amount)?;
                Ok(path)
            })();
            match acquired {
                Ok(path) => println!("{}", path.display()),
                Err(error) => {
                    // The updater reports this reason once, beside the counts
                    // (`<name>.reason`; results read only `*.json`). Print it
                    // here only when that handoff itself fails.
                    let reason = counts.with_extension("reason");
                    if download::atomic_text(&reason, &error.to_string()).is_err() {
                        eprintln!("hamn: {error}");
                    }
                    return Err(ReportedError(error.to_string()).into());
                }
            }
        }
        Operation::Result {
            manifest: path,
            current,
            status,
            counts,
        } => {
            let mut values = BTreeMap::new();
            for entry in fs::read_dir(counts)? {
                let path = entry?.path();
                if path.extension().is_none_or(|s| s != "json") {
                    continue;
                }
                let key = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or("invalid accounting filename")?
                    .to_owned();
                let count: Counts =
                    serde_json::from_slice(&download::read_file(&path, 4096, true)?)?;
                values.insert(key, count);
            }
            println!(
                "{}",
                result(&current, &manifest::load(&path)?, &status, values)?
            );
        }
        Operation::ReuseCounts {
            manifest: path,
            counts,
            name,
        } => {
            let value = manifest::load(&path)?;
            let names: &[&str] = match name.as_str() {
                "both" => &["host", "guestImage"],
                "host" => &["host"],
                "guestImage" => &["guestImage"],
                _ => return Err("unknown release artifact".into()),
            };
            // The caller verified these artifacts (receipt and guest checks)
            // against the manifest, whose sizes are exact.
            for name in names {
                let artifact = value.artifact(name)?;
                download::atomic_json(
                    &counts.join(format!("{name}.json")),
                    &Counts {
                        reused_bytes: artifact.size,
                        source: "installed".into(),
                        ..Counts::default()
                    },
                )?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_support::test_support::Temp;
    fn value() -> Manifest {
        manifest::parse(
            &manifest::tests::fixture().to_string().into_bytes(),
            "13",
            "arm64",
        )
        .unwrap()
    }
    fn args(home: PathBuf) -> Automatic {
        Automatic {
            manifest: "https://unused.invalid/manifest".into(),
            macos: Some("13".into()),
            architecture: Some("arm64".into()),
            home,
            current_version: "1.0.0".into(),
            output: None,
            target: None,
        }
    }
    #[test]
    fn automatic_success_and_failure_ttl_boundaries_preserve_profiles_and_prior_notice() {
        let t = Temp::new();
        let args = args(t.0.clone());
        let cache = download::cache_root(&args.home).unwrap();
        let now = 1_000_000;
        let profile = t.0.join(".hamn/profile");
        fs::create_dir(&profile).unwrap();
        fs::write(profile.join("disk.img"), b"preserved").unwrap();
        for (ok, ttl) in [(true, 86400), (false, 21600)] {
            for age in [ttl - 1, ttl] {
                // The prior record names an older release than the fetched
                // manifest (v1.2.3), so a refresh is distinguishable from reuse.
                download::atomic_json(
                    &cache.join("update-check-v1.json"),
                    &CheckRecord {
                        schema_version: 1,
                        checked_at: now - age,
                        ok,
                        latest_version: Some("1.1.0".into()),
                    },
                )
                .unwrap();
                let mut calls = 0;
                automatic_at(&args, now, || {
                    calls += 1;
                    Ok(value())
                })
                .unwrap();
                assert_eq!(calls, usize::from(age == ttl));
                let record = read_check(&cache, now).unwrap();
                let expected = if age == ttl {
                    (true, now, "1.2.3")
                } else {
                    (ok, now - age, "1.1.0")
                };
                assert_eq!(
                    (
                        record.ok,
                        record.checked_at,
                        record.latest_version.as_deref().unwrap()
                    ),
                    expected
                );
            }
        }
        download::atomic_json(
            &cache.join("update-check-v1.json"),
            &CheckRecord {
                schema_version: 1,
                checked_at: now - 86400,
                ok: true,
                latest_version: Some("1.2.3".into()),
            },
        )
        .unwrap();
        automatic_at(&args, now, || Err("offline".into())).unwrap();
        let record = read_check(&cache, now).unwrap();
        assert!(!record.ok);
        assert_eq!(record.latest_version.as_deref(), Some("1.2.3"));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(cache.join("update-check-v1.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(fs::read(profile.join("disk.img")).unwrap(), b"preserved");
    }
    #[test]
    fn automatic_refresh_is_bounded_by_the_automatic_transfer_deadline() {
        // Contract: automatic checks allow 2 s to connect (including TLS) and
        // 5 s in total. A listener that never accepts stalls the handshake, so
        // only the automatic deadline can end this refresh promptly.
        let t = Temp::new();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut args = args(t.0.clone());
        args.manifest = format!(
            "https://127.0.0.1:{}/manifest",
            listener.local_addr().unwrap().port()
        );
        let started = std::time::Instant::now();
        automatic(args).unwrap();
        let elapsed = started.elapsed();
        drop(listener);
        assert!(
            elapsed < std::time::Duration::from_secs(8),
            "automatic refresh exceeded its deadline: {elapsed:?}"
        );
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let record = read_check(&t.0.join(".hamn/cache"), now).unwrap();
        assert!(!record.ok && record.latest_version.is_none());
    }
    #[test]
    fn development_version_check_is_unsupported_without_network_or_state() {
        let t = Temp::new();
        let home = t.0.join("home");
        fs::create_dir(&home).unwrap();
        let check = |current: &str| {
            run(&[
                "check",
                "--current-version",
                current,
                "--manifest",
                "not-a-network-url",
                "--macos",
                "13.0",
                "--architecture",
                "arm64",
                "--home",
                home.to_str().unwrap(),
            ]
            .map(String::from))
        };
        check("0.1.1-dev").unwrap();
        // A stable version must reach manifest validation; this URL is invalid.
        assert!(check("0.1.1").is_err());
        assert_eq!(fs::read_dir(&home).unwrap().count(), 0);
        let empty = json!({"downloadedBytes":0,"resumedBytes":0,"reusedBytes":0,"source":"none"});
        assert_eq!(
            unsupported("0.1.1-dev"),
            json!({"schemaVersion":1,"currentVersion":"0.1.1-dev","latestVersion":null,
                "status":"unsupported-install","downloadedBytes":0,"resumedBytes":0,"reusedBytes":0,
                "artifacts":{"manifest":empty,"host":empty,"guestImage":empty},
                "profileDisksChanged":false,"completed":true})
        );
    }
    #[test]
    fn generated_accounting_sums_every_source_and_rejects_only_real_overflow() {
        // Property 12 (transfer accounting conservation), seed 20260925:
        // 12 small totals, then totals u64::MAX-2 ..= u64::MAX+2 split across
        // three sources. Totals are summed in u128 independently of result().
        let value = value();
        let mut seed = 20260925u64;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            seed
        };
        for case in 0..24_u64 {
            let parts: [u64; 3] = if case < 12 {
                let total = next() % (1 << 20);
                let mut cuts = [0, next() % (total + 1), next() % (total + 1), total];
                cuts.sort_unstable();
                [cuts[1] - cuts[0], cuts[2] - cuts[1], cuts[3] - cuts[2]]
            } else {
                let total = u128::from(u64::MAX) + u128::from(case % 5) - 2;
                let first = 2 + next() % (u64::MAX / 4);
                let second = next() % (u64::MAX / 4);
                let third = total - u128::from(first) - u128::from(second);
                [first, second, u64::try_from(third).unwrap()]
            };
            let mut counts = BTreeMap::new();
            for (name, downloaded) in ["manifest", "host", "guestImage"].into_iter().zip(parts) {
                counts.insert(
                    name.to_owned(),
                    Counts {
                        downloaded_bytes: downloaded,
                        resumed_bytes: next() % downloaded.saturating_add(1).max(1),
                        reused_bytes: next() % 65537,
                        source: "generated".into(),
                    },
                );
            }
            let sum = |field: fn(&Counts) -> u64| -> u128 {
                counts.values().map(|item| u128::from(field(item))).sum()
            };
            let expected = [
                sum(|item| item.downloaded_bytes),
                sum(|item| item.resumed_bytes),
                sum(|item| item.reused_bytes),
            ];
            let actual = result("1.2.2", &value, "updated", counts.clone());
            if expected[0] > u128::from(u64::MAX) {
                assert!(actual.is_err(), "case {case} accepted overflow");
                continue;
            }
            let actual = actual.unwrap();
            for (field, total) in ["downloadedBytes", "resumedBytes", "reusedBytes"]
                .into_iter()
                .zip(expected)
            {
                assert_eq!(
                    u128::from(actual[field].as_u64().unwrap()),
                    total,
                    "case {case}"
                );
            }
            assert_eq!(actual["artifacts"], serde_json::to_value(&counts).unwrap());
        }
    }
    #[test]
    fn automatic_rejects_future_duplicate_unsafe_records_and_busy_lock() {
        let t = Temp::new();
        let args = args(t.0.clone());
        let cache = download::cache_root(&args.home).unwrap();
        download::atomic_json(
            &cache.join("update-check-v1.json"),
            &CheckRecord {
                schema_version: 1,
                checked_at: 1001,
                ok: true,
                latest_version: Some("1.2.3".into()),
            },
        )
        .unwrap();
        assert!(read_check(&cache, 1000).is_none());
        fs::write(cache.join("update-check-v1.json"), br#"{"schemaVersion":1,"checkedAt":1,"ok":true,"latestVersion":"1.2.3","latestVersion":"9.0.0"}"#).unwrap();
        assert!(read_check(&cache, 1000).is_none());
        let lock = download::lock(&cache.join(".update-check.lock"), false).unwrap();
        assert!(automatic_at(&args, 1000, || panic!("busy lock must not fetch")).is_err());
        drop(lock);
        fs::remove_file(cache.join("update-check-v1.json")).unwrap();
        fs::write(t.0.join("outside"), b"sentinel").unwrap();
        std::os::unix::fs::symlink(t.0.join("outside"), cache.join("update-check-v1.json"))
            .unwrap();
        assert!(automatic_at(&args, 1000, || Ok(value())).is_err());
        assert_eq!(fs::read(t.0.join("outside")).unwrap(), b"sentinel");
    }
    #[test]
    fn check_cache_requires_latest_version_key_but_accepts_null_for_failure() {
        let t = Temp::new();
        let args = args(t.0.clone());
        let cache = download::cache_root(&args.home).unwrap();
        let path = cache.join("update-check-v1.json");
        download::atomic_json(
            &path,
            &json!({"schemaVersion":1,"checkedAt":999,"ok":false}),
        )
        .unwrap();
        assert!(read_check(&cache, 1000).is_none());
        let mut calls = 0;
        automatic_at(&args, 1000, || {
            calls += 1;
            Err("offline".into())
        })
        .unwrap();
        assert_eq!(calls, 1, "an incomplete record must not suppress a check");
        let repaired = read_check(&cache, 1000).unwrap();
        assert!(!repaired.ok);
        assert!(repaired.latest_version.is_none());
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap(),
            json!({"schemaVersion":1,"checkedAt":1000,"ok":false,"latestVersion":null})
        );
        automatic_at(&args, 1001, || panic!("valid failed record must back off")).unwrap();
    }
    #[test]
    fn guest_selection_rejects_duplicate_and_unknown_fields() {
        let good = r#"{"schemaVersion":1,"file":"image","sha256":"digest"}"#;
        assert!(serde_json::from_str::<Selection>(good).is_ok());
        for bad in [
            good.replace(
                "\"schemaVersion\":1",
                "\"schemaVersion\":1,\"schemaVersion\":1",
            ),
            good.replace(
                "\"file\":\"image\"",
                "\"file\":\"other\",\"file\":\"image\"",
            ),
            good.replace('{', "{\"unknown\":true,"),
        ] {
            assert!(serde_json::from_str::<Selection>(&bad).is_err());
        }
    }
    #[test]
    fn checked_accounting_and_version_status_keep_repair_distinct() {
        let t = Temp::new();
        let value = value();
        assert_eq!(
            version_status("1.2.2", &value, &t.0, None).unwrap(),
            "update-available"
        );
        assert_eq!(
            version_status("1.2.4", &value, &t.0, None).unwrap(),
            "ahead"
        );
        assert_eq!(
            version_status("1.2.3", &value, &t.0, None).unwrap(),
            "repair-required"
        );
        let counters = BTreeMap::from([
            (
                "host".into(),
                Counts {
                    downloaded_bytes: u64::MAX,
                    ..empty_counts()
                },
            ),
            (
                "guestImage".into(),
                Counts {
                    downloaded_bytes: 1,
                    ..empty_counts()
                },
            ),
        ]);
        assert!(result("1.0.0", &value, "updated", counters).is_err());
        assert!(
            result(
                "1.0.0",
                &value,
                "updated",
                BTreeMap::from([("unknown".into(), empty_counts())])
            )
            .is_err()
        );
        assert_eq!(
            result("v1.0.0", &value, "updated", BTreeMap::new()).unwrap()["downloadedBytes"],
            0
        );
    }
}
