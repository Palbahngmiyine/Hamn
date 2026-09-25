//! The update notice and the scheduled checker of a real managed install,
//! observed on a PTY. Never rebuilds, runs a VM or contacts a release
//! service: the installed generation's manifest URL is an invalid offline
//! URL, so dispatch is observed through the checker's owned lock and cache
//! files. Native tests in control/upgrade.rs and control/install_support
//! cover the launcher's argv, environment and stdio, TTL/backoff boundaries,
//! failed refreshes, the automatic transfer deadline and the lock.
use crate::runner::{self, case};
use crate::support::pty::{self, Pty};
use crate::support::screen::RatatuiScreen;
use crate::support::termios;
use crate::support::tmp::TempDir;
use crate::support::tui::install_fixture;
use crate::support::upgrade::{self, write_json};
use serde_json::{Value, json};
use std::cell::Cell;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub fn main(filters: &[String]) -> ExitCode {
    let managed = Rc::new(Managed::new());
    let cases = [
        ("cached_notice_after_restore_and_24_hour_throttle", cached_notice_after_restore_and_24_hour_throttle as fn(&Home)),
        ("stale_cache_schedules_with_clean_environment_and_no_network", stale_cache_schedules_with_clean_environment_and_no_network),
        ("first_check_can_be_scheduled_before_cache_directory_exists", first_check_can_be_scheduled_before_cache_directory_exists),
        ("unsafe_cache_directory_blocks_notices_and_scheduling", unsafe_cache_directory_blocks_notices_and_scheduling),
        ("source_direct_generation_ci_opt_out_and_stderr_exclusions", source_direct_generation_ci_opt_out_and_stderr_exclusions),
        ("headless_and_non_tty_keep_original_results", headless_and_non_tty_keep_original_results),
        (
            "corrupt_future_unsafe_and_linked_check_cache_cannot_change_tui_result",
            corrupt_future_unsafe_and_linked_check_cache_cannot_change_tui_result,
        ),
        ("unsafe_notice_target_is_preserved", unsafe_notice_target_is_preserved),
    ]
    .into_iter()
    .map(|(name, test)| {
        let managed = Rc::clone(&managed);
        case(name, move || test(&Home::new(&managed)))
    })
    .collect();
    runner::run("update-check", "managed update notices and scheduled checks on a real PTY", cases, filters)
}

/// The recorded `docker`/`kubectl` of this suite: one running container,
/// `notice-fixture`, in the external Docker context.
pub fn cli_fixture(_program: &str, args: &[String]) -> ExitCode {
    let has = |value: &str| args.iter().any(|arg| arg == value);
    if has("context") {
        println!("{}", json!({"Name": "external", "DockerEndpoint": "unix:///unused", "Current": true}));
    } else if has("ps") {
        println!("{}", json!({"ID": "abcdef123456", "Names": "notice-fixture", "State": "running"}));
    } else if has("get") {
        println!("{{\"items\":[]}}");
    }
    ExitCode::SUCCESS
}

/// One managed install shared by every case.
struct Managed {
    root: PathBuf,
    frozen: PathBuf,
    latest: String,
    bindir: PathBuf,
    managed: PathBuf,
    tools: PathBuf,
    homes: Cell<usize>,
    _directory: TempDir,
}

impl Managed {
    fn new() -> Self {
        let directory = TempDir::new("hamn-check-");
        let root = fs::canonicalize(directory.path()).unwrap();
        let frozen = root.join("hamn-source");
        fs::copy(crate::support::hamn(), &frozen).unwrap();
        fs::set_permissions(&frozen, fs::Permissions::from_mode(0o755)).unwrap();
        let version = upgrade::run(Command::new(&frozen).arg("--version"), Duration::from_secs(5)).stdout();
        let version = version.split_whitespace().nth(1).unwrap_or_else(|| panic!("--version: {version:?}"));
        let major: u64 = version.trim_start_matches('v').split('.').next().unwrap().parse().unwrap();
        let (bindir, datadir) = (root.join("bin"), root.join("src"));
        let installed = upgrade::run(
            Command::new("bash")
                .arg(upgrade::checkout().join("scripts/install-host.sh"))
                .arg(&frozen)
                .arg(&bindir)
                .arg(&datadir)
                .env("HOME", &root),
            Duration::from_secs(30),
        );
        assert_eq!(installed.returncode, 0, "{}", installed.stderr());
        let managed = bindir.join("hamn");
        let generation = fs::canonicalize(&managed).unwrap().parent().and_then(Path::parent).unwrap().to_path_buf();
        fs::write(generation.join("share/hamn/src/packaging/release/update-manifest-url"), "invalid-offline-url\n").unwrap();
        let tools = root.join("tools");
        fs::create_dir(&tools).unwrap();
        for name in ["docker", "kubectl"] {
            install_fixture(&tools, name);
        }
        Self { latest: format!("{}.0.0", major + 1), root, frozen, bindir, managed, tools, homes: Cell::new(0), _directory: directory }
    }
}

/// A fresh HOME with a TUI preference for the external Docker context and a
/// fresh successful check record naming a newer release.
struct Home<'a> {
    managed: &'a Managed,
    home: PathBuf,
    runtime: PathBuf,
    cache: PathBuf,
    check_path: PathBuf,
    notice_path: PathBuf,
    now: u64,
    check_record: Value,
}

const NOTICE: &str = "is available; run hamn upgrade.";

impl<'a> Home<'a> {
    fn new(managed: &'a Managed) -> Self {
        managed.homes.set(managed.homes.get() + 1);
        let home = managed.root.join(format!("h{}", managed.homes.get()));
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let runtime = home.join(".hamn");
        fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
        let cache = runtime.join("cache");
        fs::DirBuilder::new().mode(0o755).create(&cache).unwrap();
        write_private(
            &runtime.join("tui.json"),
            &json!({"version": 1, "defaultWorkspace": "containers",
                "recentTargets": [{"kind": "docker", "name": "external", "config": null}]}),
            0o600,
        );
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let check_record = json!({"schemaVersion": 1, "checkedAt": now, "ok": true, "latestVersion": managed.latest});
        let check_path = cache.join("update-check-v1.json");
        write_private(&check_path, &check_record, 0o600);
        Self { managed, notice_path: cache.join("update-notice-v1.json"), home, runtime, cache, check_path, now, check_record }
    }

    fn stale_record(&self) -> Value {
        let mut record = self.check_record.clone();
        record["checkedAt"] = (self.now - 86460).into();
        record
    }

    fn command(&self, binary: &Path, extra: &[(&str, &str)]) -> Command {
        let mut command = Command::new(binary);
        command
            .env("HOME", &self.home)
            .env("TERM", "xterm-256color")
            .env("PATH", format!("{}:/usr/bin:/bin", self.managed.tools.display()))
            .env("KUBECONFIG", self.home.join("no-kubeconfig"))
            .env("HAMN_UPDATE_CHECK_TEST_SECRET", "must-not-reach-scheduler")
            .env("HAMN_DEV_FIXTURE", "update-check-cli")
            .env_remove("CI")
            .env_remove("HAMN_NO_UPDATE_CHECK");
        for (name, value) in extra {
            command.env(name, value);
        }
        command
    }

    /// Runs the TUI (or `arguments`) on a PTY and returns everything written
    /// to the terminal. `schedule` states whether a checker must have been
    /// dispatched; the negative case waits half a second for its lock file.
    fn run_tui(&self, mut command: Command, schedule: bool, stderr_tty: bool, arguments: &[&str]) -> Vec<u8> {
        let lock_path = self.cache.join(".update-check.lock");
        // Each invocation needs a new creation witness. A prior checker must
        // have released its actual cross-process lock before it is removed.
        if lock_path.exists() {
            let lock = fs::OpenOptions::new().read(true).write(true).open(&lock_path).unwrap();
            // SAFETY: flock only affects this open file description.
            assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0, "a checker still holds its lock");
            fs::remove_file(&lock_path).unwrap();
        }
        command.args(arguments);
        let output = terminal(command, stderr_tty, arguments.is_empty());
        let deadline = Instant::now() + if schedule { Duration::from_secs(5) } else { Duration::from_millis(500) };
        while !lock_path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(lock_path.exists(), schedule, "{}", tail(&output));
        if schedule {
            // Creating the lock precedes acquiring it. Observe the owned
            // executable's checker exit instead of racing that acquisition.
            let prefix = format!("{} __install-support upgrade schedule ", fs::canonicalize(&self.managed.managed).unwrap().display());
            loop {
                let processes = upgrade::run(Command::new("/bin/ps").arg("-axo").arg("args="), Duration::from_secs(2)).stdout();
                if !processes.lines().any(|line| line.starts_with(&prefix)) {
                    break;
                }
                assert!(Instant::now() < deadline, "the native checker did not finish");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        output
    }

    /// The managed command through its link.
    fn tui(&self, schedule: bool) -> Vec<u8> {
        self.run_tui(self.command(&self.managed.managed, &[]), schedule, true, &[])
    }
}

fn write_private(path: &Path, value: &Value, mode: u32) {
    write_json(path, value);
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn tail(output: &[u8]) -> String {
    String::from_utf8_lossy(&output[output.len().saturating_sub(2000)..]).into_owned()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn assert_no_notice(output: &[u8]) {
    assert!(!contains(output, NOTICE.as_bytes()), "{}", tail(output));
}

/// Runs `command` on a 30x130 PTY in a new session; an interactive TUI is
/// quit with `q` once the fixture container is on screen. The terminal's
/// modes must be restored.
fn terminal(mut command: Command, stderr_tty: bool, interactive: bool) -> Vec<u8> {
    let terminal = Pty::open(30, 130);
    let before = termios::settings(terminal.slave.as_raw_fd());
    let stream = || Stdio::from(terminal.slave.try_clone().expect("PTY slave"));
    command.stdin(stream()).stdout(stream()).stderr(if stderr_tty { stream() } else { Stdio::piped() });
    // SAFETY: setsid is async-signal-safe and touches no parent state.
    unsafe {
        command.pre_exec(|| if libc::setsid() < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
    }
    let child = command.spawn().expect("spawn under PTY");
    let mut child = crate::support::exec::Session(child);
    let master = terminal.master.as_raw_fd();
    let mut output = Vec::new();
    let mut screen = RatatuiScreen::new(30, 130);
    if interactive {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !screen.text().contains("notice-fixture") {
            let remaining = deadline.checked_duration_since(Instant::now()).unwrap_or_else(|| panic!("{}", screen.text()));
            assert!(!pty::readable(&[master], remaining).is_empty(), "{}", screen.text());
            let data = pty::read_some(master);
            assert!(!data.is_empty(), "{}", screen.text());
            output.extend_from_slice(&data);
            screen.feed(&data);
        }
        pty::write_all(master, b"q");
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        let remaining = deadline.checked_duration_since(Instant::now()).unwrap_or_else(|| panic!("{}", tail(&output)));
        if !pty::readable(&[master], remaining.min(Duration::from_millis(100))).is_empty() {
            output.extend(pty::read_some(master));
        }
    };
    assert!(status.success(), "{status:?}: {}", tail(&output));
    while !pty::readable(&[master], Duration::ZERO).is_empty() {
        let data = pty::read_some(master);
        if data.is_empty() {
            break;
        }
        output.extend(data);
    }
    if let Some(mut stderr) = child.0.stderr.take() {
        std::io::Read::read_to_end(&mut stderr, &mut output).unwrap();
    }
    assert_eq!(termios::settings(terminal.slave.as_raw_fd()), before, "the terminal modes were not restored");
    if interactive {
        assert!(contains(&output, b"\x1b[?1049l"), "{}", tail(&output));
    }
    output
}

fn cached_notice_after_restore_and_24_hour_throttle(home: &Home) {
    let output = home.tui(false);
    let notice = format!("Hamn {} {NOTICE}", home.managed.latest);
    let at = find(&output, notice.as_bytes()).unwrap_or_else(|| panic!("{}", tail(&output)));
    // The notice follows the restored main screen.
    let restored = output.windows(8).rposition(|window| window == b"\x1b[?1049l").unwrap();
    assert!(restored < at, "{}", tail(&output));
    let first = fs::read(&home.notice_path).unwrap();
    assert_eq!(fs::metadata(&home.notice_path).unwrap().permissions().mode() & 0o7777, 0o600);
    assert_no_notice(&home.tui(false));
    assert_eq!(fs::read(&home.notice_path).unwrap(), first);
    let mut value: Value = serde_json::from_slice(&first).unwrap();
    value["shownAt"] = (SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() - 86460).into();
    write_private(&home.notice_path, &value, 0o600);
    // `hamn` found through PATH identifies the same managed install.
    let path = format!("{}:{}:/usr/bin:/bin", home.managed.bindir.display(), home.managed.tools.display());
    let mut command = home.command(&home.managed.managed, &[("PATH", &path)]);
    command.arg0("hamn");
    let output = home.run_tui(command, false, true, &[]);
    assert!(contains(&output, notice.as_bytes()), "{}", tail(&output));
}

fn stale_cache_schedules_with_clean_environment_and_no_network(home: &Home) {
    write_private(&home.check_path, &home.stale_record(), 0o600);
    assert!(contains(&home.tui(true), b"run hamn upgrade."));
    let mut names: Vec<String> = fs::read_dir(&home.runtime).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["cache", "tui.json", "tui.lock"]);
}

fn first_check_can_be_scheduled_before_cache_directory_exists(home: &Home) {
    fs::remove_file(&home.check_path).unwrap();
    fs::remove_dir(&home.cache).unwrap();
    assert_no_notice(&home.tui(true));
}

fn unsafe_cache_directory_blocks_notices_and_scheduling(home: &Home) {
    write_private(&home.check_path, &home.stale_record(), 0o600);
    let original = fs::read(&home.check_path).unwrap();
    let external = home.home.join("outside-directory");
    fs::rename(&home.cache, &external).unwrap();
    std::os::unix::fs::symlink(&external, &home.cache).unwrap();
    assert_no_notice(&home.tui(false));
    assert!(!external.join("update-notice-v1.json").exists());
    assert_eq!(fs::read(external.join("update-check-v1.json")).unwrap(), original);
    fs::remove_file(&home.cache).unwrap();
    fs::rename(&external, &home.cache).unwrap();
    fs::set_permissions(&home.cache, fs::Permissions::from_mode(0o777)).unwrap();
    assert_no_notice(&home.tui(false));
    assert!(!home.notice_path.exists());
    assert_eq!(fs::read(&home.check_path).unwrap(), original);
}

fn source_direct_generation_ci_opt_out_and_stderr_exclusions(home: &Home) {
    // Stale data would cause both a notice and scheduling if eligibility leaks.
    write_private(&home.check_path, &home.stale_record(), 0o600);
    let direct = fs::canonicalize(&home.managed.managed).unwrap();
    let managed = &home.managed.managed;
    for (label, binary, extra, stderr_tty) in [
        ("source build", home.managed.frozen.as_path(), &[][..], true),
        ("direct generation", direct.as_path(), &[][..], true),
        ("CI", managed.as_path(), &[("CI", "1")][..], true),
        ("opt-out", managed.as_path(), &[("HAMN_NO_UPDATE_CHECK", "1")][..], true),
        ("stderr not a TTY", managed.as_path(), &[][..], false),
    ] {
        let output = home.run_tui(home.command(binary, extra), false, stderr_tty, &[]);
        assert!(!contains(&output, NOTICE.as_bytes()), "{label}: {}", tail(&output));
        assert!(!home.notice_path.exists(), "{label}");
    }
}

fn headless_and_non_tty_keep_original_results(home: &Home) {
    write_private(&home.check_path, &home.stale_record(), 0o600);
    let before = fs::read(&home.check_path).unwrap();
    // Both output descriptors stay on the terminal, so this checks the
    // headless exclusion independently of the non-TTY guard.
    let output = home.run_tui(home.command(&home.managed.managed, &[]), false, true, &["--headless", "capabilities"]);
    let text = String::from_utf8_lossy(&output).replace("\r\n", "\n");
    let envelope: Value = serde_json::from_str(text.trim()).unwrap_or_else(|error| panic!("{error}: {text}"));
    assert_eq!(envelope["ok"], true);
    assert_no_notice(&output);
    let result = upgrade::run(home.command(&home.managed.managed, &[]).args(["--headless", "capabilities"]), Duration::from_secs(5));
    assert_eq!(result.returncode, 0, "{}", result.stderr());
    assert_eq!(serde_json::from_slice::<Value>(&result.stdout).unwrap()["ok"], true);
    assert_no_notice(&[result.stdout.as_slice(), result.stderr.as_slice()].concat());
    let result = upgrade::run(home.command(&home.managed.managed, &[]).stdin(Stdio::null()), Duration::from_secs(5));
    assert_eq!(result.returncode, 2, "{}", result.stderr());
    assert!(result.stdout().contains("terminal is required"), "{}", result.stdout());
    assert_no_notice(&[result.stdout.as_slice(), result.stderr.as_slice()].concat());
    assert_eq!(fs::read(&home.check_path).unwrap(), before);
    assert!(!home.notice_path.exists());
}

fn corrupt_future_unsafe_and_linked_check_cache_cannot_change_tui_result(home: &Home) {
    let external = home.home.join("outside-cache");
    write_private(&external, &home.check_record, 0o600);
    for kind in ["malformed", "duplicate", "future", "mode", "symlink", "hardlink"] {
        let _ = fs::remove_file(&home.check_path);
        match kind {
            "malformed" => {
                fs::write(&home.check_path, "{").unwrap();
                fs::set_permissions(&home.check_path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            "duplicate" => {
                fs::write(
                    &home.check_path,
                    r#"{"schemaVersion":1,"checkedAt":0,"ok":true,"latestVersion":"1.0.0","latestVersion":"9.0.0"}"#,
                )
                .unwrap();
                fs::set_permissions(&home.check_path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            "future" => {
                let mut record = home.check_record.clone();
                record["checkedAt"] = (home.now + 3600).into();
                write_private(&home.check_path, &record, 0o600);
            }
            "mode" => write_private(&home.check_path, &home.check_record, 0o644),
            "symlink" => std::os::unix::fs::symlink(&external, &home.check_path).unwrap(),
            _ => fs::hard_link(&external, &home.check_path).unwrap(),
        }
        let before = fs::read(&home.check_path).unwrap();
        let output = home.tui(true);
        assert!(!contains(&output, NOTICE.as_bytes()), "{kind}: {}", tail(&output));
        if matches!(kind, "mode" | "symlink" | "hardlink") {
            assert_eq!(fs::read(&home.check_path).unwrap(), before, "{kind}");
        } else {
            let refreshed: Value = serde_json::from_slice(&fs::read(&home.check_path).unwrap()).unwrap();
            assert_eq!(refreshed["ok"], false, "{kind}");
            assert!(refreshed["checkedAt"].as_u64().unwrap() >= home.now, "{kind}");
        }
        assert_eq!(serde_json::from_slice::<Value>(&fs::read(&external).unwrap()).unwrap(), home.check_record, "{kind}");
        assert!(!home.notice_path.exists(), "{kind}");
    }
}

fn unsafe_notice_target_is_preserved(home: &Home) {
    let external = home.home.join("notice-sentinel");
    fs::write(&external, "preserve").unwrap();
    std::os::unix::fs::symlink(&external, &home.notice_path).unwrap();
    assert_no_notice(&home.tui(false));
    assert!(fs::symlink_metadata(&home.notice_path).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_to_string(&external).unwrap(), "preserve");
}
