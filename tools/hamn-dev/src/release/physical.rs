//! `release physical-e2e`: runs the exact candidate bytes on a physical
//! Apple Silicon Mac in an isolated HOME and writes evidence only when every
//! physical check in [`CHECKS`] passed.
//!
//! No user VM or kubeconfig is changed: profiles live in a private
//! workspace under /private/tmp, which is removed only after every owned
//! profile was proven stopped.
use super::archive::unpack;
use super::contract::{CHECKS, physical_evidence, validate_physical};
use super::files::{canonical_json, owned_regular, read_json_limited, sha256_file, write, write_new};
use super::kubernetes;
use super::process::{self, Spec};
use super::runtime::{Runtime, which};
use crate::build_host::check_host_binary;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const USAGE: &str = "usage: hamn-dev release physical-e2e

Run exact candidate bytes; successful evidence requires every physical check.

Requires HAMN_CANDIDATE_DIR, HAMN_E2E_OUTPUT, HAMN_E2E_CONTEXT and
HAMN_E2E_KUBECONFIG. HAMN_E2E_K8S_HOST_NETWORK=1 runs the Kubernetes checks
with host networking (default 0). No user VM or kubeconfig is changed; all
profiles are created in an isolated temporary HOME.";

/// The validator's inputs, read from the environment before any action.
#[derive(Debug, PartialEq, Eq)]
pub struct Config {
    pub candidate_dir: PathBuf,
    pub output: PathBuf,
    pub context: String,
    pub kubeconfig: PathBuf,
    pub host_network: bool,
    pub run: String,
    pub attempt: String,
}

impl Config {
    /// Validates the inputs; `lookup` reads one environment variable.
    pub fn from_env(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let host_network = match lookup("HAMN_E2E_K8S_HOST_NETWORK").as_deref().unwrap_or("0") {
            "0" => false,
            "1" => true,
            _ => return Err("HAMN_E2E_K8S_HOST_NETWORK must be 0 or 1".into()),
        };
        let required = ["HAMN_CANDIDATE_DIR", "HAMN_E2E_OUTPUT", "HAMN_E2E_CONTEXT", "HAMN_E2E_KUBECONFIG"];
        let missing: Vec<&str> =
            required.into_iter().filter(|key| lookup(key).is_none_or(|value| value.is_empty())).collect();
        if !missing.is_empty() {
            return Err(format!("missing physical validator inputs: {}", missing.join(", ")));
        }
        let value = |key: &str| lookup(key).expect("checked above");
        Ok(Self {
            candidate_dir: value("HAMN_CANDIDATE_DIR").into(),
            output: value("HAMN_E2E_OUTPUT").into(),
            context: value("HAMN_E2E_CONTEXT"),
            kubeconfig: value("HAMN_E2E_KUBECONFIG").into(),
            host_network,
            run: lookup("GITHUB_RUN_ID").unwrap_or_else(|| "local".into()),
            attempt: lookup("GITHUB_RUN_ATTEMPT").unwrap_or_else(|| "local".into()),
        })
    }
}

pub fn main(args: &[String]) -> Result<(), String> {
    match args {
        [] => {}
        [help] if help == "-h" || help == "--help" => {
            println!("{USAGE}");
            return Ok(());
        }
        _ => return Err(format!("unrecognized arguments: {}\n{USAGE}", args.join(" "))),
    }
    let config = Config::from_env(|key| std::env::var(key).ok())?;
    run(&config)
}

/// A private workspace in /private/tmp: macOS TMPDIR normally lives under a
/// long /var/folders path, and profile control sockets must fit
/// `sockaddr_un.sun_path` (104 bytes on Darwin).
pub fn workspace() -> Result<PathBuf, String> {
    let parent = Path::new("/private/tmp");
    for attempt in 0..100u32 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.subsec_nanos());
        let path = parent.join(format!("hamn-e2e-{}-{nanos:08x}{attempt:02x}", std::process::id()));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    }
    Err("cannot create a unique physical validation workspace".into())
}

fn run(config: &Config) -> Result<(), String> {
    let machine = process::run(OsStr::new("uname"), &["-m"], &Spec::default(), Duration::from_secs(30))?;
    if machine.trim() != "arm64" {
        return Err("physical Apple Silicon validator required".into());
    }
    let path = std::env::var_os("PATH");
    let docker = which("docker", path.as_deref());
    let (Some(docker), Some(_)) = (docker, which("kubectl", path.as_deref())) else {
        return Err("external Docker CLI and kubectl fixture tools are required".into());
    };
    let candidate_dir =
        config.candidate_dir.canonicalize().map_err(|error| format!("{}: {error}", config.candidate_dir.display()))?;
    let candidate_path = candidate_dir.join("candidate.json");
    let candidate = read_json_limited(&candidate_path)?;
    let artifacts = super::contract::artifact_map(&candidate)?;
    for (name, digest) in &artifacts {
        let plain = Path::new(name).file_name() == Some(OsStr::new(name));
        if !plain || Some(sha256_file(owned_regular(&candidate_dir.join(name))?)?.as_str()) != digest.as_str() {
            return Err("candidate artifact digest mismatch".into());
        }
    }
    let find = |suffix: &str| artifacts.keys().find(|name| name.ends_with(suffix)).cloned();
    let (Some(host), Some(guest)) = (find("-darwin-arm64.tar.gz"), find("-ubuntu-24.04-arm64.img")) else {
        return Err("candidate lacks its host archive or guest image".into());
    };
    let output = std::path::absolute(&config.output).map_err(|error| error.to_string())?;
    if fs::symlink_metadata(&output).is_ok() {
        return Err("physical evidence output already exists".into());
    }
    let work = workspace()?;
    let mut state = Harness { runtime: None, profiles: Vec::new(), checks: BTreeSet::new() };
    let body = panic::catch_unwind(AssertUnwindSafe(|| {
        state.exercise(config, &work, &candidate_dir, &candidate, &host, &guest, &docker)
    }))
    .unwrap_or_else(|panic| Err(format!("physical validation panicked: {}", panic_message(&panic))));
    // Stop every owned profile before deleting the disks; a failed stop
    // keeps the workspace for inspection.
    if let Some(runtime) = &state.runtime
        && let Err(error) = runtime.stop(&state.profiles)
    {
        return Err(match body {
            Ok(_) => error,
            Err(failure) => format!("{failure}; {error}"),
        });
    }
    fs::remove_dir_all(&work).map_err(|error| format!("{}: {error}", work.display()))?;
    let kubernetes = body?;
    state.checks.insert("cleanup");
    if state.checks != BTreeSet::from(CHECKS) {
        return Err(format!("physical checks are incomplete: {:?}", state.checks));
    }
    let evidence = physical_evidence(
        &candidate,
        &sha256_file(&candidate_path)?,
        &sha256_file(&candidate_dir.join("SHA256SUMS"))?,
        &config.run,
        &config.attempt,
        kubernetes,
    )?;
    write_new(&output, canonical_json(&evidence).as_bytes())?;
    validate_physical(&candidate_path, &candidate_dir.join("SHA256SUMS"), &output, &config.run, &config.attempt)
        .map(drop)
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|message| message.to_string()))
        .unwrap_or_else(|| "unknown panic".into())
}

struct Harness {
    runtime: Option<Runtime>,
    profiles: Vec<String>,
    checks: BTreeSet<&'static str>,
}

impl Harness {
    /// Every physical check except cleanup; returns the Kubernetes evidence.
    #[allow(clippy::too_many_arguments)]
    fn exercise(
        &mut self,
        config: &Config,
        work: &Path,
        candidate_dir: &Path,
        candidate: &Value,
        host: &str,
        guest: &str,
        docker: &Path,
    ) -> Result<Value, String> {
        let unpacked = work.join("candidate");
        fs::create_dir(&unpacked).map_err(|error| error.to_string())?;
        let root = unpack(&candidate_dir.join(host), &unpacked)?;
        let binary = root.join("bin/hamn");
        owned_regular(&binary)?;
        check_host_binary(&binary)?;
        let version = process::run(binary.as_os_str(), &["--version"], &Spec::default(), Duration::from_secs(660))?;
        let expected = candidate["version"].as_str().and_then(|version| version.strip_prefix('v')).unwrap_or_default();
        if version.trim() != format!("hamn {expected}") {
            return Err("candidate executable version mismatch".into());
        }
        let binary_sha256 = sha256_file(&binary)?;
        self.checks.insert("singleBinary");

        let home = work.join("home");
        let cache = home.join(".hamn/cache");
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&cache).map_err(|error| error.to_string())?;
        let digest = candidate["artifacts"]
            .as_array()
            .and_then(|entries| entries.iter().find(|entry| entry["name"] == guest))
            .and_then(|entry| entry["sha256"].as_str())
            .ok_or("candidate guest image digest is missing")?
            .to_owned();
        let image = format!("hamn-guest-{digest}.img");
        process::run(
            OsStr::new("/bin/cp"),
            &[OsStr::new("-c"), candidate_dir.join(guest).as_os_str(), cache.join(&image).as_os_str()],
            &Spec::default(),
            Duration::from_secs(660),
        )?;
        write(&cache.join(format!("{image}.verified")), digest.as_bytes())?;
        write(
            &cache.join("guest-image.json"),
            json!({"schemaVersion": 1, "file": image, "sha256": digest}).to_string().as_bytes(),
        )?;
        let runtime = self.runtime.insert(Runtime::new(&binary, &home, docker));

        for profile in ["default", "second"] {
            runtime.call(&["vm", "create"], profile, &["--yes", "--cpu", "2", "--memory", "2"])?;
            self.profiles.push(profile.into());
            let config_path = runtime.profile_dir(profile).join("config.yaml");
            let text = fs::read_to_string(&config_path).map_err(|error| error.to_string())?;
            write(&config_path, text.replace("mountHome: true", "mountHome: false").as_bytes())?;
            runtime.call(&["vm", "start"], profile, &["--yes"])?;
            runtime.call(&["docker", "containers", "list"], profile, &[])?;
        }
        self.checks.extend(["multipleProfiles", "dockerWithoutCli", "vmLifecycle"]);

        runtime.engine(
            &["run", "-d", "--name", "hamn-e2e", "busybox:1.37", "sh", "-c", "echo hamn-e2e; sleep 3600"],
            "default",
        )?;
        for action in ["inspect", "logs", "stats", "stop", "start", "restart"] {
            let flags: &[&str] = if ["stop", "start", "restart"].contains(&action) { &["--yes"] } else { &[] };
            runtime.call(&["docker", "containers", action, "hamn-e2e"], "default", flags)?;
        }
        runtime.call(&["docker", "containers", "stop", "hamn-e2e"], "default", &["--yes"])?;
        runtime.call(&["docker", "containers", "delete", "hamn-e2e"], "default", &["--yes"])?;
        self.checks.extend(["dockerApi", "externalDockerSocket"]);

        runtime.terminal()?;
        if runtime.call(&["vm", "status"], "default", &[])?["state"] != "running" {
            return Err("the VM did not survive TUI exit".into());
        }
        self.checks.extend(["tuiTerminalRestore", "vmSurvivesTuiExit"]);
        runtime.call(&["vm", "stop"], "default", &["--yes"])?;
        runtime.call(&["vm", "start"], "default", &["--yes"])?;
        for profile in ["default", "second"] {
            runtime.call(&["vm", "stop"], profile, &["--yes"])?;
        }

        let kubernetes_path = work.join("kubernetes.json");
        let options = kubernetes::Options {
            hamn: binary.clone(),
            context: config.context.clone(),
            kubeconfig: Some(config.kubeconfig.clone()),
            output: kubernetes_path.clone(),
            host_network: config.host_network,
        };
        kubernetes::run_child(&options, Duration::from_secs(1200))?;
        let kubernetes = read_json_limited(&kubernetes_path)?;
        self.checks.extend(["externalKubernetes", "kubeconfigUnchanged"]);
        if sha256_file(&binary)? != binary_sha256 {
            return Err("candidate executable changed during validation".into());
        }
        Ok(kubernetes)
    }
}
