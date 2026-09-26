//! `guest-image-live`: opt-in same-contract physical tests of locally built
//! guest images.
//!
//! Each image gets an isolated HOME and a single 2-CPU/2-GiB VM. The
//! private cache marker is local test authorization by exact digest, not a
//! release signature or attestation. No user profile, home sharing, source
//! provisioning or Rosetta installation is used. A failed cleanup stops the
//! sequence and keeps the evidence; a failed image stops it too, since a
//! common runtime defect needs diagnosis rather than repetition.
use super::workspace_live::{
    Flags, Live, PROFILE, catch_interrupts, cli_extensions, interrupted, is_lower_hex, mkdtemp, panic_text, prepare,
    random_hex, repository, write_json,
};
use crate::release::files::{read_json, sha256_file};
use crate::release::process::{self, Spec};
use crate::release::syntax::shell_quote;
use regex::Regex;
use serde_json::{Map, Value, json};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str = "usage: hamn-dev test guest-image-live --binary BINARY --baseline IMAGE --optimized IMAGE
       --size-report REPORT --variations VARIATIONS --source-revision COMMIT
       --output-directory DIR [--only baseline|optimized|case-0|case-1]

Opt-in same-contract physical tests for locally built guest images; each
image runs in an isolated HOME. DIR must not exist.";

const REQUIRED: [&str; 7] =
    ["binary", "baseline", "optimized", "size-report", "variations", "source-revision", "output-directory"];

/// One image under test.
#[derive(Debug, PartialEq)]
struct Image {
    label: String,
    path: PathBuf,
    sha256: String,
    size: u64,
    size_report: PathBuf,
}

pub fn main(args: &[String]) -> ExitCode {
    let flags = match Flags::parse(args, &[&REQUIRED[..], &["only"]].concat(), &[], USAGE) {
        Ok(Some(flags)) => flags,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("guest-image-live: {message}");
            return ExitCode::from(2);
        }
    };
    let missing: Vec<&str> = REQUIRED.iter().copied().filter(|name| flags.value(name).is_none()).collect();
    if !missing.is_empty() {
        eprintln!("guest-image-live: missing --{}\n{USAGE}", missing.join(", --"));
        return ExitCode::from(2);
    }
    let only = flags.value("only");
    if let Some(only) = only.filter(|only| !["baseline", "optimized", "case-0", "case-1"].contains(only)) {
        eprintln!("guest-image-live: invalid --only {only:?}\n{USAGE}");
        return ExitCode::from(2);
    }
    let path = |name: &str| PathBuf::from(flags.value(name).expect("required"));
    let options = Options {
        binary: path("binary"),
        size_report: path("size-report"),
        variations: path("variations"),
        source_revision: flags.value("source-revision").expect("required").to_owned(),
        output: path("output-directory"),
    };
    let harness_sha256 = std::env::current_exe().map_err(|error| error.to_string()).and_then(|exe| sha256_file(&exe));
    let images = harness_sha256
        .and_then(|harness| inputs(&options, &path("baseline"), &path("optimized")).map(|images| (harness, images)));
    let (harness_sha256, images) = match images {
        Ok(found) => found,
        Err(message) => {
            eprintln!("guest-image-live: {message}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = fs::DirBuilder::new().mode(0o700).create(&options.output) {
        eprintln!("guest-image-live: {}: {error}", options.output.display());
        return ExitCode::FAILURE;
    }
    catch_interrupts();
    let mut results: Vec<Value> = Vec::new();
    for image in images.iter().filter(|image| only.is_none_or(|only| image.label == only)) {
        let evidence = match test_image(&options, image) {
            Ok(evidence) => evidence,
            Err(message) => {
                eprintln!("guest-image-live: {message}");
                return ExitCode::FAILURE;
            }
        };
        if interrupted() {
            // The sequence stopped; the remaining images were not run.
            return ExitCode::from(130);
        }
        results.push(evidence);
        let done: Vec<&str> = results.iter().filter_map(|result| result["label"].as_str()).collect();
        let pending: Vec<&str> =
            images.iter().map(|image| image.label.as_str()).filter(|label| !done.contains(label)).collect();
        let summary = json!({"schemaVersion": 1, "harnessSHA256": harness_sha256,
            "sourceRevision": options.source_revision,
            "allRequiredPassed": results.len() == 4 && results.iter().all(passed),
            "pending": pending, "results": results});
        write_json_line(&options.output.join("summary.json"), &summary);
        if !passed(results.last().expect("a result")) {
            break;
        }
    }
    if results.iter().all(passed) { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn passed(result: &Value) -> bool {
    result["passed"] == true
}

#[derive(Clone)]
struct Options {
    binary: PathBuf,
    size_report: PathBuf,
    variations: PathBuf,
    source_revision: String,
    output: PathBuf,
}

/// Validates the size reports and variations of one source revision and
/// every image's size and digest; returns the images in test order.
fn inputs(options: &Options, baseline: &Path, optimized: &Path) -> Result<Vec<Image>, String> {
    let check = |condition: bool, message: &str| if condition { Ok(()) } else { Err(message.to_owned()) };
    let revision = options.source_revision.as_str();
    let report = read_json(&options.size_report)?;
    check(report["sourceRevision"] == revision, "the size report is for another revision")?;
    check(is_lower_hex(revision, 40), "the source revision is not a commit")?;
    check(report["virtualBytes"] == 8u64 * 1024 * 1024 * 1024, "the images are not 8 GiB disks")?;
    let text = |value: &Value, key: &str| value[key].as_str().map(str::to_owned).ok_or(format!("no {key}"));
    let number = |value: &Value, key: &str| value[key].as_u64().ok_or(format!("no {key}"));
    let mut images = vec![
        Image {
            label: "baseline".into(),
            path: baseline.into(),
            sha256: text(&report, "baselineSha256")?,
            size: number(&report, "baselineCompressedBytes")?,
            size_report: options.size_report.clone(),
        },
        Image {
            label: "optimized".into(),
            path: optimized.into(),
            sha256: text(&report, "imageSha256")?,
            size: number(&report, "compressedBytes")?,
            size_report: options.size_report.clone(),
        },
    ];
    let variants = read_json(&options.variations)?;
    check(variants["sourceRevision"] == revision, "the variations are for another revision")?;
    check(variants["baselineSha256"] == report["baselineSha256"], "the variations have another baseline")?;
    check(
        variants["baselineSizeReportSha256"] == sha256_file(&options.size_report)?.as_str(),
        "the variations name another size report",
    )?;
    let list = variants["variants"].as_array().filter(|list| list.len() == 2).ok_or("expected two variants")?;
    let directory = options.variations.parent().unwrap_or(Path::new("."));
    for variant in list {
        let size_report = directory.join(text(variant, "sizeReport")?);
        let detail = read_json(&size_report)?;
        check(
            detail["sourceRevision"] == revision && detail["baselineSha256"] == report["baselineSha256"],
            "a variant is for another revision or baseline",
        )?;
        check(detail["imageSha256"] == variant["imageSha256"], "a variant's size report is for another image")?;
        check(variant["structuralChecks"] == "passed", "a variant failed its structural checks")?;
        let case = match &variant["case"] {
            Value::String(case) => case.clone(),
            Value::Number(case) => case.to_string(),
            _ => return Err("a variant has no case".into()),
        };
        images.push(Image {
            label: format!("case-{case}"),
            path: directory.join(text(variant, "image")?),
            sha256: text(variant, "imageSha256")?,
            size: number(&detail, "compressedBytes")?,
            size_report,
        });
    }
    for image in &images {
        let info = fs::symlink_metadata(&image.path).map_err(|error| format!("{}: {error}", image.path.display()))?;
        check(info.is_file() && info.len() == image.size, &format!("image size mismatch: {}", image.label))?;
        check(sha256_file(&image.path)? == image.sha256, &format!("image identity mismatch: {}", image.label))?;
    }
    Ok(images)
}

/// Sets the one `name: true|false` line of a profile configuration.
fn config_bool(config: &Path, name: &str, value: bool) {
    let text = fs::read_to_string(config).unwrap_or_else(|error| panic!("{}: {error}", config.display()));
    let pattern = Regex::new(&format!("(?m)^{}: (?:true|false)$", regex::escape(name))).expect("valid pattern");
    assert_eq!(pattern.find_iter(&text).count(), 1, "invalid boolean configuration: {name}");
    let replaced = pattern.replace_all(&text, format!("{name}: {value}").as_str());
    fs::write(config, replaced.as_bytes()).unwrap();
}

/// A CNI ADD, ping and DEL cycle in a network namespace. Every namespace,
/// bridge, allocation directory and temporary file is named by `token`,
/// and DEL runs even when ADD partly failed. The already run ARM64 image
/// supplies the ICMP observer, so no guest package is installed or needed.
fn cni_script(token: &str, arm_image: &str) -> String {
    assert!(is_lower_hex(token, 32), "{token}");
    assert!(arm_image.strip_prefix("sha256:").is_some_and(|digest| is_lower_hex(digest, 64)), "{arm_image}");
    let (network, bridge, namespace) =
        (format!("hamn-proof-{token}"), format!("hp{}", &token[..12]), format!("hp-{token}"));
    let third = 16 + u32::from_str_radix(&token[..2], 16).expect("hex") % 220;
    let config = json!({"cniVersion": "0.4.0", "name": network, "type": "bridge", "bridge": bridge,
        "isGateway": true, "ipMasq": false, "ipam": {"type": "host-local", "subnet": format!("10.237.{third}.0/24")}});
    let loopback = json!({"cniVersion": "0.4.0", "name": format!("{network}-lo"), "type": "loopback"});
    format!(
        r#"set -euo pipefail
network={network}
bridge={bridge}
namespace={namespace}
config={config}
loopback={loopback}
! ip link show "$bridge" >/dev/null 2>&1
! ip netns list | awk '{{print $1}}' | grep -Fx "$namespace"
test ! -e "/var/lib/cni/networks/$network"
work=$(mktemp -d /tmp/hamn-cni-proof.XXXXXX)
ns_created=0
attempted=0
tool_attempted=0
cleanup() {{
    status=$?
    trap - EXIT
    set +e
    failed=0
    if [ "$attempted" = 1 ]; then
        printf '%s' "$loopback" | CNI_COMMAND=DEL CNI_IFNAME=lo /opt/cni/bin/loopback >"$work/loop-del" 2>&1 || failed=1
        printf '%s' "$config" | CNI_COMMAND=DEL CNI_IFNAME=eth0 /opt/cni/bin/bridge >"$work/bridge-del" 2>&1 || failed=1
    fi
    if [ "$ns_created" = 1 ]; then ip netns delete "$namespace" || failed=1; fi
    if ip link show "$bridge" >/dev/null 2>&1; then ip link delete "$bridge" || failed=1; fi
    if [ -d "/var/lib/cni/networks/$network" ]; then
        if find "/var/lib/cni/networks/$network" -type f -name '10.*' | grep -q .; then failed=1; fi
        rm -rf "/var/lib/cni/networks/$network"
    fi
    if [ "$tool_attempted" = 1 ]; then
        # Re-observe after a timeout: create can succeed before its output is
        # received. The exact generated name plus owner label excludes other work.
        tool_container=$(timeout 30 docker ps -aq --no-trunc \
            --filter 'name=^/hamn-cni-ping-{token}$' --filter 'label=io.hamn.test={token}') || failed=1
        if [ -n "$tool_container" ]; then
            if [[ "$tool_container" =~ ^[0-9a-f]{{64}}$ ]]; then
                timeout 30 docker rm "$tool_container" >/dev/null || failed=1
            else failed=1; fi
        fi
    fi
    cat "$work/loop-del" "$work/bridge-del" 2>/dev/null || true
    rm -rf "$work"
    [ "$failed" = 0 ] || status=1
    if [ "$status" = 0 ]; then printf 'CNI_ADD_PING_DEL_OK %s\n' "$namespace"; fi
    exit "$status"
}}
trap cleanup EXIT
# The already executed ARM64 image supplies the test-only ICMP observer. This
# keeps minimal/standard comparisons independent of optional host ping tools
# without installing or repairing any guest package.
tool_attempted=1
tool_container=$(timeout 30 docker create --platform linux/arm64 \
    --name hamn-cni-ping-{token} --label io.hamn.test={token} {arm_image} true)
[[ "$tool_container" =~ ^[0-9a-f]{{64}}$ ]]
timeout 30 docker cp "$tool_container:/bin/busybox" "$work/busybox"
test -f "$work/busybox" && test ! -L "$work/busybox"
chmod 0755 "$work/busybox"
printf 'CNI_ICMP_TOOL_IMAGE %s\n' {arm_image}
sha256sum "$work/busybox"
export CNI_CONTAINERID={token} CNI_NETNS=/var/run/netns/$namespace CNI_PATH=/opt/cni/bin
ip netns add "$namespace"
ns_created=1
attempted=1
printf '%s' "$loopback" | CNI_COMMAND=ADD CNI_IFNAME=lo /opt/cni/bin/loopback
printf '%s' "$config" | CNI_COMMAND=ADD CNI_IFNAME=eth0 /opt/cni/bin/bridge
ip -n "$namespace" -j address show eth0
ip netns exec "$namespace" "$work/busybox" ping -c 2 -W 2 10.237.{third}.1
ip netns exec "$namespace" "$work/busybox" ping -c 2 -W 2 127.0.0.1
"#,
        network = shell_quote(&network),
        bridge = shell_quote(&bridge),
        namespace = shell_quote(&namespace),
        config = shell_quote(&config.to_string()),
        loopback = shell_quote(&loopback.to_string()),
    )
}

fn write_json_line(path: &Path, value: &Value) {
    let text = serde_json::to_string_pretty(value).expect("serializable JSON") + "\n";
    fs::write(path, text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// Runs every check of one image in its own root; returns its evidence. An
/// error means the owned VM could not be proven stopped, which stops the
/// sequence.
fn test_image(options: &Options, image: &Image) -> Result<Value, String> {
    let root = mkdtemp(Path::new("/tmp"), &format!("hamn-image-live-{}-", image.label))?;
    let root = fs::canonicalize(&root).map_err(|error| error.to_string())?;
    let home = root.join("home");
    let cache = home.join(".hamn/cache");
    fs::DirBuilder::new().mode(0o700).create(&home).map_err(|error| error.to_string())?;
    fs::DirBuilder::new().mode(0o700).create(home.join(".hamn")).map_err(|error| error.to_string())?;
    fs::DirBuilder::new().mode(0o755).create(&cache).map_err(|error| error.to_string())?;
    let name = format!("hamn-guest-{}.img", image.sha256);
    let selected = cache.join(&name);
    process::run(
        OsStr::new("/bin/cp"),
        &[OsStr::new("-c"), image.path.as_os_str(), selected.as_os_str()],
        &Spec::default(),
        Duration::from_secs(60),
    )?;
    fs::write(cache.join(format!("{name}.verified")), format!("{}\n", image.sha256))
        .map_err(|error| error.to_string())?;
    let manifest = json!({"schemaVersion": 1, "file": name, "sha256": image.sha256});
    fs::write(cache.join("guest-image.json"), manifest.to_string()).map_err(|error| error.to_string())?;
    let ownership = json!({"owner": "Hamn locally built image runtime test", "workspace": repository(),
        "profile": PROFILE, "home": home, "guestImageSha256": image.sha256,
        "trust": "explicit local test digest; not release attestation"});
    write_json(&root.join("ownership.json"), &ownership);
    let binary = fs::canonicalize(&options.binary).map_err(|error| format!("{}: {error}", options.binary.display()))?;
    let (root, runtime) = prepare(&binary, None, Some(&root))?;
    let live = Live { root, runtime };
    let token = random_hex(16);
    let (container, volume) = (format!("hamn-proof-{}", &token[..12]), format!("hamn-data-{}", &token[..12]));
    let label = image.label.as_str();
    let output = options.output.join(format!("{label}.json"));
    let mut evidence = Evidence {
        output: output.clone(),
        label: label.to_owned(),
        value: json!({"label": label, "sourceRevision": options.source_revision, "imageSHA256": image.sha256,
            "compressedBytes": image.size, "sizeReportSHA256": sha256_file(&image.size_report)?,
            "candidateBinarySHA256": sha256_file(&live.runtime.binary)?, "trust": ownership["trust"],
            "cpus": 2, "memoryGiB": 2, "mountHome": false, "checks": {}, "ownershipRoot": live.root}),
    };
    let mut created = false;
    let mut stage = "create";
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        run_image(&live, &token, &container, &volume, &mut evidence, &mut created, &mut stage)
    }));
    if let Err(payload) = &outcome {
        let error = panic_text(&**payload);
        let object = evidence.object();
        object.insert("passed".into(), json!(false));
        object.insert("failedStage".into(), json!(stage));
        if interrupted() {
            object.insert("interrupted".into(), json!(true));
            object.insert("error".into(), json!("Image sequence interrupted; remaining images were not run"));
        } else {
            object.insert("error".into(), json!(error));
            println!("{label}: FAIL {stage}: {error}");
        }
    }
    let stopped = if created {
        match live.runtime.stop(&[PROFILE.to_owned()]) {
            Ok(()) => {
                evidence.object().insert("cleanup".into(), json!("VM stopped and ownership verified"));
                true
            }
            Err(error) => {
                evidence.object().insert("cleanupError".into(), json!(error));
                false
            }
        }
    } else {
        true
    };
    for name in ["vmrun.log", "serial.log", "provision.log"] {
        let log = home.join(".hamn/verify/logs").join(name);
        if log.is_file() {
            fs::copy(&log, options.output.join(format!("{label}-{name}"))).map_err(|error| error.to_string())?;
        }
    }
    evidence.save();
    if stopped && passed(&evidence.value) {
        if read_json(&live.root.join("ownership.json"))? != ownership {
            return Err(format!("the ownership record of {} changed", live.root.display()));
        }
        fs::remove_dir_all(&live.root).map_err(|error| format!("{}: {error}", live.root.display()))?;
        evidence
            .object()
            .insert("cleanup".into(), json!("VM stopped; private HOME, profile, cache and SSH keys removed"));
        evidence.save();
    } else if stopped {
        evidence.object().insert("retainedStoppedRoot".into(), json!(live.root));
        evidence.save();
    } else {
        return Err(format!("owned VM cleanup failed; sequence stopped: {}", live.root.display()));
    }
    Ok(evidence.value)
}

/// An image's evidence document, rewritten after every check.
struct Evidence {
    output: PathBuf,
    label: String,
    value: Value,
}

impl Evidence {
    fn object(&mut self) -> &mut Map<String, Value> {
        self.value.as_object_mut().expect("evidence object")
    }

    fn save(&self) {
        write_json_line(&self.output, &self.value);
    }

    fn mark(&mut self, check: &str, details: Value) {
        self.value["checks"][check] = details;
        self.save();
        println!("{}: PASS {check}", self.label);
    }
}

fn run_image(
    live: &Live,
    token: &str,
    container: &str,
    volume: &str,
    evidence: &mut Evidence,
    created: &mut bool,
    stage: &mut &'static str,
) {
    let state = || {
        let volume_info: Value =
            serde_json::from_str(&live.docker(&["volume", "inspect", volume])).expect("volume JSON");
        let mut images: Vec<String> =
            live.docker(&["images", "-q", "--no-trunc"]).split_whitespace().map(str::to_owned).collect();
        images.sort();
        images.dedup();
        let sentinel = live.docker(&["exec", container, "sha256sum", "/data/sentinel"]);
        json!({"containerId": live.docker(&["inspect", "--format", "{{.Id}}", container]).trim(),
            "volume": volume_info[0], "sentinelSHA256": sentinel.split_whitespace().next().expect("digest"),
            "images": images})
    };
    live.call(&["vm", "create"], &["--yes", "--cpu", "2", "--memory", "2", "--disk", "24"]);
    *created = true;
    let config = live.profile().join("config.yaml");
    config_bool(&config, "mountHome", false);
    config_bool(&config, "rosetta", false);
    *stage = "initial-boot";
    let started = Instant::now();
    let boot = live.call(&["vm", "start"], &["--yes"]);
    let status = live.call(&["vm", "status"], &[]);
    assert!(
        boot["dockerStatus"] == "ready" && status["mountHome"] == false && status["rosetta"] == false,
        "{boot} {status}"
    );
    evidence.mark("boot", json!({"elapsedSeconds": started.elapsed().as_secs_f64(), "status": status}));

    *stage = "container-runtimes";
    let versions =
        live.ssh("systemctl is-active containerd docker\ncontainerd --version\nrunc --version\nctr namespaces list -q");
    assert!(versions.contains("moby"), "{versions}");
    let qemu = live.ssh("cat /proc/sys/fs/binfmt_misc/qemu-x86_64\ntest ! -e /proc/sys/fs/binfmt_misc/hamn-rosetta");
    assert!(qemu.starts_with("enabled\n") && qemu.contains("qemu"), "{qemu}");
    live.docker(&["pull", "--platform=linux/arm64", "busybox:1.37"]);
    let arm = live.docker(&["image", "inspect", "--format", "{{.Id}}", "busybox:1.37"]).trim().to_owned();
    assert_eq!(live.docker(&["run", "--rm", "--platform=linux/arm64", &arm, "uname", "-m"]).trim(), "aarch64");
    live.docker(&["pull", "--platform=linux/amd64", "busybox:1.37"]);
    let amd = live.docker(&["image", "inspect", "--format", "{{.Id}}", "busybox:1.37"]).trim().to_owned();
    assert_eq!(live.docker(&["run", "--rm", "--platform=linux/amd64", &amd, "uname", "-m"]).trim(), "x86_64");
    live.docker(&["tag", &arm, "busybox:1.37"]);
    evidence.mark(
        "arm64-and-qemu-amd64",
        json!({"armImageId": arm, "amdImageId": amd, "qemuRegistration": qemu, "runtimeVersions": versions}),
    );
    let label = format!("io.hamn.test={token}");
    live.docker(&["volume", "create", "--label", &label, volume]);
    let mount = format!("type=volume,src={volume},dst=/data");
    live.docker(&["run", "--rm", "--mount", &mount, &arm, "sh", "-c", &format!("printf %s {token} > /data/sentinel")]);
    let sentinel_id = live
        .docker(&[
            "run",
            "-d",
            "--name",
            container,
            "--restart",
            "always",
            "--label",
            &label,
            "--mount",
            &mount,
            &arm,
            "sh",
            "-c",
            "while :; do sleep 30; done",
        ])
        .trim()
        .to_owned();
    let rows = live.call(&["docker", "containers", "list"], &[]);
    assert!(rows.as_array().is_some_and(|rows| rows.iter().any(|row| row["Id"] == sentinel_id.as_str())), "{rows}");
    let tasks = live.ssh("ctr -n moby tasks list");
    assert!(tasks.contains(&sentinel_id), "{tasks}");
    assert_eq!(live.docker(&["inspect", "--format", "{{.HostConfig.Runtime}}", container]).trim(), "runc");
    evidence.mark(
        "docker-api-containerd-runc",
        json!({"containerId": sentinel_id, "containerdTasks": tasks, "dockerVersion": live.docker(&["version"]),
            "apiRows": rows}),
    );

    *stage = "compose-buildx";
    cli_extensions(live);
    let cli_versions = fs::read_to_string(live.root.join("cli-versions.txt")).unwrap();
    evidence.mark("compose-buildx", json!({"versions": cli_versions}));

    *stage = "cni";
    let cni = live.ssh(&cni_script(token, &arm));
    assert!(cni.contains(&format!("CNI_ADD_PING_DEL_OK hp-{token}")), "{cni}");
    evidence.mark("cni-bridge-host-local-loopback", json!({"output": cni}));

    *stage = "stop-start-preservation";
    let before = state();
    live.call(&["vm", "stop"], &["--yes"]);
    live.call(&["vm", "start"], &["--yes"]);
    assert_eq!(state(), before, "stop/start changed Docker state");
    evidence.mark("stop-start-preservation", before.clone());
    // This boot starts with the deployed guest configuration already in place,
    // so an image unit that conflicts with it fails here, not on first boot.
    let failed = live.ssh("systemctl --failed --no-legend --plain");
    assert!(failed.trim().is_empty(), "failed systemd units after restart:\n{failed}");
    evidence.mark("no-failed-units-after-restart", json!({"failedUnits": failed}));

    *stage = "rosetta";
    live.call(&["vm", "stop"], &["--yes"]);
    config_bool(&config, "rosetta", true);
    if let Err(error) = live.runtime.call(&["vm", "start"], PROFILE, &["--yes"]) {
        evidence.value["checks"]["rosetta"] =
            json!({"passed": false, "error": error, "systemInstallationAttempted": false});
        panic!("{error}");
    }
    let rosetta = live.ssh("cat /proc/sys/fs/binfmt_misc/hamn-rosetta\ntest ! -e /proc/sys/fs/binfmt_misc/qemu-x86_64");
    assert!(rosetta.starts_with("enabled\n") && rosetta.contains("/mnt/hamn-rosetta/rosetta"), "{rosetta}");
    assert_eq!(live.docker(&["run", "--rm", "--platform=linux/amd64", &amd, "uname", "-m"]).trim(), "x86_64");
    assert_eq!(state(), before, "Rosetta start changed Docker state");
    evidence.mark("rosetta", json!({"passed": true, "registration": rosetta, "systemInstallationAttempted": false}));
    let tools = live.ssh(
        "for tool in gcc make; do if command -v \"$tool\"; then \"$tool\" --version | head -n 1; else printf \"%s absent\\n\" \"$tool\"; fi; done",
    );
    let object = evidence.object();
    object.insert("buildTools".into(), json!(tools));
    object.insert("passed".into(), json!(true));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::tmp::TempDir;

    fn write(path: &Path, value: &Value) {
        fs::write(path, value.to_string()).unwrap();
    }

    #[test]
    fn inputs_bind_every_image_to_one_revision_and_its_reports() {
        let directory = TempDir::new("hamn-image-inputs-");
        let root = directory.path();
        let revision = "a".repeat(40);
        let image = |name: &str, bytes: &[u8]| {
            fs::write(root.join(name), bytes).unwrap();
            (root.join(name), sha256_file(&root.join(name)).unwrap())
        };
        let (baseline, baseline_sha) = image("baseline.img", b"baseline");
        let (optimized, optimized_sha) = image("optimized.img", b"optimized!");
        let report = root.join("size-report.json");
        write(
            &report,
            &json!({"sourceRevision": revision, "virtualBytes": 8u64 << 30, "baselineSha256": baseline_sha,
            "baselineCompressedBytes": 8, "imageSha256": optimized_sha, "compressedBytes": 10}),
        );
        let mut variants = Vec::new();
        for case in 0..2 {
            let (_, sha) = image(&format!("case-{case}.img"), format!("case {case}").as_bytes());
            let detail = root.join(format!("case-{case}.json"));
            write(
                &detail,
                &json!({"sourceRevision": revision, "baselineSha256": baseline_sha, "imageSha256": sha,
                "compressedBytes": 6}),
            );
            variants.push(json!({"case": case, "image": format!("case-{case}.img"), "imageSha256": sha,
                "sizeReport": format!("case-{case}.json"), "structuralChecks": "passed"}));
        }
        let variations = root.join("variations.json");
        let valid = json!({"sourceRevision": revision, "baselineSha256": baseline_sha,
            "baselineSizeReportSha256": sha256_file(&report).unwrap(), "variants": variants});
        write(&variations, &valid);
        let options = Options {
            binary: PathBuf::from("/nonexistent"),
            size_report: report.clone(),
            variations: variations.clone(),
            source_revision: revision.clone(),
            output: root.join("out"),
        };
        let images = inputs(&options, &baseline, &optimized).unwrap();
        let labels: Vec<&str> = images.iter().map(|image| image.label.as_str()).collect();
        assert_eq!(labels, ["baseline", "optimized", "case-0", "case-1"]);
        assert_eq!(images[3].path, root.join("case-1.img"));

        let rejected = |options: &Options, message: &str| {
            let error = inputs(options, &baseline, &optimized).unwrap_err();
            assert!(error.contains(message), "{error}");
        };
        rejected(&Options { source_revision: "b".repeat(40), ..options.clone() }, "another revision");
        rejected(&Options { source_revision: revision.to_uppercase(), ..options.clone() }, "another revision");
        fs::write(root.join("case-1.img"), b"case 2").unwrap();
        rejected(&options, "image identity mismatch: case-1");
        fs::write(root.join("case-1.img"), b"case 1").unwrap();
        let mut changed = valid.clone();
        changed["variants"][1]["structuralChecks"] = json!("failed");
        write(&variations, &changed);
        rejected(&options, "structural checks");
        let mut changed = valid.clone();
        changed["baselineSizeReportSha256"] = json!("0".repeat(64));
        write(&variations, &changed);
        rejected(&options, "another size report");
        write(&variations, &valid);
        fs::write(&optimized, b"optimized?").unwrap();
        rejected(&options, "image identity mismatch: optimized");
    }

    #[test]
    fn config_bool_replaces_exactly_one_setting() {
        let directory = TempDir::new("hamn-image-config-");
        let config = directory.path().join("config.yaml");
        fs::write(&config, "cpu: 2\nrosetta: true\nmountHome: true\n").unwrap();
        config_bool(&config, "rosetta", false);
        assert_eq!(fs::read_to_string(&config).unwrap(), "cpu: 2\nrosetta: false\nmountHome: true\n");
        fs::write(&config, "rosetta: true\nrosetta: false\n").unwrap();
        assert!(panic::catch_unwind(|| config_bool(&config, "rosetta", false)).is_err());
    }

    #[test]
    fn the_cni_script_is_valid_bash_and_names_only_its_token() {
        let token = "0123456789abcdef0123456789abcdef";
        let script = cni_script(token, &format!("sha256:{}", "e".repeat(64)));
        assert!(script.contains("namespace=hp-0123456789abcdef0123456789abcdef\n"));
        assert!(script.contains("ping -c 2 -W 2 10.237.17.1"), "16 + 0x01 % 220");
        let directory = TempDir::new("hamn-cni-script-");
        let path = directory.path().join("cni.sh");
        fs::write(&path, &script).unwrap();
        let status = std::process::Command::new("/bin/bash").arg("-n").arg(&path).status().unwrap();
        assert!(status.success());
        assert!(panic::catch_unwind(|| cni_script("not-a-token", "sha256:x")).is_err());
    }
}
