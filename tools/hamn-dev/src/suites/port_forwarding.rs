//! Port-forward state (host/fwd/ports.c) under real process concurrency, the
//! Docker observer's parsers and snapshot synchronization, and the UDP relay.
//! The suite compiles the C driver tests/host/test_port_forwarding.c: the
//! forwarding sources with stubbed SSH control, which records every TCP
//! listener add and cancel in an events file. Each case gets a fresh profile
//! directory; the driver's `cleanup`, and SIGKILL for every relay or daemon
//! the case observed whose start token still matches, run even when the case
//! fails.
//!
//! `hamn-dev test port-forwarding [FILTER...]`, from the repository root.
//! With `HAMN_UDP_EXECUTABLE` set, the UDP relay cases run against that
//! production executable, which has no fault injection.
use crate::runner::{self, Case, case};
use crate::suites::{observer_requests, udp_proxy};
use crate::support::exec::{self, Session};
use crate::support::{bounded_process, pty, tmp::TempDir};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::CString;
use std::fmt::Debug;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output, Stdio};
use std::rc::Rc;
use std::str::FromStr;
use std::time::{Duration, Instant};

/// The bound for one driver command. A UDP add waits up to 2 s for its
/// relay, and a stop up to 2 s after each signal.
const TIMEOUT: Duration = Duration::from_secs(30);
const COMPILE_TIMEOUT: Duration = Duration::from_secs(300);
/// The driver's executable name, which `pgrep -f test-port-forwarding` finds.
const DRIVER_NAME: &str = "test-port-forwarding";
const DRIVER_SOURCES: &[&str] = &[
    "tests/host/test_port_forwarding.c",
    "host/fwd/docker_observer.c",
    "host/fwd/ports.c",
    "host/fwd/udp_proxy.c",
    "host/util/fs.c",
    "host/util/proc.c",
    "vendor/cjson/cJSON.c",
];
/// Driver failure injection read by name, besides every `HAMN_TEST_*` and
/// `PORT_TEST_*` variable. None is inherited from the caller.
const INJECTION_VARIABLES: &[&str] = &["FAIL_FORWARD_PORT", "FAIL_CANCEL_PORT", "SSH_MASTER_GONE"];
/// Above macOS's PID range (PID_MAX is 99999), so no process holds them.
const IMPOSSIBLE_PID: i32 = 2147483647;
const IMPOSSIBLE_OWNER: i32 = 2147483646;

type Test = fn(&Driver);

/// The cases after the parser, observer and relay ones, in order.
const DRIVER_CASES: &[(&str, Test)] = &[
    ("published_ports_accept_only_1_to_65535", published_ports_accept_only_1_to_65535),
    ("tcp_reservation_reconcile_commit_and_remove", tcp_reservation_reconcile_commit_and_remove),
    (
        "docker_sync_commits_listeners_and_rejects_an_ambiguous_snapshot",
        docker_sync_commits_listeners_and_rejects_an_ambiguous_snapshot,
    ),
    ("late_owned_completion_leaves_the_replacement_record", late_owned_completion_leaves_the_replacement_record),
    ("serialized_reconcile_keeps_a_live_foreign_generation", serialized_reconcile_keeps_a_live_foreign_generation),
    ("every_host_address_maps_to_one_guest_listener", every_host_address_maps_to_one_guest_listener),
    ("listener_failures_leave_no_reservation", listener_failures_leave_no_reservation),
    ("failed_cancel_of_a_free_port_removes_the_tcp_record", failed_cancel_of_a_free_port_removes_the_tcp_record),
    ("udp_relay_records_its_start_token_and_stops_on_remove", udp_relay_records_its_start_token_and_stops_on_remove),
    ("udp_pidfile_is_replaced_only_for_a_verified_gone_relay", udp_pidfile_is_replaced_only_for_a_verified_gone_relay),
    (
        "dead_pending_udp_owner_is_removed_only_when_its_port_is_free",
        dead_pending_udp_owner_is_removed_only_when_its_port_is_free,
    ),
    ("udp_state_save_failure_creates_no_listener", udp_state_save_failure_creates_no_listener),
    ("mismatched_udp_start_token_clears_without_signaling", mismatched_udp_start_token_clears_without_signaling),
    ("udp_record_without_a_start_token_is_preserved_fail_closed", udp_record_without_a_start_token_is_preserved_fail_closed),
    (
        "sigterm_resistant_relay_is_killed_while_its_identity_matches",
        sigterm_resistant_relay_is_killed_while_its_identity_matches,
    ),
    ("reconcile_matches_the_exact_guest_endpoint", reconcile_matches_the_exact_guest_endpoint),
    ("published_udp_without_a_live_relay_is_not_ready", published_udp_without_a_live_relay_is_not_ready),
    ("reconcile_keeps_published_listeners_and_stops_stale_ones", reconcile_keeps_published_listeners_and_stops_stale_ones),
    ("reconcile_keeps_only_exact_grouped_and_ranged_pairs", reconcile_keeps_only_exact_grouped_and_ranged_pairs),
    ("cleanup_removes_mixed_tcp_and_udp_forwards", cleanup_removes_mixed_tcp_and_udp_forwards),
    ("parallel_adds_and_removes_preserve_every_record", parallel_adds_and_removes_preserve_every_record),
    ("same_listener_race_has_exactly_one_winner", same_listener_race_has_exactly_one_winner),
    ("parallel_udp_adds_keep_every_relay_identity", parallel_udp_adds_keep_every_relay_identity),
    ("state_capacity_rejects_one_more_add_and_an_oversized_file", state_capacity_rejects_one_more_add_and_an_oversized_file),
    ("corrupt_state_fails_every_mutation", corrupt_state_fails_every_mutation),
    ("malformed_records_are_rejected_whole", malformed_records_are_rejected_whole),
    ("pre_release_record_shapes_are_refused_without_side_effects", pre_release_record_shapes_are_refused_without_side_effects),
    ("unopenable_state_lock_fails_every_mutation", unopenable_state_lock_fails_every_mutation),
];

pub fn main(filters: &[String]) -> ExitCode {
    let work = TempDir::new("hamn-port-forwarding-");
    let driver = match compile(work.path()) {
        Ok(binary) => Rc::new(Driver { binary, work: work.path().to_path_buf() }),
        Err(message) => {
            eprintln!("port-forwarding: {message}");
            return ExitCode::FAILURE;
        }
    };
    let udp_work = work.path().join("udp-proxy");
    fs::create_dir(&udp_work).unwrap_or_else(|error| panic!("{}: {error}", udp_work.display()));
    let production = std::env::var_os("HAMN_UDP_EXECUTABLE").filter(|path| !path.is_empty()).map(PathBuf::from);

    let mut cases: Vec<Case> = Vec::new();
    cases.push(driver_case(&driver, "docker_inspect_parser_fixtures", |driver| driver.succeeds(&["inspect-fixtures"])));
    cases.push(driver_case(&driver, "docker_list_parser_fixtures", |driver| driver.succeeds(&["list-fixtures"])));
    cases.extend(observer_requests::cases(&driver.binary, work.path()));
    cases.push(driver_case(&driver, "docker_snapshot_sync_and_revocation", docker_snapshot_sync_and_revocation));
    cases.extend(match &production {
        Some(executable) => udp_proxy::cases(executable, &udp_work, true),
        None => udp_proxy::cases(&driver.binary, &udp_work, false),
    });
    for &(name, test) in DRIVER_CASES {
        cases.push(driver_case(&driver, name, test));
    }
    runner::run("port-forwarding", "process-safe TCP/UDP port forward state and cleanup", cases, filters)
}

fn driver_case(driver: &Rc<Driver>, name: &str, test: Test) -> Case {
    let driver = Rc::clone(driver);
    case(name, move || test(&driver))
}

/// Compiles the driver into `work` with the host tests' C flags.
fn compile(work: &Path) -> Result<PathBuf, String> {
    if let Some(missing) = DRIVER_SOURCES.iter().find(|source| !Path::new(source).is_file()) {
        return Err(format!("{missing} is missing; run from the repository root"));
    }
    let binary = work.join(DRIVER_NAME);
    let mut command = Command::new("clang");
    command
        .args(["-DHAMN_TEST", "-std=c11", "-Wall", "-Wextra", "-Werror=implicit-function-declaration"])
        .args(["-mmacosx-version-min=13.0", "-Ihost", "-Ivendor"])
        .args(DRIVER_SOURCES)
        .arg("-o")
        .arg(&binary);
    let output = bounded_process::output(&mut command, COMPILE_TIMEOUT);
    if output.status.success() { Ok(binary) } else { Err(format!("clang failed: {}", describe(&output))) }
}

/// The compiled C driver.
struct Driver {
    binary: PathBuf,
    /// The suite's temporary directory, holding the driver.
    work: PathBuf,
}

impl Driver {
    /// `driver args`, without any inherited fault injection or test barrier.
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.binary);
        command.args(args).stdin(Stdio::null());
        for (name, _) in std::env::vars_os() {
            let Some(name) = name.to_str() else { continue };
            if name.starts_with("HAMN_TEST_") || name.starts_with("PORT_TEST_") || INJECTION_VARIABLES.contains(&name) {
                command.env_remove(name);
            }
        }
        command
    }

    fn succeeds(&self, args: &[&str]) {
        let output = bounded_process::output(&mut self.command(args), TIMEOUT);
        assert!(output.status.success(), "{args:?}: {}", describe(&output));
    }
}

fn describe(output: &Output) -> String {
    format!(
        "{}, stdout={:?}, stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// One port-forwards.tsv record, as records_save() writes it.
#[derive(Clone, Debug, PartialEq)]
struct Record {
    protocol: String,
    host_ip: String,
    host_port: u16,
    container_port: u16,
    pid: i32,
    start: (u64, u64),
    ownership: String,
    owner_pid: i32,
    owner_start: (u64, u64),
}

impl Record {
    /// Parses one line; the product writes exactly 11 tab-separated fields.
    fn parse(line: &str) -> Self {
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 11, "a port-forwards.tsv record has 11 fields: {line:?}");
        Self {
            protocol: fields[0].to_owned(),
            host_ip: fields[1].to_owned(),
            host_port: number(line, fields[2]),
            container_port: number(line, fields[3]),
            pid: number(line, fields[4]),
            start: (number(line, fields[5]), number(line, fields[6])),
            ownership: fields[7].to_owned(),
            owner_pid: number(line, fields[8]),
            owner_start: (number(line, fields[9]), number(line, fields[10])),
        }
    }

    fn has_no_owner(&self) -> bool {
        self.owner_pid == 0 && self.owner_start == (0, 0)
    }
}

fn number<T: FromStr>(line: &str, field: &str) -> T
where
    T::Err: Debug,
{
    field.parse().unwrap_or_else(|error| panic!("field {field:?} of {line:?}: {error:?}"))
}

/// The OS start token (seconds, microseconds) of process `pid`, as ports.c
/// reads it; `None` when no such process is visible.
fn start_token(pid: i32) -> Option<(u64, u64)> {
    // SAFETY: proc_bsdinfo is plain data, valid when zeroed.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`.
    let written = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (written == size && info.pbi_pid == pid as u32).then_some((info.pbi_start_tvsec, info.pbi_start_tvusec))
}

/// Whether the process that had `token` as `pid` still runs.
fn running(pid: i32, token: (u64, u64)) -> bool {
    start_token(pid) == Some(token)
}

/// An unrelated live process in its own process group, killed and reaped
/// when dropped (or by `stop`).
struct Unrelated {
    session: Session,
    token: (u64, u64),
}

impl Unrelated {
    fn start() -> Self {
        let child = Command::new("/bin/sleep")
            .arg("60")
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap_or_else(|error| panic!("spawn /bin/sleep: {error}"));
        let pid = child.id() as i32;
        let session = Session(child);
        let token = start_token(pid).expect("the start token of a new child");
        Self { session, token }
    }

    fn pid(&self) -> i32 {
        self.session.id() as i32
    }

    fn assert_running(&mut self) {
        assert!(self.session.running() && running(self.pid(), self.token), "unrelated process {} was stopped", self.pid());
    }

    /// Kills and reaps the process, so its identity is definitely gone.
    fn stop(self) {
        drop(self.session);
    }
}

/// One case's profile directory: `PORT_TEST_DIR`, with `logs/` and the
/// events file `PORT_TEST_EVENTS`.
struct Profile<'a> {
    driver: &'a Driver,
    directory: TempDir,
    /// Relays and daemons the case observed, by PID and start token.
    processes: RefCell<Vec<(i32, (u64, u64))>>,
}

impl<'a> Profile<'a> {
    fn new(driver: &'a Driver) -> Self {
        let directory = TempDir::new_in(&driver.work, "profile-");
        fs::create_dir(directory.path().join("logs")).unwrap();
        fs::write(directory.path().join("events.tsv"), "").unwrap();
        Self { driver, directory, processes: RefCell::new(Vec::new()) }
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn state_path(&self) -> PathBuf {
        self.path().join("port-forwards.tsv")
    }

    fn pidfile(&self, port: u16) -> PathBuf {
        self.path().join(format!("udp-127-0-0-1-{port}.pid"))
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = self.driver.command(args);
        command.env("PORT_TEST_DIR", self.path()).env("PORT_TEST_EVENTS", self.path().join("events.tsv"));
        command
    }

    fn run(&self, env: &[(&str, &str)], args: &[&str]) -> Output {
        let mut command = self.command(args);
        command.envs(env.iter().copied());
        bounded_process::output(&mut command, TIMEOUT)
    }

    fn ok(&self, args: &[&str]) -> Output {
        self.ok_with(&[], args)
    }

    fn ok_with(&self, env: &[(&str, &str)], args: &[&str]) -> Output {
        let output = self.run(env, args);
        assert!(output.status.success(), "{env:?} {args:?}: {}", describe(&output));
        output
    }

    /// Runs a mutation that must fail as an operation (exit 1), not as a
    /// usage error or a crash.
    fn fails(&self, args: &[&str]) -> Output {
        self.fails_with(&[], args)
    }

    fn fails_with(&self, env: &[(&str, &str)], args: &[&str]) -> Output {
        let output = self.run(env, args);
        assert_eq!(output.status.code(), Some(1), "{env:?} {args:?} must fail: {}", describe(&output));
        output
    }

    fn state(&self) -> Option<String> {
        match fs::read_to_string(self.state_path()) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("{}: {error}", self.state_path().display()),
        }
    }

    fn assert_no_state(&self) {
        assert_eq!(self.state(), None, "port-forwards.tsv remains");
    }

    fn write_state(&self, lines: &[String]) {
        let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
        fs::write(self.state_path(), text).unwrap();
    }

    fn lines(&self) -> Vec<String> {
        self.state().map_or_else(Vec::new, |text| text.lines().map(str::to_owned).collect())
    }

    fn records(&self) -> Vec<Record> {
        self.lines().iter().map(|line| Record::parse(line)).collect()
    }

    /// The only record for host port `port`.
    fn record(&self, port: u16) -> Record {
        let matching: Vec<Record> = self.records().into_iter().filter(|record| record.host_port == port).collect();
        assert_eq!(matching.len(), 1, "one record for port {port}: {matching:?}");
        matching.into_iter().next().unwrap()
    }

    /// The raw line of the only record for `port`.
    fn line(&self, port: u16) -> String {
        let port = port.to_string();
        let matching: Vec<String> =
            self.lines().into_iter().filter(|line| line.split('\t').nth(2) == Some(port.as_str())).collect();
        assert_eq!(matching.len(), 1, "one record for port {port}: {matching:?}");
        matching.into_iter().next().unwrap()
    }

    fn ports(&self) -> BTreeSet<(u16, u16)> {
        self.records().iter().map(|record| (record.host_port, record.container_port)).collect()
    }

    fn events(&self) -> String {
        fs::read_to_string(self.path().join("events.tsv")).unwrap()
    }

    fn event_count(&self, event: &str) -> usize {
        self.events().lines().filter(|line| *line == event).count()
    }

    /// The live relay of the UDP record for `port`: its start token is
    /// recorded and still identifies the running process.
    fn relay(&self, port: u16) -> Relay {
        let record = self.record(port);
        // pidfile() names 127.0.0.1 pidfiles, the only relays cases start.
        assert!(record.protocol == "udp" && record.host_ip == "127.0.0.1" && record.pid > 1 && record.start.0 > 0, "{record:?}");
        self.track(record.pid, record.start);
        let relay = Relay { pid: record.pid, token: record.start };
        assert!(relay.running(), "UDP relay {} of port {port} is not running", relay.pid);
        relay
    }

    fn track(&self, pid: i32, token: (u64, u64)) {
        self.processes.borrow_mut().push((pid, token));
    }

    /// Runs one driver command per argument list at once and returns their
    /// exit codes in order.
    fn concurrently(&self, env: &[(&str, &str)], commands: &[Vec<String>]) -> Vec<Option<i32>> {
        let mut sessions: Vec<Session> = commands
            .iter()
            .map(|args| {
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                let mut command = self.command(&args);
                command.envs(env.iter().copied()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0);
                Session(command.spawn().unwrap_or_else(|error| panic!("spawn {args:?}: {error}")))
            })
            .collect();
        sessions
            .iter_mut()
            .map(|session| {
                let status = exec::wait_timeout(&mut session.0, TIMEOUT);
                status.unwrap_or_else(|| panic!("driver {} did not finish within {TIMEOUT:?}", session.id())).code()
            })
            .collect()
    }
}

impl Drop for Profile<'_> {
    /// Stops what the state still tracks, then SIGKILLs every observed
    /// process whose identity is unchanged. Never panics.
    fn drop(&mut self) {
        let mut command = self.command(&["cleanup"]);
        command.stdout(Stdio::null()).stderr(Stdio::null());
        if let Ok(mut child) = command.spawn()
            && exec::wait_timeout(&mut child, TIMEOUT).is_none()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        for &(pid, token) in self.processes.borrow().iter() {
            if running(pid, token) {
                pty::kill(pid as u32, libc::SIGKILL);
            }
        }
    }
}

/// A UDP relay (or SIGTERM-resistant daemon) by its PID and start token.
#[derive(Clone, Copy, Debug)]
struct Relay {
    pid: i32,
    token: (u64, u64),
}

impl Relay {
    fn running(&self) -> bool {
        running(self.pid, self.token)
    }

    /// Requires the relay to be gone: the operation that stops it returns
    /// only after it exited.
    fn assert_gone(&self) {
        assert!(!self.running(), "UDP forward process {} is still running", self.pid);
    }
}

/// The driver's rendezvous for unlocked read-modify-write regressions: a
/// named semaphore, unlinked when dropped in case a failed run left it.
struct Rendezvous {
    name: String,
    counter: PathBuf,
    count: usize,
}

impl Rendezvous {
    fn new(profile: &Profile, operation: &str, count: usize) -> Self {
        let name = format!("/hamn-port-{operation}-{}", std::process::id());
        Self { name, counter: profile.path().join(format!("{operation}-barrier-count")), count }
    }

    fn env(&self) -> [(&'static str, String); 3] {
        [
            ("PORT_TEST_BARRIER_NAME", self.name.clone()),
            ("PORT_TEST_BARRIER_COUNTER", self.counter.to_string_lossy().into_owned()),
            ("PORT_TEST_BARRIER_COUNT", self.count.to_string()),
        ]
    }
}

impl Drop for Rendezvous {
    fn drop(&mut self) {
        let name = CString::new(self.name.as_str()).unwrap();
        // SAFETY: name is a valid C string; a missing semaphore is ENOENT.
        unsafe { libc::sem_unlink(name.as_ptr()) };
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn committed_tcp(port: u16, container_port: u16) -> String {
    format!("tcp\t127.0.0.1\t{port}\t{container_port}\t0\t0\t0\tcommitted\t0\t0\t0")
}

fn docker_snapshot_sync_and_revocation(driver: &Driver) {
    // The driver serves a fixture Engine on PROFILE/docker.sock, syncs one
    // snapshot (TCP 48250 and UDP 48251) through the observer, checks the
    // state, cleans up and revokes the observer lease.
    let profile = Profile::new(driver);
    profile.ok(&["snapshot-fixture", profile.path().to_str().unwrap()]);
    profile.assert_no_state();
}

fn published_ports_accept_only_1_to_65535(driver: &Driver) {
    // Host and container ports share one parser; check both sides of the
    // valid interval and the values just outside it.
    for specification in ["127.0.0.1:1:80/tcp", "127.0.0.1:65535:80/tcp", "127.0.0.1:8080:1/tcp", "127.0.0.1:8080:65535/tcp"] {
        driver.succeeds(&["parse", specification]);
    }
    for specification in ["127.0.0.1:0:80/tcp", "127.0.0.1:65536:80/tcp", "127.0.0.1:8080:0/tcp", "127.0.0.1:8080:65536/tcp"] {
        let output = bounded_process::output(&mut driver.command(&["parse", specification]), TIMEOUT);
        assert_eq!(output.status.code(), Some(2), "out-of-range port was accepted: {specification}: {}", describe(&output));
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid port number"), "{}", describe(&output));
    }
}

fn tcp_reservation_reconcile_commit_and_remove(driver: &Driver) {
    // TCP add returns to pending host-listener ownership after its exact
    // control request. The driver process then exits, so reconcile cancels
    // and removes the stale host-only listener: no remote create was
    // submitted.
    let profile = Profile::new(driver);
    profile.ok(&["add", "127.0.0.1:48101:80/tcp"]);
    let records = profile.records();
    assert_eq!(records.len(), 1, "{records:?}");
    let record = &records[0];
    assert!(
        record.protocol == "tcp"
            && record.host_port == 48101
            && record.pid == 0
            && record.ownership == "pending"
            && record.owner_pid > 1
            && record.owner_start.0 > 0,
        "{record:?}"
    );
    // Pending recovery ownership reserves its listener.
    profile.fails(&["add", "127.0.0.1:48101:80/tcp"]);
    profile.ok(&["reconcile", ""]);
    profile.assert_no_state();

    profile.ok(&["add", "127.0.0.1:48101:80/tcp"]);
    profile.ok(&["commit", "127.0.0.1:48101:80/tcp"]);
    profile.ok(&["commit", "127.0.0.1:48101:80/tcp"]);
    let record = profile.record(48101);
    assert!(record.ownership == "committed" && record.has_no_owner(), "{record:?}");
    profile.ok(&["remove", "127.0.0.1:48101:80/tcp"]);
    profile.assert_no_state();
    profile.ok(&["remove", "127.0.0.1:48101:80/tcp"]);
    // A missing forward cannot be committed.
    profile.fails(&["commit", "127.0.0.1:48101:80/tcp"]);
    assert!(profile.event_count("add\t127.0.0.1\t48101") >= 1, "{}", profile.events());
    assert!(profile.event_count("cancel\t127.0.0.1\t48101") >= 1, "{}", profile.events());
}

fn docker_sync_commits_listeners_and_rejects_an_ambiguous_snapshot(driver: &Driver) {
    // The observer synchronizes a complete snapshot. New mappings become
    // committed once their host listener exists; a repeat is idempotent,
    // removed mappings are stopped, and an ambiguous snapshot changes
    // nothing.
    let profile = Profile::new(driver);
    profile.ok(&["sync", "127.0.0.1:48230:80/tcp", "127.0.0.1:48231:53/udp"]);
    let relay = profile.relay(48231);
    assert_eq!(profile.record(48230).ownership, "committed");
    assert_eq!(profile.record(48231).ownership, "committed");
    assert_eq!(profile.event_count("add\t127.0.0.1\t48230"), 1);
    profile.ok(&["sync", "127.0.0.1:48230:80/tcp", "127.0.0.1:48231:53/udp"]);
    assert_eq!(profile.event_count("add\t127.0.0.1\t48230"), 1, "a repeated snapshot re-added a listener");
    profile.ok(&["sync", "127.0.0.1:48232:81/tcp"]);
    assert!(profile.records().iter().all(|record| record.host_port != 48230 && record.host_port != 48231));
    relay.assert_gone();
    profile.fails(&["sync", "127.0.0.1:48232:81/tcp", "0.0.0.0:48232:82/tcp"]);
    let record = profile.record(48232);
    assert!(record.container_port == 81 && record.ownership == "committed", "{record:?}");
    profile.ok(&["sync"]);
    profile.assert_no_state();
}

fn late_owned_completion_leaves_the_replacement_record(driver: &Driver) {
    // A late completion is scoped to its exact wrapper PID/start generation.
    // Once an old reservation is removed and the same listener is claimed
    // again, its stale commit and cleanup leave the new record untouched.
    let profile = Profile::new(driver);
    profile.ok(&["add", "127.0.0.1:48133:80/tcp"]);
    let old = profile.record(48133);
    let generation = [old.owner_pid.to_string(), old.owner_start.0.to_string(), old.owner_start.1.to_string()];
    profile.ok(&["remove", "127.0.0.1:48133:80/tcp"]);
    profile.ok(&["add", "127.0.0.1:48133:81/tcp"]);
    let replacement = profile.line(48133);
    let [pid, sec, usec] = generation.each_ref().map(String::as_str);
    profile.ok(&["commit-owned", "127.0.0.1:48133:80/tcp", pid, sec, usec]);
    assert_eq!(profile.line(48133), replacement);
    profile.ok(&["remove-owned", "127.0.0.1:48133:80/tcp", pid, sec, usec]);
    assert_eq!(profile.line(48133), replacement);
    profile.ok(&["remove", "127.0.0.1:48133:81/tcp"]);
    profile.assert_no_state();
}

fn serialized_reconcile_keeps_a_live_foreign_generation(driver: &Driver) {
    // Serialized reconciliation resolves an in-flight record only when that
    // record is itself serialized. An unrelated operation must not promote a
    // live foreground generation merely because its guest mapping is visible.
    let profile = Profile::new(driver);
    let owner = Unrelated::start();
    let (sec, usec) = owner.token;
    profile.write_state(&[format!("tcp\t127.0.0.1\t48134\t80\t0\t0\t0\tsubmitted\t{}\t{sec}\t{usec}", owner.pid())]);
    profile.ok(&["reconcile-serialized", "127.0.0.1:48134->80/tcp"]);
    let record = profile.record(48134);
    assert!(record.ownership == "submitted" && record.owner_pid == owner.pid(), "{record:?}");
    owner.stop();
    profile.ok(&["reconcile-serialized", "127.0.0.1:48134->80/tcp"]);
    let record = profile.record(48134);
    assert!(record.ownership == "committed" && record.has_no_owner(), "{record:?}");
    profile.ok(&["cleanup"]);
    profile.assert_no_state();
}

fn every_host_address_maps_to_one_guest_listener(driver: &Driver) {
    // Every macOS address maps to the same guest protocol/port, so wildcard
    // and distinct loopback addresses conflict before Docker can become
    // ambiguous.
    let profile = Profile::new(driver);
    profile.ok(&["add", "0.0.0.0:48102:80/tcp"]);
    profile.fails(&["add", "127.0.0.1:48102:80/tcp"]);
    profile.fails(&["add", "127.0.0.2:48102:80/tcp"]);
    assert_eq!(profile.records().len(), 1);
    profile.ok(&["cleanup"]);
    profile.assert_no_state();
}

fn listener_failures_leave_no_reservation(driver: &Driver) {
    // A listener creation failure removes its reservation. A state-save
    // failure is detected before listener creation, so even an injected
    // cancel failure cannot leave a live untracked listener.
    let profile = Profile::new(driver);
    profile.fails_with(&[("FAIL_FORWARD_PORT", "48103")], &["add", "127.0.0.1:48103:80/tcp"]);
    profile.assert_no_state();
    profile.fails_with(
        &[("HAMN_TEST_FS_FAIL_BEFORE_RENAME", "1"), ("FAIL_CANCEL_PORT", "48104")],
        &["add", "127.0.0.1:48104:80/tcp"],
    );
    profile.assert_no_state();
    assert_eq!(profile.event_count("add\t127.0.0.1\t48104"), 0, "listener was touched before its state was reserved");
    assert_eq!(profile.event_count("cancel\t127.0.0.1\t48104"), 0, "listener was touched before its state was reserved");
}

fn failed_cancel_of_a_free_port_removes_the_tcp_record(driver: &Driver) {
    // A failed TCP cancel with a free SO_REUSEADDR bind is idempotent
    // evidence that the listener is already absent, even while the SSH
    // master remains alive.
    let profile = Profile::new(driver);
    profile.ok(&["add", "127.0.0.1:48115:80/tcp"]);
    profile.ok_with(&[("FAIL_CANCEL_PORT", "48115")], &["remove", "127.0.0.1:48115:80/tcp"]);
    profile.assert_no_state();
}

fn udp_relay_records_its_start_token_and_stops_on_remove(driver: &Driver) {
    let profile = Profile::new(driver);
    profile.ok(&["add", "127.0.0.1:48105:53/udp"]);
    let record = profile.record(48105);
    assert!(
        record.protocol == "udp"
            && record.start.0 > 0
            && record.ownership == "pending"
            && record.owner_pid > 1
            && record.owner_start.0 > 0,
        "{record:?}"
    );
    let relay = profile.relay(48105);
    profile.ok(&["remove", "127.0.0.1:48105:53/udp"]);
    profile.assert_no_state();
    relay.assert_gone();
}

fn udp_pidfile_is_replaced_only_for_a_verified_gone_relay(driver: &Driver) {
    // A missing state file does not authorize replacing the only identity
    // evidence of a live relay.
    let profile = Profile::new(driver);
    profile.ok(&["add", "127.0.0.1:48119:53/udp"]);
    let relay = profile.relay(48119);
    let state = profile.state().unwrap();
    let pidfile = read(&profile.pidfile(48119));
    fs::remove_file(profile.state_path()).unwrap();
    profile.fails(&["add", "127.0.0.1:48119:53/udp"]);
    assert!(relay.running(), "live UDP relay was replaced after state loss");
    assert_eq!(read(&profile.pidfile(48119)), pidfile);
    profile.assert_no_state();
    fs::write(profile.state_path(), state).unwrap();
    profile.ok(&["remove", "127.0.0.1:48119:53/udp"]);
    relay.assert_gone();

    // An unverifiable (PID-only) pidfile is preserved fail-closed.
    fs::write(profile.pidfile(48121), "4242\n").unwrap();
    profile.fails(&["add", "127.0.0.1:48121:53/udp"]);
    assert_eq!(read(&profile.pidfile(48121)), "4242\n", "unverified UDP pidfile was replaced");
    profile.assert_no_state();
    fs::remove_file(profile.pidfile(48121)).unwrap();

    // Only a complete token of a process that is definitely gone may be
    // replaced.
    fs::write(profile.pidfile(48121), format!("{IMPOSSIBLE_PID}\t1\t1\n")).unwrap();
    profile.ok(&["add", "127.0.0.1:48121:53/udp"]);
    let relay = profile.relay(48121);
    assert_ne!(relay.pid, IMPOSSIBLE_PID);
    profile.ok(&["remove", "127.0.0.1:48121:53/udp"]);
    relay.assert_gone();
}

fn dead_pending_udp_owner_is_removed_only_when_its_port_is_free(driver: &Driver) {
    // A dead pending owner with no relay identity is removable only when a
    // bind probe proves that no UDP listener exists. An occupied port stays
    // tracked fail-closed until an exact process identity is available.
    let profile = Profile::new(driver);
    let dead_owner = |port: u16| format!("udp\t127.0.0.1\t{port}\t53\t0\t0\t0\tpending\t{IMPOSSIBLE_OWNER}\t1\t1");
    profile.write_state(&[dead_owner(48117)]);
    profile.ok(&["reconcile", ""]);
    profile.assert_no_state();

    profile.ok(&["add", "127.0.0.1:48118:53/udp"]);
    let relay = profile.relay(48118);
    let state = profile.state().unwrap();
    let pidfile = read(&profile.pidfile(48118));
    fs::remove_file(profile.pidfile(48118)).unwrap();
    profile.write_state(&[dead_owner(48118)]);
    profile.ok(&["reconcile", ""]);
    assert_eq!(profile.lines(), [dead_owner(48118)], "an occupied port lost its tracking");
    fs::write(profile.state_path(), state).unwrap();
    fs::write(profile.pidfile(48118), pidfile).unwrap();
    profile.ok(&["remove", "127.0.0.1:48118:53/udp"]);
    relay.assert_gone();
}

fn udp_state_save_failure_creates_no_listener(driver: &Driver) {
    // The reservation cannot be saved, so no relay starts; the port is then
    // immediately bindable by a new relay.
    let profile = Profile::new(driver);
    profile.fails_with(&[("HAMN_TEST_FS_FAIL_BEFORE_RENAME", "1")], &["add", "127.0.0.1:48106:53/udp"]);
    profile.assert_no_state();
    assert!(!profile.pidfile(48106).exists());
    profile.ok(&["add", "127.0.0.1:48106:53/udp"]);
    let relay = profile.relay(48106);
    profile.ok(&["remove", "127.0.0.1:48106:53/udp"]);
    relay.assert_gone();
}

fn mismatched_udp_start_token_clears_without_signaling(driver: &Driver) {
    // A mismatched start token proves the recorded relay is gone, so stale
    // tracking clears without signaling the process now holding its PID.
    // A verified relay beside it is stopped.
    let profile = Profile::new(driver);
    let mut unrelated = Unrelated::start();
    let pid = unrelated.pid();
    profile.write_state(&[format!("udp\t127.0.0.1\t48108\t53\t{pid}\t1\t1\tcommitted\t0\t0\t0")]);
    fs::write(profile.pidfile(48108), format!("{pid}\n")).unwrap();
    profile.ok(&["add", "127.0.0.1:48116:53/udp"]);
    let verified = profile.relay(48116);
    profile.ok(&["cleanup"]);
    unrelated.assert_running();
    verified.assert_gone();
    profile.assert_no_state();
    assert!(!profile.pidfile(48108).exists() && !profile.pidfile(48116).exists());
}

fn udp_record_without_a_start_token_is_preserved_fail_closed(driver: &Driver) {
    // A UDP record whose relay has no start token (a hand-edited record)
    // cannot distinguish the relay from PID reuse. Every destructive path
    // fails and preserves both state and pidfile for inspection.
    let profile = Profile::new(driver);
    let mut unrelated = Unrelated::start();
    let pid = unrelated.pid();
    let line = format!("udp\t127.0.0.1\t48109\t53\t{pid}\t0\t0\tcommitted\t0\t0\t0");
    profile.write_state(std::slice::from_ref(&line));
    fs::write(profile.pidfile(48109), format!("{pid}\n")).unwrap();
    let output = profile.fails(&["cleanup"]);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("refusing to stop unverified UDP forward process"),
        "{}",
        describe(&output)
    );
    for args in [&["remove", "127.0.0.1:48109:53/udp"][..], &["reconcile", ""]] {
        unrelated.assert_running();
        assert_eq!(profile.lines(), [line.clone()]);
        assert!(profile.pidfile(48109).exists());
        profile.fails(args);
    }
    unrelated.assert_running();
    assert_eq!(profile.lines(), [line]);
    assert!(profile.pidfile(48109).exists());
}

fn sigterm_resistant_relay_is_killed_while_its_identity_matches(driver: &Driver) {
    // A verified relay that ignores SIGTERM is escalated to SIGKILL only while
    // its persisted identity still matches; success removes all tracking.
    let profile = Profile::new(driver);
    let ready_path = profile.path().join("stubborn-ready");
    let ready = pty::fifo(&ready_path);
    let output = profile.ok_with(&[("PORT_TEST_READY_FIFO", ready_path.to_str().unwrap())], &["spawn-ignore-sigterm"]);
    let pid: i32 = number("spawn-ignore-sigterm", String::from_utf8_lossy(&output.stdout).trim());
    // Tracked at once, so a failed readiness wait still kills it. Its start
    // token dates from the spawn and does not change when it signals.
    let token = start_token(pid).expect("the start token of the SIGTERM-resistant relay");
    profile.track(pid, token);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut signal = Vec::new();
    while !signal.ends_with(b"ready\n") {
        let remaining = deadline.checked_duration_since(Instant::now()).expect("the SIGTERM-resistant relay's readiness");
        if !pty::readable(&[ready.as_raw_fd()], remaining).is_empty() {
            signal.extend(pty::read_some(ready.as_raw_fd()));
        }
    }
    let relay = Relay { pid, token };
    assert!(relay.running(), "the SIGTERM-resistant relay exited");
    profile.write_state(&[format!("udp\t127.0.0.1\t48110\t53\t{pid}\t{}\t{}\tcommitted\t0\t0\t0", token.0, token.1)]);
    fs::write(profile.pidfile(48110), format!("{pid}\n")).unwrap();
    profile.ok(&["cleanup"]);
    profile.assert_no_state();
    assert!(!profile.pidfile(48110).exists());
    relay.assert_gone();
}

fn reconcile_matches_the_exact_guest_endpoint(driver: &Driver) {
    // A same-port mapping on another loopback address cannot retain the
    // macOS listener.
    let profile = Profile::new(driver);
    profile.ok(&["add", "127.0.0.1:48111:80/tcp"]);
    profile.ok(&["commit", "127.0.0.1:48111:80/tcp"]);
    profile.ok(&["reconcile", "127.0.0.2:48111->80/tcp"]);
    profile.assert_no_state();
}

fn published_udp_without_a_live_relay_is_not_ready(driver: &Driver) {
    // A published guest UDP mapping is not host-ready without a matching
    // relay PID/start token. A dead pending relay is never promoted and a dead
    // committed one never kept as healthy; the evidence stays and reconcile
    // fails until absent inventory makes removal safe.
    let profile = Profile::new(driver);
    for (ownership, owner) in [("pending", format!("{IMPOSSIBLE_OWNER}\t1\t1")), ("committed", "0\t0\t0".to_owned())] {
        let line = format!("udp\t127.0.0.1\t48129\t53\t{IMPOSSIBLE_PID}\t1\t1\t{ownership}\t{owner}");
        profile.write_state(std::slice::from_ref(&line));
        fs::write(profile.pidfile(48129), format!("{IMPOSSIBLE_PID}\t1\t1\n")).unwrap();
        profile.fails(&["reconcile", "192.0.2.10:48129->53/udp"]);
        assert_eq!(profile.lines(), [line], "a dead {ownership} relay was promoted or dropped");
        profile.ok(&["reconcile", ""]);
        profile.assert_no_state();
        assert!(!profile.pidfile(48129).exists());
    }
}

fn reconcile_keeps_published_listeners_and_stops_stale_ones(driver: &Driver) {
    let profile = Profile::new(driver);
    profile.ok(&["add", "127.0.0.1:48111:80/tcp"]);
    profile.ok(&["add", "127.0.0.1:48112:53/udp"]);
    profile.ok(&["commit", "127.0.0.1:48111:80/tcp"]);
    profile.ok(&["commit", "127.0.0.1:48112:53/udp"]);
    let relay = profile.relay(48112);
    profile.ok(&["reconcile", "127.0.0.1:48111->80/tcp"]);
    assert_eq!(profile.ports(), BTreeSet::from([(48111, 80)]));
    assert_eq!(profile.record(48111).protocol, "tcp");
    relay.assert_gone();
    profile.ok(&["reconcile", ""]);
    profile.assert_no_state();
}

fn reconcile_keeps_only_exact_grouped_and_ranged_pairs(driver: &Driver) {
    // Docker groups mappings under one bind IP/protocol suffix and compresses
    // consecutive ports into ranges. Only exact host/container pairs stay.
    let profile = Profile::new(driver);
    for (inventory, specifications, kept) in [
        (
            "127.0.0.1:48120->80, 48122->82, 48123->84/tcp",
            ["127.0.0.1:48120:80/tcp", "127.0.0.1:48122:82/tcp", "127.0.0.1:48123:83/tcp"],
            [(48120, 80), (48122, 82)],
        ),
        (
            "127.0.0.1:48130-48132->90-92/tcp",
            ["127.0.0.1:48130:90/tcp", "127.0.0.1:48131:91/tcp", "127.0.0.1:48132:91/tcp"],
            [(48130, 90), (48131, 91)],
        ),
    ] {
        for specification in specifications {
            profile.ok(&["add", specification]);
            profile.ok(&["commit", specification]);
        }
        profile.ok(&["reconcile", inventory]);
        assert_eq!(profile.ports(), BTreeSet::from(kept), "inventory {inventory:?}");
        profile.ok(&["cleanup"]);
        profile.assert_no_state();
    }
}

fn cleanup_removes_mixed_tcp_and_udp_forwards(driver: &Driver) {
    let profile = Profile::new(driver);
    profile.ok(&["add", "127.0.0.1:48113:80/tcp"]);
    profile.ok(&["add", "127.0.0.1:48114:53/udp"]);
    let relay = profile.relay(48114);
    profile.ok(&["cleanup"]);
    profile.assert_no_state();
    relay.assert_gone();
}

fn parallel_adds_and_removes_preserve_every_record(driver: &Driver) {
    // Concurrent processes' read-modify-write must preserve every
    // independently added TCP record, and concurrent removes must not
    // resurrect one. Were the SSH request made outside the state lock, the
    // driver's rendezvous would line all 24 processes up inside it.
    let profile = Profile::new(driver);
    let ports: Vec<u16> = (48200..=48223).collect();
    for operation in ["add", "remove"] {
        let rendezvous = Rendezvous::new(&profile, operation, ports.len());
        let env = rendezvous.env();
        let env: Vec<(&str, &str)> = env.iter().map(|(name, value)| (*name, value.as_str())).collect();
        let commands: Vec<Vec<String>> =
            ports.iter().map(|port| vec![operation.to_owned(), format!("127.0.0.1:{port}:80/tcp")]).collect();
        let codes = profile.concurrently(&env, &commands);
        assert!(codes.iter().all(|code| *code == Some(0)), "parallel {operation}: {codes:?}");
        if operation == "add" {
            assert_eq!(profile.records().len(), ports.len());
            assert_eq!(profile.ports(), ports.iter().map(|port| (*port, 80)).collect());
        }
    }
    profile.assert_no_state();
}

fn same_listener_race_has_exactly_one_winner(driver: &Driver) {
    let profile = Profile::new(driver);
    let add = vec!["add".to_owned(), "127.0.0.1:48224:80/tcp".to_owned()];
    let mut codes = profile.concurrently(&[], &[add.clone(), add]);
    codes.sort();
    assert_eq!(codes, [Some(0), Some(1)], "exactly one add wins the listener");
    assert_eq!(profile.records().len(), 1);
    profile.ok(&["cleanup"]);
    profile.assert_no_state();
}

fn parallel_udp_adds_keep_every_relay_identity(driver: &Driver) {
    // UDP additions share the same process lock and keep every PID/token pair.
    let profile = Profile::new(driver);
    let ports: Vec<u16> = (48225..=48228).collect();
    let commands: Vec<Vec<String>> =
        ports.iter().map(|port| vec!["add".to_owned(), format!("127.0.0.1:{port}:53/udp")]).collect();
    let codes = profile.concurrently(&[], &commands);
    assert!(codes.iter().all(|code| *code == Some(0)), "parallel UDP add: {codes:?}");
    assert_eq!(profile.records().len(), ports.len());
    let relays: Vec<Relay> = ports.iter().map(|port| profile.relay(*port)).collect();
    let pids: BTreeSet<i32> = relays.iter().map(|relay| relay.pid).collect();
    assert_eq!(pids.len(), ports.len(), "relays share a PID: {relays:?}");
    profile.ok(&["cleanup"]);
    for relay in relays {
        relay.assert_gone();
    }
}

fn state_capacity_rejects_one_more_add_and_an_oversized_file(driver: &Driver) {
    // The fixed state capacity (128 records) rejects both one more add and an
    // oversized file, which is never partially accepted or rewritten.
    let profile = Profile::new(driver);
    let mut lines: Vec<String> = (0..128).map(|offset| committed_tcp(49000 + offset, 80)).collect();
    profile.write_state(&lines);
    let full = profile.state();
    profile.fails(&["add", "127.0.0.1:49200:80/tcp"]);
    assert_eq!(profile.state(), full, "port forward state exceeded its fixed capacity");
    lines.push(committed_tcp(49201, 80));
    profile.write_state(&lines);
    let oversized = profile.state();
    profile.fails(&["cleanup"]);
    assert_eq!(profile.state(), oversized, "oversized port forward state was partially accepted");
    assert_eq!(profile.events(), "", "an oversized state touched a listener");
}

/// Every public mutation, with its arguments, against listener 49202.
const MUTATIONS: &[&[&str]] = &[
    &["add", "127.0.0.1:49202:80/tcp"],
    &["commit", "127.0.0.1:49202:80/tcp"],
    &["remove", "127.0.0.1:49202:80/tcp"],
    &["reconcile", ""],
    &["cleanup"],
];

fn corrupt_state_fails_every_mutation(driver: &Driver) {
    let profile = Profile::new(driver);
    profile.write_state(&["malformed".to_owned()]);
    for args in MUTATIONS {
        profile.fails(args);
        assert_eq!(profile.state().as_deref(), Some("malformed\n"), "{args:?} rewrote corrupt state");
    }
    assert_eq!(profile.events(), "");
}

fn malformed_records_are_rejected_whole(driver: &Driver) {
    // Each line breaks one rule of the 11-field record contract; the whole
    // load fails, so cleanup changes nothing.
    const RECORDS: &[(&str, &str)] = &[
        ("six fields", "tcp\t127.0.0.1\t49202\t80\t0\t0"),
        ("nine fields", "tcp\t127.0.0.1\t49202\t80\t0\t0\t0\tpending\textra"),
        ("a twelfth field", "tcp\t127.0.0.1\t49202\t80\t0\t0\t0\tcommitted\t0\t0\t0\textra"),
        ("an unknown protocol", "sctp\t127.0.0.1\t49202\t80\t0\t0\t0\tcommitted\t0\t0\t0"),
        ("a host address that is not IPv4", "tcp\tnot-an-ip\t49202\t80\t0\t0\t0\tcommitted\t0\t0\t0"),
        ("host port 0", "tcp\t127.0.0.1\t0\t80\t0\t0\t0\tcommitted\t0\t0\t0"),
        ("host port 65536", "tcp\t127.0.0.1\t65536\t80\t0\t0\t0\tcommitted\t0\t0\t0"),
        ("container port 0", "tcp\t127.0.0.1\t49202\t0\t0\t0\t0\tcommitted\t0\t0\t0"),
        ("container port 65536", "tcp\t127.0.0.1\t49202\t65536\t0\t0\t0\tcommitted\t0\t0\t0"),
        ("a negative relay pid", "udp\t127.0.0.1\t49202\t53\t-1\t0\t0\tcommitted\t0\t0\t0"),
        ("start microseconds out of range", "udp\t127.0.0.1\t49202\t53\t42\t1\t1000000\tcommitted\t0\t0\t0"),
        ("start microseconds without seconds", "udp\t127.0.0.1\t49202\t53\t42\t0\t1\tcommitted\t0\t0\t0"),
        ("an unknown ownership", "tcp\t127.0.0.1\t49202\t80\t0\t0\t0\tunknown\t0\t0\t0"),
        ("a negative owner pid", "tcp\t127.0.0.1\t49202\t80\t0\t0\t0\tpending\t-1\t0\t0"),
        ("owner pid 1", "tcp\t127.0.0.1\t49202\t80\t0\t0\t0\tpending\t1\t1\t0"),
        ("an owner without a start token", "tcp\t127.0.0.1\t49202\t80\t0\t0\t0\tpending\t4242\t0\t0"),
        ("an owner start token without a pid", "tcp\t127.0.0.1\t49202\t80\t0\t0\t0\tpending\t0\t1\t0"),
        ("owner microseconds out of range", "tcp\t127.0.0.1\t49202\t80\t0\t0\t0\tpending\t4242\t1\t1000000"),
        ("owner microseconds without seconds", "tcp\t127.0.0.1\t49202\t80\t0\t0\t0\tpending\t0\t0\t1"),
        ("a committed record with an owner", "tcp\t127.0.0.1\t49202\t80\t0\t0\t0\tcommitted\t4242\t1\t1"),
    ];
    let profile = Profile::new(driver);
    for (rule, line) in RECORDS {
        profile.write_state(&[(*line).to_owned()]);
        let output = profile.run(&[], &["cleanup"]);
        assert_eq!(output.status.code(), Some(1), "a record with {rule} was accepted: {}", describe(&output));
        assert_eq!(profile.state(), Some(format!("{line}\n")), "a record with {rule} was rewritten");
    }
    assert_eq!(profile.events(), "");
}

fn pre_release_record_shapes_are_refused_without_side_effects(driver: &Driver) {
    // The 5-field (no start token), 7-field (no ownership) and 8-field (no
    // owner generation) shapes exist only in pre-release state. Alone or
    // after a valid record, every mutation fails without rewriting the
    // state, touching a listener or the pidfile, or signaling the recorded
    // relay, even when the record's start token matches the live process.
    let profile = Profile::new(driver);
    let mut relay = Unrelated::start();
    let (pid, (sec, usec)) = (relay.pid(), relay.token);
    let pidfile = format!("{pid}\t{sec}\t{usec}\n");
    fs::write(profile.pidfile(49203), &pidfile).unwrap();
    let shapes = [
        ("5-field TCP", "tcp\t127.0.0.1\t49203\t80\t0".to_owned()),
        ("5-field UDP", format!("udp\t127.0.0.1\t49203\t53\t{pid}")),
        ("7-field UDP", format!("udp\t127.0.0.1\t49203\t53\t{pid}\t{sec}\t{usec}")),
        ("8-field UDP", format!("udp\t127.0.0.1\t49203\t53\t{pid}\t{sec}\t{usec}\tcommitted")),
    ];
    let mutations: [&[&str]; 7] = [
        &["cleanup"],
        &["reconcile", ""],
        &["remove", "127.0.0.1:49203:80/tcp"],
        &["remove", "127.0.0.1:49203:53/udp"],
        &["add", "127.0.0.1:49205:80/tcp"],
        &["commit", "127.0.0.1:49204:80/tcp"],
        &["sync"],
    ];
    for (shape, line) in shapes {
        for lines in [vec![line.clone()], vec![committed_tcp(49204, 80), line.clone()]] {
            profile.write_state(&lines);
            let state = profile.state();
            for args in mutations {
                let output = profile.run(&[], args);
                assert_eq!(output.status.code(), Some(1), "{args:?} accepted a {shape} record: {}", describe(&output));
                assert_eq!(profile.state(), state, "{args:?} rewrote a {shape} record");
                assert_eq!(profile.events(), "", "{args:?} touched a listener for a {shape} record");
                assert_eq!(read(&profile.pidfile(49203)), pidfile, "{args:?} touched the pidfile of a {shape} record");
                relay.assert_running();
            }
        }
    }
}

fn unopenable_state_lock_fails_every_mutation(driver: &Driver) {
    // Every public mutation fails before touching state when the process
    // lock cannot be opened.
    let profile = Profile::new(driver);
    fs::create_dir(profile.path().join("port-forwards.lock")).unwrap();
    for args in MUTATIONS {
        profile.fails(args);
        profile.assert_no_state();
    }
    assert_eq!(profile.events(), "");
}
