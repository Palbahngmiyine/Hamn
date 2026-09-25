//! Opt-in Apple Silicon integration of a candidate executable with a real
//! VM, Docker Engine, Compose/buildx and a disposable kind cluster
//! (`workspace-live`), the Kubernetes management review against a kept
//! root (`workspace-live-management`), and the VM-free checks of their
//! guards and fixtures (`workspace-live-checks`, run by `make test-control`).
//!
//! Ownership contract:
//! - A run owns a private root under /tmp: an isolated HOME,
//!   `ownership.json` (this checkout, profile `verify`, the HOME and the
//!   guest image digest) and `hamn-under-test`, a frozen copy of the
//!   candidate that a running VM may still execute.
//! - The user's ~/.hamn is only read: the signed guest image cache named by
//!   `--cache` is cloned into the root. `--root` resumes only a root this
//!   checkout created, and only for the same candidate bytes.
//! - Every child gets the runtime's environment (see `release::runtime`):
//!   no user Docker, Kubernetes or Hamn target leaks in. Its PATH holds the
//!   directories of the Docker CLI, kubectl and kind this process resolved,
//!   then Homebrew and the system directories, so the harness and the TUI
//!   under test run the same external CLIs.
//! - Cleanup stops the owned profiles and keeps the root for inspection;
//!   processes and Kubernetes objects are removed only after their identity
//!   (birth, API UID and ownership label) was checked again.
mod boundaries;
mod cancellation;
pub mod checks;
mod contexts;
mod kubernetes;
mod management;
mod processes;
mod terminal;
mod transport;

pub use contexts::exec_witness;
pub use management::main as management_main;

use crate::release::files::{read_json, sha256_file};
use crate::release::process::{self, Output, Spec};
use crate::release::runtime::Runtime;
use crate::support::exec::which;
use crate::support::py_text;
use regex::Regex;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::os::fd::RawFd;
use std::os::unix::fs::DirBuilderExt;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The profile every live check owns.
pub const PROFILE: &str = "verify";

/// Directories after the resolved CLIs' own: Homebrew, then the system.
const BASE_PATH: &str = "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin";

const USAGE: &str =
    "usage: hamn-dev test workspace-live [--root ROOT] [--cache CACHE] [--binary BINARY] [--keep-running]

Opt-in Apple Silicon integration. Owns an isolated HOME; never uses user contexts.

  --root ROOT      resume only a workspace-owned integration root
  --cache CACHE    signed guest image cache, only read (default ~/.hamn/cache)
  --binary BINARY  candidate executable (default build/hamn of this checkout)
  --keep-running   retain the owned VM for further tests";

pub fn main(args: &[String]) -> ExitCode {
    let flags = match Flags::parse(args, &["root", "cache", "binary"], &["keep-running"], USAGE) {
        Ok(Some(flags)) => flags,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("workspace-live: {message}");
            return ExitCode::from(2);
        }
    };
    let cache = flags.value("cache").map_or_else(default_cache, PathBuf::from);
    let binary = flags.value("binary").map_or_else(|| repository().join("build/hamn"), PathBuf::from);
    catch_interrupts();
    let prepared = fs::canonicalize(&binary)
        .map_err(|error| format!("{}: {error}", binary.display()))
        .and_then(|binary| prepare(&binary, Some(&cache), flags.value("root").map(Path::new)));
    let live = match prepared {
        Ok((root, runtime)) => Live { root, runtime },
        Err(message) => {
            eprintln!("workspace-live: {message}");
            return ExitCode::FAILURE;
        }
    };
    println!("Evidence and owned runtime: {}", live.root.display());
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        start_isolated(&live.runtime).must();
        recovery(&live);
        boundaries::cancellation_boundaries(&live);
        transport::transport_failure(&live);
        cli_extensions(&live);
        contexts::external_contexts(&live);
        cancellation::cancellation(&live);
        cancellation::owned_start_cancellation(&live);
        kubernetes::kubernetes(&live, true);
    }));
    let stopped = if flags.switch("keep-running") { Ok(()) } else { live.runtime.stop(&[PROFILE.to_owned()]) };
    finish("workspace-live", outcome.is_ok(), stopped)
}

/// The exit status of a live suite whose body passed or failed (a failure
/// was already reported by the panic hook) and whose cleanup ran.
fn finish(suite: &str, passed: bool, cleanup: Result<(), String>) -> ExitCode {
    if let Err(error) = &cleanup {
        eprintln!("{suite}: {error}");
    }
    if interrupted() {
        ExitCode::from(130)
    } else if passed && cleanup.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn default_cache() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from(".hamn/cache"), |home| Path::new(&home).join(".hamn/cache"))
}

/// This checkout: the repository the harness was built from.
pub(crate) fn repository() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::canonicalize(manifest.join("../..")).unwrap_or_else(|error| panic!("{}: {error}", manifest.display()))
}

/// An integration root and the runtime that drives its frozen candidate.
pub(crate) struct Live {
    pub root: PathBuf,
    pub runtime: Runtime,
}

impl Live {
    pub(crate) fn profile(&self) -> PathBuf {
        self.runtime.profile_dir(PROFILE)
    }

    #[track_caller]
    pub(crate) fn call(&self, words: &[&str], flags: &[&str]) -> Value {
        self.runtime.call(words, PROFILE, flags).must()
    }

    /// The external Docker CLI on the owned profile's socket.
    #[track_caller]
    pub(crate) fn docker(&self, words: &[&str]) -> String {
        self.runtime.engine(words, PROFILE).must()
    }

    /// A root script in the owned guest.
    #[track_caller]
    pub(crate) fn ssh(&self, script: &str) -> String {
        self.runtime.ssh(script, PROFILE).must()
    }

    #[track_caller]
    pub(crate) fn state(&self) -> String {
        self.call(&["vm", "status"], &[])["state"].as_str().unwrap_or_default().to_owned()
    }

    /// The raw `vmrun.pid` of the owned VM, compared to prove it survived.
    pub(crate) fn vm_pid(&self) -> Vec<u8> {
        let path = self.profile().join("vmrun.pid");
        fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    pub(crate) fn write_json(&self, name: &str, value: &Value) {
        write_json(&self.root.join(name), value);
    }

    /// Checks the ownership record again before changing the VM: this
    /// checkout, profile `verify`, this runtime's HOME and frozen candidate.
    pub(crate) fn assert_owned(&self) {
        let owner = read_json(&self.root.join("ownership.json")).must();
        assert_eq!(owner["profile"], PROFILE, "the root does not own profile {PROFILE}");
        let home = resolved(Path::new(owner["home"].as_str().unwrap_or_default()));
        assert!(
            home == resolved(&self.runtime.home) && home == resolved(&self.root.join("home")),
            "the runtime HOME is not the root's owned HOME"
        );
        let workspace = resolved(Path::new(owner["workspace"].as_str().unwrap_or_default()));
        assert_eq!(workspace, repository(), "the root belongs to another checkout");
        assert_eq!(
            resolved(&self.runtime.binary),
            resolved(&self.root.join("hamn-under-test")),
            "the runtime does not run the root's frozen candidate"
        );
    }

    /// This runtime's environment plus `extra`.
    pub(crate) fn environment(&self, extra: &[(&str, &str)]) -> BTreeMap<String, String> {
        let mut environment = self.runtime.environment.clone();
        for (key, value) in extra {
            environment.insert((*key).to_owned(), (*value).to_owned());
        }
        environment
    }
}

/// `path` with symbolic links resolved when it exists (Python's
/// `Path.resolve()` for existing paths), else unchanged.
pub(crate) fn resolved(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Prepares a new integration root from the signed cache, or resumes
/// `existing`, and returns it with a runtime for its frozen candidate.
/// Only the cache is read outside the root.
pub(crate) fn prepare(
    binary: &Path,
    cache: Option<&Path>,
    existing: Option<&Path>,
) -> Result<(PathBuf, Runtime), String> {
    let repository = repository();
    let root = match existing {
        Some(existing) => {
            let root = std::path::absolute(existing).map_err(|error| format!("{}: {error}", existing.display()))?;
            let owner = read_json(&root.join("ownership.json"))?;
            if owner["workspace"].as_str() != repository.to_str() || owner["profile"] != PROFILE {
                return Err(format!("{} is not a workspace-owned integration root", root.display()));
            }
            let home = owner["home"].as_str().ok_or("the ownership record names no HOME")?;
            if resolved(Path::new(home)) != resolved(&root.join("home")) {
                return Err(format!("{} does not own its HOME", root.display()));
            }
            root
        }
        None => {
            let cache = cache.ok_or("a new integration root needs the signed guest image cache")?;
            // Copy only the selected, locally verified signed image; start
            // verifies it again. The cache is checked before a root exists.
            let manifest = read_json(&cache.join("guest-image.json"))?;
            let expected = manifest["sha256"].as_str().unwrap_or_default();
            if !is_lower_hex(expected, 64) {
                return Err("the cache manifest has no SHA-256 image digest".into());
            }
            let name = format!("hamn-guest-{expected}.img");
            let image = cache.join(&name);
            let marker = cache.join(format!("{name}.verified"));
            let image_sha256 = sha256_file(&image)?;
            if manifest["file"] != name.as_str() || image_sha256 != expected || !marker.is_file() {
                return Err("the cache does not hold the verified image its manifest selects".into());
            }
            let root = mkdtemp(Path::new("/tmp"), "hamn-workspace-live-")?;
            let home = root.join("home");
            let target = home.join(".hamn/cache");
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&target)
                .map_err(|error| format!("{}: {error}", target.display()))?;
            for source in [&image, &marker, &cache.join("guest-image.json")] {
                let copy = target.join(source.file_name().expect("file name"));
                process::run(
                    OsStr::new("/bin/cp"),
                    &[OsStr::new("-c"), source.as_os_str(), copy.as_os_str()],
                    &Spec::default(),
                    Duration::from_secs(660),
                )?;
            }
            let owner = json!({"owner": "Hamn workspace integration test", "workspace": repository,
                "profile": PROFILE, "home": home, "guestImageSha256": image_sha256});
            write_json(&root.join("ownership.json"), &owner);
            root
        }
    };
    let frozen = root.join("hamn-under-test");
    // A running VM may still execute this inode, and even an identical
    // in-place copy invalidates macOS code-signing state. A root stays
    // bound to its first candidate; another candidate needs a fresh root.
    if frozen.exists() {
        if sha256_file(binary)? != sha256_file(&frozen)? {
            return Err("validation root belongs to a different candidate; use a fresh root".into());
        }
    } else {
        fs::copy(binary, &frozen).map_err(|error| format!("{}: {error}", frozen.display()))?;
    }
    let digest = sha256_file(&frozen)?;
    if sha256_file(binary)? != digest {
        return Err("the frozen candidate differs from the candidate".into());
    }
    fs::write(root.join("binary-sha256.txt"), format!("{digest}\n")).map_err(|error| error.to_string())?;
    let docker = which("docker");
    let tools = [docker.clone(), which("kubectl"), which("kind")];
    let mut directories: Vec<String> = Vec::new();
    for directory in tools.iter().flatten().filter_map(|tool| tool.parent()) {
        let directory = directory.to_string_lossy().into_owned();
        if !directories.contains(&directory) {
            directories.push(directory);
        }
    }
    directories.push(BASE_PATH.to_owned());
    // Without a Docker CLI on PATH, engine calls fail when first used.
    let mut runtime = Runtime::new(&frozen, root.join("home"), docker.unwrap_or_else(|| PathBuf::from("docker")));
    runtime.environment.insert("PATH".into(), directories.join(":"));
    runtime.environment.insert("TERM".into(), "xterm-256color".into());
    let docker_config = runtime.home.join(".docker");
    fs::create_dir_all(&docker_config).map_err(|error| format!("{}: {error}", docker_config.display()))?;
    let config = json!({"cliPluginsExtraDirs": ["/opt/homebrew/lib/docker/cli-plugins"]});
    fs::write(docker_config.join("config.json"), config.to_string()).map_err(|error| error.to_string())?;
    Ok((root, runtime))
}

/// What `start_isolated` needs of a runtime, so its guards can be checked
/// against a recorded fake.
pub(crate) trait Headless {
    fn home(&self) -> &Path;
    fn call(&self, words: &[&str], profile: &str, flags: &[&str]) -> Result<Value, String>;
}

impl Headless for Runtime {
    fn home(&self) -> &Path {
        &self.home
    }

    fn call(&self, words: &[&str], profile: &str, flags: &[&str]) -> Result<Value, String> {
        Runtime::call(self, words, profile, flags)
    }
}

/// Creates the validation profile when missing, disables home sharing while
/// it is stopped, then starts it. A running profile that shares the home
/// directory is never changed.
pub(crate) fn start_isolated(runtime: &impl Headless) -> Result<Value, String> {
    let config = runtime.home().join(".hamn/verify/config.yaml");
    if !config.exists() {
        runtime.call(&["vm", "create"], PROFILE, &["--yes", "--cpu", "4", "--memory", "6", "--disk", "60"])?;
    }
    let text = fs::read_to_string(&config).map_err(|error| format!("{}: {error}", config.display()))?;
    let setting = Regex::new(r"(?m)^mountHome: (true|false)$").expect("valid pattern");
    let values: Vec<&str> = setting.captures_iter(&text).map(|found| found.get(1).expect("group").as_str()).collect();
    if values.len() != 1 {
        return Err("missing or ambiguous home-sharing configuration".into());
    }
    if values[0] == "true" {
        let status = runtime.call(&["vm", "status"], PROFILE, &[])?;
        if status["state"] != "stopped" {
            return Err("refusing to change a running validation profile".into());
        }
        let enabled = Regex::new(r"(?m)^mountHome: true$").expect("valid pattern");
        fs::write(&config, enabled.replace_all(&text, "mountHome: false").as_bytes())
            .map_err(|error| format!("{}: {error}", config.display()))?;
    }
    let result = runtime.call(&["vm", "start"], PROFILE, &["--yes"])?;
    if runtime.call(&["vm", "status"], PROFILE, &[])?["mountHome"] != false {
        return Err("the validation profile still shares the home directory".into());
    }
    Ok(result)
}

/// The Docker state a check must preserve: containers, images, volumes and
/// the sentinel volume's data.
pub(crate) fn snapshot(live: &Live) -> Value {
    let fields = |text: String| -> Vec<String> { py_text::split(&text).into_iter().map(str::to_owned).collect() };
    let mut containers = fields(live.docker(&["ps", "-aq", "--no-trunc"]));
    containers.sort();
    let images: BTreeSet<String> = fields(live.docker(&["images", "-q", "--no-trunc"])).into_iter().collect();
    let mut volumes = fields(live.docker(&["volume", "ls", "-q"]));
    volumes.sort();
    let data = fields(live.docker(&["exec", "hamn-workspace-sentinel", "sha256sum", "/data/sentinel"]));
    json!({"containers": containers, "images": images, "volumes": volumes,
        "data": data.first().expect("sentinel digest")})
}

/// A deployment backup left by an interrupted transaction is rolled back
/// before the running VM's Docker socket is repaired, and a real stop and
/// start keep every container, image, volume and the sentinel data.
fn recovery(live: &Live) {
    let names = live.docker(&["ps", "-a", "--format", "{{.Names}}"]);
    if !py_text::split(&names).contains(&"hamn-workspace-sentinel") {
        live.docker(&["pull", "busybox:1.37"]);
        live.docker(&["volume", "create", "--label", "io.hamn.test=workspace", "hamn-workspace-data"]);
        live.docker(&[
            "run",
            "--rm",
            "-v",
            "hamn-workspace-data:/data",
            "busybox:1.37",
            "sh",
            "-c",
            r#"printf "hamn-preservation-proof-20260907\n" > /data/sentinel"#,
        ]);
        live.docker(&[
            "run",
            "-d",
            "--name",
            "hamn-workspace-sentinel",
            "--restart",
            "always",
            "--label",
            "io.hamn.test=workspace",
            "-v",
            "hamn-workspace-data:/data",
            "busybox:1.37",
            "sh",
            "-c",
            "while :; do sleep 30; done",
        ]);
    }
    let before = snapshot(live);
    live.write_json("before.json", &before);
    let pid = live.vm_pid();
    let token = "c".repeat(32);
    live.ssh(&format!(
        "flock /run/hamn-deployment.lock bash /usr/local/libexec/hamn/guest-deployment-transaction begin {token}"
    ));
    let socket = live.profile().join("docker.sock");
    fs::remove_file(&socket).unwrap_or_else(|error| panic!("{}: {error}", socket.display()));
    let result = live.call(&["vm", "start"], &["--yes"]);
    assert!(
        result["dockerStatus"] == "ready" && result["lastOperation"]["status"] == "completed",
        "socket recovery did not complete: {result}"
    );
    assert_eq!(result["lastOperation"]["startedVm"], false, "socket recovery restarted the VM: {result}");
    assert_eq!(live.vm_pid(), pid, "socket recovery replaced the VM");
    live.ssh(&format!("test ! -e /var/lib/hamn/deployment-transactions/{token}"));
    assert_eq!(snapshot(live), before, "socket recovery changed Docker data");
    println!("PASS: socket recovery and data preservation");
    live.write_json("recovery-results.json", &json!([{"status": result}]));
    live.call(&["vm", "stop"], &["--yes"]);
    let restarted = live.call(&["vm", "start"], &["--yes"]);
    assert_eq!(restarted["dockerStatus"], "ready", "{restarted}");
    assert_eq!(snapshot(live), before, "stop/start changed Docker data");
    println!("PASS: real stop/start preserved container, image, volume and sentinel");
}

/// The installed Compose and buildx plugins against the isolated engine.
pub(crate) fn cli_extensions(live: &Live) {
    let build = live.root.join("build-context");
    fs::create_dir_all(&build).unwrap();
    fs::write(build.join("Dockerfile"), "FROM busybox:1.37\nRUN printf buildx-proof > /proof\n").unwrap();
    live.docker(&["buildx", "build", "--load", "-t", "hamn-workspace-build:verify", path_str(&build)]);
    let proof = live.docker(&["run", "--rm", "hamn-workspace-build:verify", "cat", "/proof"]);
    assert_eq!(py_text::strip(&proof), "buildx-proof");
    let compose = live.root.join("compose.yaml");
    fs::write(
        &compose,
        "services:\n  probe:\n    image: busybox:1.37\n    command: [\"sh\", \"-c\", \"echo compose-proof; sleep 300\"]\n",
    )
    .unwrap();
    let project = ["compose", "-p", "hamn-workspace-proof", "-f", path_str(&compose)];
    let compose_run = |words: &[&str]| live.docker(&[&project[..], words].concat());
    let mut nothing = ();
    finally(
        &mut nothing,
        |_| {
            compose_run(&["up", "-d"]);
            assert!(compose_run(&["logs"]).contains("compose-proof"), "Compose logs lack the service output");
        },
        |_| {
            compose_run(&["down"]);
        },
    );
    let versions =
        live.docker(&["version"]) + &live.docker(&["compose", "version"]) + &live.docker(&["buildx", "version"]);
    fs::write(live.root.join("cli-versions.txt"), versions).unwrap();
    println!("PASS: installed Compose and buildx against isolated Docker");
}

/// Parsed `--name VALUE`, `--name=VALUE` and `--switch` options.
pub(crate) struct Flags {
    values: BTreeMap<String, String>,
    switches: Vec<String>,
}

impl Flags {
    /// Parses `args`: `valued` and `switches` name the accepted options
    /// without dashes. `-h`/`--help` prints `usage` and returns `None`;
    /// anything else is an error carrying `usage`.
    pub(crate) fn parse(
        args: &[String],
        valued: &[&str],
        switches: &[&str],
        usage: &str,
    ) -> Result<Option<Self>, String> {
        if args.iter().any(|arg| arg == "-h" || arg == "--help") {
            println!("{usage}");
            return Ok(None);
        }
        let mut flags = Self { values: BTreeMap::new(), switches: Vec::new() };
        let mut rest = args.iter();
        while let Some(arg) = rest.next() {
            let Some(option) = arg.strip_prefix("--") else {
                return Err(format!("unexpected argument {arg:?}\n{usage}"));
            };
            let (name, inline) = match option.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (option, None),
            };
            if valued.contains(&name) {
                let value = match inline {
                    Some(value) => value,
                    None => rest.next().cloned().ok_or_else(|| format!("--{name} needs a value\n{usage}"))?,
                };
                flags.values.insert(name.to_owned(), value);
            } else if switches.contains(&name) && inline.is_none() {
                flags.switches.push(name.to_owned());
            } else {
                return Err(format!("unknown option {arg:?}\n{usage}"));
            }
        }
        Ok(Some(flags))
    }

    pub(crate) fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub(crate) fn switch(&self, name: &str) -> bool {
        self.switches.iter().any(|switch| switch == name)
    }
}

/// A failed runtime step fails the live check at the caller's line.
pub(crate) trait Must<T> {
    fn must(self) -> T;
}

impl<T> Must<T> for Result<T, String> {
    #[track_caller]
    fn must(self) -> T {
        self.unwrap_or_else(|error| panic!("{error}"))
    }
}

/// Runs `body`, then `cleanup` whether or not `body` panicked (Python's
/// try/finally): a panic in `body` resumes after `cleanup`. A panic in
/// `cleanup` propagates; the body's failure was already reported.
pub(crate) fn finally<S, T>(state: &mut S, body: impl FnOnce(&mut S) -> T, cleanup: impl FnOnce(&mut S)) -> T {
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| body(state)));
    cleanup(state);
    outcome.unwrap_or_else(|payload| panic::resume_unwind(payload))
}

/// The message of a caught panic.
pub(crate) fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|text| (*text).to_owned()))
        .unwrap_or_else(|| "unknown panic".into())
}

/// Runs `program args` with exactly `environment` (a bare program is found
/// on its PATH) and `input` within `timeout`; a nonzero exit is an error
/// carrying both streams' tails.
pub(crate) fn run_env<S: AsRef<OsStr>>(
    program: &str,
    args: &[S],
    environment: &BTreeMap<String, String>,
    timeout: Duration,
    input: Option<&[u8]>,
) -> Result<String, String> {
    process::run(OsStr::new(program), args, &Spec { environment: Some(environment), input }, timeout)
}

/// Like [`run_env`], returning any exit status with both streams.
pub(crate) fn capture_env<S: AsRef<OsStr>>(
    program: &OsStr,
    args: &[S],
    environment: &BTreeMap<String, String>,
    timeout: Duration,
) -> Output {
    process::capture(program, args, &Spec { environment: Some(environment), input: None }, timeout).must()
}

/// Waits up to `timeout` for `child` (with piped output) to exit, reading
/// its output; a child still running then is killed and fails the check.
pub(crate) fn communicate(child: &mut Child, timeout: Duration) -> (std::process::ExitStatus, String, String) {
    let mut captured = crate::support::bounded_process::Captured::default();
    let status = crate::support::bounded_process::communicate(child, &mut captured, timeout);
    let text = |data: &[u8]| String::from_utf8_lossy(data).into_owned();
    match status {
        Some(status) => (status, text(&captured.stdout), text(&captured.stderr)),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{} did not exit within {timeout:?}: {}", child.id(), text(&captured.stderr));
        }
    }
}

/// Whether our child is still running (not yet reaped).
pub(crate) fn running(child: &mut Child) -> bool {
    matches!(child.try_wait(), Ok(None))
}

/// Sends `signal` to our own unreaped child, whose PID cannot have been
/// reused.
pub(crate) fn signal_child(child: &mut Child, signal: i32) {
    if running(child) {
        crate::support::pty::kill(child.id(), signal);
    }
}

/// A JSON document as `json.dumps(value, indent=2)` writes it.
pub(crate) fn write_json(path: &Path, value: &Value) {
    let text = serde_json::to_string_pretty(value).expect("serializable JSON");
    fs::write(path, text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// The operation record, or `{}` while none exists.
pub(crate) fn read_record(path: &Path) -> Value {
    match fs::read(path) {
        Ok(data) => {
            serde_json::from_slice(&data).unwrap_or_else(|error| panic!("{}: invalid JSON: {error}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(error) => panic!("{}: {error}", path.display()),
    }
}

pub(crate) fn path_str(path: &Path) -> &str {
    path.to_str().unwrap_or_else(|| panic!("{} is not UTF-8", path.display()))
}

pub(crate) fn is_lower_hex(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// `bytes` random bytes in lowercase hex.
pub(crate) fn random_hex(bytes: usize) -> String {
    let mut data = vec![0u8; bytes];
    fs::File::open("/dev/urandom").and_then(|mut random| random.read_exact(&mut data)).expect("read /dev/urandom");
    data.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A random version-4 UUID in its dashed form.
pub(crate) fn uuid4() -> String {
    let mut hex: Vec<char> = random_hex(16).chars().collect();
    hex[12] = '4';
    hex[16] = ['8', '9', 'a', 'b'][hex[16].to_digit(16).expect("hex digit") as usize % 4];
    let text: String = hex.into_iter().collect();
    format!("{}-{}-{}-{}-{}", &text[..8], &text[8..12], &text[12..16], &text[16..20], &text[20..])
}

/// Creates a new private directory `<parent>/<prefix><random>` that is kept
/// (Python's `tempfile.mkdtemp`).
pub(crate) fn mkdtemp(parent: &Path, prefix: &str) -> Result<PathBuf, String> {
    for _ in 0..100 {
        let path = parent.join(format!("{prefix}{}", random_hex(4)));
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    }
    Err(format!("cannot create a unique directory in {}", parent.display()))
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Turns SIGINT into a flag, so an interrupted live run still stops its VM
/// (the Python harness's `finally` on KeyboardInterrupt). Children still
/// receive the terminal's SIGINT and fail, which ends the current step.
pub(crate) fn catch_interrupts() {
    // SAFETY: the handler only stores to an atomic, which is
    // async-signal-safe.
    unsafe { libc::signal(libc::SIGINT, on_interrupt as *const () as libc::sighandler_t) };
}

pub(crate) fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

#[track_caller]
pub(crate) fn check_interrupt() {
    assert!(!interrupted(), "interrupted");
}

/// Waits up to `timeout` until any of `fds` is readable, with select(2),
/// which also watches kqueue descriptors. Returns the readable ones; empty
/// after a timeout or a signal, so callers re-check deadlines and
/// interruption.
pub(crate) fn readable(fds: &[RawFd], timeout: Duration) -> Vec<RawFd> {
    for &fd in fds {
        assert!((0..libc::FD_SETSIZE as RawFd).contains(&fd), "descriptor {fd} out of select range");
    }
    let mut wait = libc::timeval {
        tv_sec: timeout.as_secs() as libc::time_t,
        tv_usec: timeout.subsec_micros() as libc::suseconds_t,
    };
    // SAFETY: fd_set is plain data; FD_ZERO and FD_SET initialize it for
    // descriptors below FD_SETSIZE.
    let mut set: libc::fd_set = unsafe { std::mem::zeroed() };
    unsafe { libc::FD_ZERO(&mut set) };
    for &fd in fds {
        // SAFETY: fd is below FD_SETSIZE (checked above).
        unsafe { libc::FD_SET(fd, &mut set) };
    }
    let highest = fds.iter().copied().max().unwrap_or(0);
    // SAFETY: select reads and writes only the set and the timeval.
    let result = unsafe { libc::select(highest + 1, &mut set, std::ptr::null_mut(), std::ptr::null_mut(), &mut wait) };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted, "select: {error}");
        return Vec::new();
    }
    // SAFETY: the set was filled in by select above.
    fds.iter().copied().filter(|&fd| unsafe { libc::FD_ISSET(fd, &set) }).collect()
}
