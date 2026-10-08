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
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::fmt::Debug;
use std::fs;
use std::io::Write;
use std::os::fd::{AsRawFd, OwnedFd};
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
    (
        "docker_sync_commits_listeners_and_rejects_an_ambiguous_snapshot",
        docker_sync_commits_listeners_and_rejects_an_ambiguous_snapshot,
    ),
    ("sync_records_unforwarded_ports_and_logs_each_change_once", sync_records_unforwarded_ports_and_logs_each_change_once),
    ("sync_retries_an_unanswered_tcp_request_until_it_succeeds", sync_retries_an_unanswered_tcp_request_until_it_succeeds),
    ("sync_does_not_take_another_process_listener_for_its_own", sync_does_not_take_another_process_listener_for_its_own),
    ("sync_commits_a_pending_tcp_record_only_on_the_master_answer", sync_commits_a_pending_tcp_record_only_on_the_master_answer),
    ("sync_restarts_a_udp_relay_that_is_gone", sync_restarts_a_udp_relay_that_is_gone),
    ("udp_relay_that_ended_under_its_live_parent_counts_as_gone", udp_relay_that_ended_under_its_live_parent_counts_as_gone),
    ("udp_record_without_a_relay_identity_is_recovered", udp_record_without_a_relay_identity_is_recovered),
    (
        "udp_port_published_on_one_address_is_reported_and_gets_no_relay",
        udp_port_published_on_one_address_is_reported_and_gets_no_relay,
    ),
    ("udp_relay_recorded_for_one_address_is_stopped_and_reported", udp_relay_recorded_for_one_address_is_stopped_and_reported),
    ("unconfirmed_tcp_records_are_asked_about_again", unconfirmed_tcp_records_are_asked_about_again),
    ("observer_asks_again_for_inherited_and_replaced_tcp_forwards", observer_asks_again_for_inherited_and_replaced_tcp_forwards),
    ("watch_names_an_unreadable_container_list_once", watch_names_an_unreadable_container_list_once),
    ("every_host_address_maps_to_one_guest_listener", every_host_address_maps_to_one_guest_listener),
    ("listener_failures_leave_no_reservation", listener_failures_leave_no_reservation),
    ("failed_cancel_of_a_free_port_removes_the_tcp_record", failed_cancel_of_a_free_port_removes_the_tcp_record),
    ("udp_pidfile_is_replaced_only_for_a_verified_gone_relay", udp_pidfile_is_replaced_only_for_a_verified_gone_relay),
    ("udp_state_save_failure_creates_no_listener", udp_state_save_failure_creates_no_listener),
    ("mismatched_udp_start_token_clears_without_signaling", mismatched_udp_start_token_clears_without_signaling),
    ("udp_record_without_a_start_token_is_preserved_fail_closed", udp_record_without_a_start_token_is_preserved_fail_closed),
    (
        "sigterm_resistant_relay_is_killed_while_its_identity_matches",
        sigterm_resistant_relay_is_killed_while_its_identity_matches,
    ),
    ("cleanup_removes_mixed_tcp_and_udp_forwards", cleanup_removes_mixed_tcp_and_udp_forwards),
    ("parallel_syncs_of_one_snapshot_create_each_listener_once", parallel_syncs_of_one_snapshot_create_each_listener_once),
    (
        "state_damaged_during_a_pass_fails_it_and_the_next_pass_recovers",
        state_damaged_during_a_pass_fails_it_and_the_next_pass_recovers,
    ),
    (
        "state_capacity_rejects_one_more_published_port_and_an_oversized_file",
        state_capacity_rejects_one_more_published_port_and_an_oversized_file,
    ),
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
/// when dropped.
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

    /// The pidfile of the relay for UDP `port` published on all addresses,
    /// the only publication that gets a relay.
    fn pidfile(&self, port: u16) -> PathBuf {
        self.path().join(format!("udp-0-0-0-0-{port}.pid"))
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
        // A relay exists only for a port published on all addresses.
        assert!(record.protocol == "udp" && record.host_ip == "0.0.0.0" && record.pid > 1 && record.start.0 > 0, "{record:?}");
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

    /// Kills the relay as a crash would and waits until it is gone.
    fn kill(&self) {
        pty::kill(self.pid as u32, libc::SIGKILL);
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.running() {
            assert!(Instant::now() < deadline, "UDP forward process {} survived SIGKILL for 5 s", self.pid);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Requires the relay to be gone: the operation that stops it returns
    /// only after it exited.
    fn assert_gone(&self) {
        assert!(!self.running(), "UDP forward process {} is still running", self.pid);
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Waits up to 10 s for a fixture to write `ready` into the FIFO `ready`.
fn await_ready(ready: &OwnedFd, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut signal = Vec::new();
    while !signal.ends_with(b"ready\n") {
        let remaining = deadline.checked_duration_since(Instant::now()).unwrap_or_else(|| panic!("no signal of {what}"));
        if !pty::readable(&[ready.as_raw_fd()], remaining).is_empty() {
            signal.extend(pty::read_some(ready.as_raw_fd()));
        }
    }
}

/// Binds the UDP `address` whose port a relay held until `Relay::kill`. The
/// killed process stops being visible before the kernel has closed its
/// socket, so the port can stay in use for a moment: waits up to 5 s for it.
fn bind_udp_after_kill(address: &str) -> std::net::UdpSocket {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match std::net::UdpSocket::bind(address) {
            Ok(socket) => return socket,
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("bind the host port {address} of the relay that ended: {error}"),
        }
    }
}

fn committed_tcp(port: u16, container_port: u16) -> String {
    format!("tcp\t127.0.0.1\t{port}\t{container_port}\t0\t0\t0\tcommitted\t0\t0\t0")
}

/// The unforwarded published ports that VM status reports for the profile.
fn failures(profile: &Profile) -> serde_json::Value {
    serde_json::from_slice(&profile.ok(&["failures"]).stdout).expect("a JSON array of failures")
}

/// What `failures` reports for one unforwarded 127.0.0.1 TCP port.
fn unforwarded_tcp(port: u16, reason: &str) -> serde_json::Value {
    serde_json::json!([{"hostIp": "127.0.0.1", "hostPort": port, "protocol": "tcp", "reason": reason}])
}

/// What `failures` reports for one unforwarded UDP port that is published on
/// all addresses.
fn unforwarded_udp(port: u16, reason: &str) -> serde_json::Value {
    serde_json::json!([{"hostIp": "0.0.0.0", "hostPort": port, "protocol": "udp", "reason": reason}])
}

fn docker_snapshot_sync_and_revocation(driver: &Driver) {
    // The driver serves a fixture Engine on PROFILE/docker.sock and syncs one
    // snapshot through the observer: TCP 48250 and UDP 48251, which are
    // forwarded, and UDP 48252 on 127.0.0.1, which is reported and does not
    // keep the observer from waiting for the next event. It checks the state
    // and the reported port, cleans up and revokes the observer lease.
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

fn docker_sync_commits_listeners_and_rejects_an_ambiguous_snapshot(driver: &Driver) {
    // The observer synchronizes a complete snapshot. New mappings become
    // committed once their host listener exists; a repeat is idempotent,
    // removed mappings are stopped, and an ambiguous snapshot changes
    // nothing.
    let profile = Profile::new(driver);
    profile.ok(&["sync", "127.0.0.1:48230:80/tcp", "0.0.0.0:48231:53/udp"]);
    let relay = profile.relay(48231);
    assert_eq!(profile.record(48230).ownership, "committed");
    assert_eq!(profile.record(48231).ownership, "committed");
    assert_eq!(profile.event_count("add\t127.0.0.1\t48230"), 1);
    profile.ok(&["sync", "127.0.0.1:48230:80/tcp", "0.0.0.0:48231:53/udp"]);
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

fn sync_records_unforwarded_ports_and_logs_each_change_once(driver: &Driver) {
    // A synchronization that cannot create a host listener records the port
    // and why, keeps forwarding the others, and stays silent while nothing
    // changes: the observer repeats the same snapshot twice a second.
    let profile = Profile::new(driver);
    let logged = |output: &Output| -> Vec<String> {
        String::from_utf8_lossy(&output.stderr).lines().filter(|line| line.contains("published")).map(str::to_owned).collect()
    };
    assert_eq!(failures(&profile), serde_json::json!([]));
    // Another process holds 48243; the request for the free port 48240 fails.
    let holder = std::net::TcpListener::bind("127.0.0.1:48243").expect("bind the host port a container publishes");
    let earlier = ["sync", "127.0.0.1:48240:80/tcp", "127.0.0.1:48241:81/tcp"];
    let snapshot = ["sync", "127.0.0.1:48243:83/tcp", "127.0.0.1:48240:80/tcp", "127.0.0.1:48241:81/tcp"];
    let broken = [("FAIL_FORWARD_PORT", "48240")];
    let held = [("FAIL_FORWARD_PORT", "48243")];
    let first = profile.fails_with(&broken, &earlier);
    assert_eq!(logged(&first), ["cannot forward published tcp port 127.0.0.1:48240: the forward request failed"]);
    assert_eq!(
        failures(&profile),
        serde_json::json!([{"hostIp": "127.0.0.1", "hostPort": 48240, "protocol": "tcp", "reason": "forwardFailed"}])
    );
    assert_eq!(profile.record(48241).ownership, "committed");
    assert!(profile.records().iter().all(|record| record.host_port != 48240));
    // The same failing snapshot again: a new attempt, no new record or line.
    let recorded = fs::metadata(profile.path().join("port-forward-failures.json")).unwrap().modified().unwrap();
    let repeated = profile.fails_with(&broken, &earlier);
    assert_eq!(logged(&repeated), Vec::<String>::new());
    assert_eq!(profile.event_count("add\t127.0.0.1\t48240"), 2);
    assert_eq!(fs::metadata(profile.path().join("port-forward-failures.json")).unwrap().modified().unwrap(), recorded);
    // 48240 recovers while the published 48243 is held by the other process.
    let changed = profile.fails_with(&held, &snapshot);
    assert_eq!(logged(&changed), ["cannot forward published tcp port 127.0.0.1:48243: another process holds the host port"]);
    assert_eq!(
        failures(&profile),
        serde_json::json!([{"hostIp": "127.0.0.1", "hostPort": 48243, "protocol": "tcp", "reason": "hostPortInUse"}])
    );
    assert_eq!(profile.record(48240).ownership, "committed");
    // The process releases the port and the next pass forwards everything.
    drop(holder);
    assert_eq!(logged(&profile.ok(&snapshot)), Vec::<String>::new());
    assert_eq!(profile.record(48243).ownership, "committed");
    assert_eq!(failures(&profile), serde_json::json!([]));
    assert_eq!(logged(&profile.ok(&snapshot)), Vec::<String>::new());
    // A record that is not what the synchronization writes is not reported.
    for invalid in ["{}", "[{\"hostIp\":\"127.0.0.1\",\"hostPort\":70000,\"protocol\":\"tcp\",\"reason\":\"hostPortInUse\"}]", "not json"] {
        fs::write(profile.path().join("port-forward-failures.json"), invalid).unwrap();
        assert_eq!(failures(&profile), serde_json::json!([]), "{invalid}");
    }
    profile.ok(&["sync"]);
    profile.assert_no_state();
}

fn sync_retries_an_unanswered_tcp_request_until_it_succeeds(driver: &Driver) {
    // The forward request and its cancel both fail while the SSH master is
    // alive, so the request can still take effect: the record stays
    // control-locked. That record says a request was sent, not that a host
    // listener exists. Every later synchronization sends the request again
    // and keeps reporting the port, and the record is committed only by a
    // request that succeeds.
    let profile = Profile::new(driver);
    let snapshot = ["sync", "127.0.0.1:48260:80/tcp"];
    let unanswered = [("FAIL_FORWARD_PORT", "48260"), ("FAIL_CANCEL_PORT", "48260")];
    for attempt in 1..=2 {
        let output = profile.fails_with(&unanswered, &snapshot);
        let record = profile.record(48260);
        assert!(record.ownership == "control-locked" && record.owner_pid > 1, "attempt {attempt}: {record:?}");
        assert_eq!(failures(&profile), unforwarded_tcp(48260, "forwardFailed"), "attempt {attempt}");
        assert_eq!(profile.event_count("add\t127.0.0.1\t48260"), attempt, "attempt {attempt}: {}", profile.events());
        // The observer repeats this twice a second: only the change is logged.
        let logged = String::from_utf8_lossy(&output.stderr).into_owned();
        let expected = if attempt == 1 { "cannot forward published tcp port 127.0.0.1:48260: the forward request failed\n" } else { "" };
        assert_eq!(logged, expected, "attempt {attempt}");
    }
    profile.ok(&snapshot);
    let record = profile.record(48260);
    assert!(record.ownership == "committed" && record.has_no_owner(), "{record:?}");
    assert_eq!(failures(&profile), serde_json::json!([]));
    assert_eq!(profile.event_count("add\t127.0.0.1\t48260"), 3, "{}", profile.events());
    // A committed record is not asked about again.
    profile.ok(&snapshot);
    assert_eq!(profile.event_count("add\t127.0.0.1\t48260"), 3, "{}", profile.events());

    // An unanswered request for a port that is no longer published is
    // cancelled instead of sent again.
    profile.ok(&["sync"]);
    profile.assert_no_state();
    profile.fails_with(&unanswered, &snapshot);
    assert_eq!(profile.record(48260).ownership, "control-locked");
    let cancels = profile.event_count("cancel\t127.0.0.1\t48260");
    profile.ok(&["sync"]);
    profile.assert_no_state();
    assert_eq!(profile.event_count("add\t127.0.0.1\t48260"), 4, "{}", profile.events());
    assert_eq!(profile.event_count("cancel\t127.0.0.1\t48260"), cancels + 1, "{}", profile.events());
    assert_eq!(failures(&profile), serde_json::json!([]));
}

fn sync_does_not_take_another_process_listener_for_its_own(driver: &Driver) {
    // A host port that cannot be bound is not evidence of our listener:
    // another process holds 48261 throughout. While neither the request nor
    // its cancel is answered, who holds the port is unknown. Once the cancel
    // is answered the master holds no such forward, so the holder is another
    // process and nothing remains to resolve.
    let profile = Profile::new(driver);
    let holder = std::net::TcpListener::bind("127.0.0.1:48261").expect("bind the host port a container publishes");
    let snapshot = ["sync", "127.0.0.1:48261:80/tcp"];
    profile.fails_with(&[("FAIL_FORWARD_PORT", "48261"), ("FAIL_CANCEL_PORT", "48261")], &snapshot);
    assert_eq!(profile.record(48261).ownership, "control-locked");
    assert_eq!(failures(&profile), unforwarded_tcp(48261, "forwardFailed"));
    profile.fails_with(&[("FAIL_FORWARD_PORT", "48261")], &snapshot);
    profile.assert_no_state();
    assert_eq!(failures(&profile), unforwarded_tcp(48261, "hostPortInUse"));
    assert_eq!(profile.event_count("add\t127.0.0.1\t48261"), 2, "{}", profile.events());
    // The process releases the port and the next pass forwards it.
    drop(holder);
    profile.ok(&snapshot);
    assert_eq!(profile.record(48261).ownership, "committed");
    assert_eq!(failures(&profile), serde_json::json!([]));
    profile.ok(&["sync"]);
    profile.assert_no_state();
}

fn sync_commits_a_pending_tcp_record_only_on_the_master_answer(driver: &Driver) {
    // A request that never returned leaves its record as reserved (pending)
    // or as sent (control-locked), with an owner that is gone. The two
    // submitted shapes are read as pending records too, although nothing
    // writes them any more. No shape is committed as it stands. The
    // synchronization sends the request once more: a master that already
    // holds the forward answers with success, and that answer commits the
    // record. The committed forward recorded after it is neither asked about
    // nor changed.
    let profile = Profile::new(driver);
    let neighbor = committed_tcp(48264, 81);
    for (port, ownership) in [(48262_u16, "pending"), (48263, "control-locked"), (48265, "submitted"), (48266, "submitted-locked")] {
        let specification = format!("127.0.0.1:{port}:80/tcp");
        let snapshot = ["sync", specification.as_str(), "127.0.0.1:48264:81/tcp"];
        let added = format!("add\t127.0.0.1\t{port}");
        let abandoned = format!("tcp\t127.0.0.1\t{port}\t80\t0\t0\t0\t{ownership}\t{IMPOSSIBLE_OWNER}\t1\t1");
        profile.write_state(&[abandoned.clone(), neighbor.clone()]);
        profile.ok(&snapshot);
        let record = profile.record(port);
        assert!(record.ownership == "committed" && record.has_no_owner(), "{ownership}: {record:?}");
        assert_eq!(profile.line(48264), neighbor, "{ownership}");
        assert_eq!(profile.event_count(&added), 1, "{ownership}: {}", profile.events());
        assert_eq!(failures(&profile), serde_json::json!([]), "{ownership}");

        // Neither the request nor its cancel is answered: the record stays
        // where it is, owned by the process that asked.
        let port_text = port.to_string();
        profile.write_state(&[abandoned.clone(), neighbor.clone()]);
        profile.fails_with(&[("FAIL_FORWARD_PORT", port_text.as_str()), ("FAIL_CANCEL_PORT", port_text.as_str())], &snapshot);
        let record = profile.record(port);
        assert!(record.ownership == "control-locked" && record.owner_pid != IMPOSSIBLE_OWNER, "{ownership}: {record:?}");
        assert_eq!(profile.lines()[1..], [neighbor.clone()], "{ownership}");

        // The master answers that it holds no such forward: the record goes
        // and the port is reported until a later pass forwards it.
        profile.write_state(&[abandoned, neighbor.clone()]);
        profile.fails_with(&[("FAIL_FORWARD_PORT", port_text.as_str())], &snapshot);
        assert_eq!(profile.lines(), [neighbor.clone()], "{ownership}");
        assert_eq!(failures(&profile), unforwarded_tcp(port, "forwardFailed"), "{ownership}");
        profile.ok(&snapshot);
        assert_eq!(profile.record(port).ownership, "committed", "{ownership}");
        assert_eq!(profile.line(48264), neighbor, "{ownership}");
        assert_eq!(profile.event_count(&added), 4, "{ownership}: {}", profile.events());
        assert_eq!(profile.event_count("add\t127.0.0.1\t48264"), 0, "{ownership}: {}", profile.events());
        profile.ok(&["sync"]);
        profile.assert_no_state();
    }
}

fn sync_restarts_a_udp_relay_that_is_gone(driver: &Driver) {
    // A committed UDP record says that a relay was started, not that it
    // still runs. A pass that finds the recorded relay gone starts a new one
    // for the published port instead of reporting the port as forwarded, and
    // a pass that finds it running leaves it alone.
    let profile = Profile::new(driver);
    let published = ["sync", "0.0.0.0:48273:53/udp"];
    profile.ok(&published);
    let first = profile.relay(48273);
    first.kill();
    profile.ok(&published);
    let second = profile.relay(48273);
    assert_ne!((second.pid, second.token), (first.pid, first.token), "the record still names the relay that is gone");
    assert_eq!(profile.record(48273).ownership, "committed");
    assert_eq!(failures(&profile), serde_json::json!([]));
    let recorded = profile.line(48273);
    profile.ok(&published);
    assert_eq!(profile.line(48273), recorded);
    assert!(second.running(), "a running relay was replaced");

    // Another process takes the port of a relay that ended. No relay can
    // start: nothing stays recorded, the stale pidfile is cleared, and the
    // port is reported until a later pass can forward it.
    second.kill();
    let holder = bind_udp_after_kill("127.0.0.1:48273");
    profile.fails(&published);
    profile.assert_no_state();
    assert!(!profile.pidfile(48273).exists());
    assert_eq!(failures(&profile), unforwarded_udp(48273, "hostPortInUse"));
    drop(holder);
    profile.ok(&published);
    let third = profile.relay(48273);
    assert_eq!(failures(&profile), serde_json::json!([]));
    profile.ok(&["sync"]);
    profile.assert_no_state();
    third.assert_gone();

    // The pidfile of a relay that is gone cannot be cleared: the record
    // stays for the pass that can clear it, and the port is reported.
    let gone = format!("udp\t0.0.0.0\t48273\t53\t{IMPOSSIBLE_PID}\t1\t1\tcommitted\t0\t0\t0");
    profile.write_state(std::slice::from_ref(&gone));
    fs::create_dir(profile.pidfile(48273)).unwrap();
    let output = profile.fails(&published);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot remove UDP forward pidfile"), "{stderr}");
    assert_eq!(profile.lines(), [gone]);
    assert_eq!(failures(&profile), unforwarded_udp(48273, "forwardFailed"));
    fs::remove_dir(profile.pidfile(48273)).unwrap();
    profile.ok(&published);
    let fourth = profile.relay(48273);
    profile.ok(&["sync"]);
    profile.assert_no_state();
    fourth.assert_gone();
}

/// Runs `snapshot` (`sync-again` and its ports) as two passes of one driver
/// process and calls `after_first` and `after_second` while that process
/// waits after each pass. Returns the process's output.
fn two_passes(profile: &Profile, snapshot: &[&str], after_first: impl FnOnce(), after_second: impl FnOnce()) -> Output {
    let ready_path = profile.path().join("pass-ready");
    let release_path = profile.path().join("pass-release");
    let ready = pty::fifo(&ready_path);
    // Held open until the process ends: a FIFO drops what no one holds.
    let mut release = fs::File::from(pty::fifo(&release_path));
    let mut command = profile.command(snapshot);
    command.env("PORT_TEST_READY_FIFO", &ready_path).env("PORT_TEST_RELEASE_FIFO", &release_path);
    let process = std::thread::spawn(move || bounded_process::output(&mut command, TIMEOUT));
    await_ready(&ready, "the first pass");
    after_first();
    release.write_all(b"\n").unwrap();
    await_ready(&ready, "the second pass");
    after_second();
    release.write_all(b"\n").unwrap();
    let output = process.join().expect("the two passes");
    fs::remove_file(ready_path).unwrap();
    fs::remove_file(release_path).unwrap();
    output
}

/// Whether a process or an unreaped zombie has `pid`.
fn answers_signals(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process can be signalled.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn udp_relay_that_ended_under_its_live_parent_counts_as_gone(driver: &Driver) {
    // The observer starts the relays and lives on. A relay that ends stays
    // its child, a zombie that still answers signals and that the process
    // information no longer describes. That is a relay that is gone, to the
    // observer and to any other process: it is not left as unverifiable.
    let snapshot = ["sync-again", "0.0.0.0:48278:53/udp"];

    // The parent's next pass reaps the relay and starts a new one.
    let profile = Profile::new(driver);
    let ended = Cell::new(None);
    let output = two_passes(
        &profile,
        &snapshot,
        || {
            let relay = profile.relay(48278);
            relay.kill();
            assert!(answers_signals(relay.pid), "the relay that ended is not an unreaped child");
            ended.set(Some(relay));
        },
        || {
            let relay: Relay = ended.get().unwrap();
            assert!(!answers_signals(relay.pid), "the relay that ended was not reaped");
            let replacement = profile.relay(48278);
            assert_ne!(replacement.pid, relay.pid);
            assert_eq!(failures(&profile), serde_json::json!([]));
        },
    );
    assert!(output.status.success(), "{}", describe(&output));
    profile.ok(&["cleanup"]);
    profile.assert_no_state();

    // The cleanup of a stopping VM runs in another process while the parent
    // has not reaped the relay: it removes the record and the pidfile
    // instead of refusing an unverified process.
    let profile = Profile::new(driver);
    let output = two_passes(
        &profile,
        &snapshot,
        || {
            let relay = profile.relay(48278);
            relay.kill();
            assert!(answers_signals(relay.pid), "the relay that ended is not an unreaped child");
            profile.ok(&["cleanup"]);
            profile.assert_no_state();
            assert!(!profile.pidfile(48278).exists());
        },
        || {
            profile.relay(48278);
        },
    );
    assert!(output.status.success(), "{}", describe(&output));
    profile.ok(&["cleanup"]);
    profile.assert_no_state();
}

fn udp_record_without_a_relay_identity_is_recovered(driver: &Driver) {
    // An observer that dies between reserving a UDP record and recording its
    // relay leaves a pending record without a relay identity. The relay's
    // own pidfile then says what became of it, for a pass that publishes the
    // port, for one that does not, and for the cleanup of a stopping VM.
    let profile = Profile::new(driver);
    let published = ["sync", "0.0.0.0:48274:53/udp"];
    let abandoned = format!("udp\t0.0.0.0\t48274\t53\t0\t0\t0\tpending\t{IMPOSSIBLE_OWNER}\t1\t1");
    let abandon = || profile.write_state(std::slice::from_ref(&abandoned));

    // No relay was started: there is no pidfile and the port can be bound.
    // The published port gets its relay, and a record that is not wanted any
    // more is removed instead of refused for good.
    abandon();
    profile.ok(&published);
    let relay = profile.relay(48274);
    assert_eq!(profile.record(48274).ownership, "committed");
    assert_eq!(failures(&profile), serde_json::json!([]));

    // The relay was started and wrote its pidfile: the record takes its
    // identity, and the relay is neither replaced nor left behind.
    abandon();
    profile.ok(&published);
    let record = profile.record(48274);
    assert!(record.ownership == "committed" && (record.pid, record.start) == (relay.pid, relay.token), "{record:?}");
    assert!(relay.running(), "the relay named by the pidfile was replaced");
    abandon();
    profile.ok(&["cleanup"]);
    profile.assert_no_state();
    relay.assert_gone();
    assert!(!profile.pidfile(48274).exists());
    for stop in [&["sync"][..], &["cleanup"]] {
        abandon();
        profile.ok(stop);
        profile.assert_no_state();
    }

    // The pidfile names a process that is gone: no relay is left.
    abandon();
    fs::write(profile.pidfile(48274), format!("{IMPOSSIBLE_PID}\t1\t1\n")).unwrap();
    profile.ok(&["sync"]);
    profile.assert_no_state();
    assert!(!profile.pidfile(48274).exists());

    // A pidfile that identifies no process cannot be verified. Nothing is
    // replaced, signalled or forgotten, and the published port is reported.
    abandon();
    fs::write(profile.pidfile(48274), "4242\n").unwrap();
    let output = profile.fails(&published);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "cannot forward published udp port 0.0.0.0:48274: its relay cannot be verified\n"
    );
    assert_eq!(failures(&profile), unforwarded_udp(48274, "forwardFailed"));
    for stop in [&["sync"][..], &["cleanup"]] {
        let output = profile.fails(stop);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("refusing to stop the UDP forward on 0.0.0.0:48274: its relay cannot be identified"), "{stderr}");
        assert_eq!(profile.lines(), [abandoned.clone()], "{stop:?}");
        assert_eq!(read(&profile.pidfile(48274)), "4242\n", "{stop:?}");
    }
    fs::remove_file(profile.pidfile(48274)).unwrap();

    // Without a pidfile, a port that another process holds may be held by
    // the relay: the record stays until the port can be bound.
    let holder = std::net::UdpSocket::bind("127.0.0.1:48274").expect("bind the host port of the abandoned record");
    profile.fails(&published);
    assert_eq!(profile.lines(), [abandoned.clone()]);
    assert_eq!(failures(&profile), unforwarded_udp(48274, "forwardFailed"));
    profile.fails(&["cleanup"]);
    assert_eq!(profile.lines(), [abandoned.clone()]);
    drop(holder);
    profile.ok(&published);
    let relay = profile.relay(48274);
    assert_eq!(failures(&profile), serde_json::json!([]));
    profile.ok(&["sync"]);
    profile.assert_no_state();
    relay.assert_gone();
}

/// What `failures` lists for a UDP port published on the one address `address`.
fn unsupported_udp(address: &str, port: u16) -> serde_json::Value {
    serde_json::json!({"hostIp": address, "hostPort": port, "protocol": "udp", "reason": "udpAddressUnsupported"})
}

/// The names of the UDP relay pidfiles in the profile directory.
fn udp_pidfiles(profile: &Profile) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(profile.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("udp-") && name.ends_with(".pid"))
        .collect();
    names.sort();
    names
}

fn udp_port_published_on_one_address_is_reported_and_gets_no_relay(driver: &Driver) {
    // A relay sends to the guest's NAT address, where Docker in the guest
    // listens only for a UDP port that is published on all addresses. A UDP
    // port published on one address would get a relay whose datagrams nothing
    // receives, behind a record that says it is forwarded. It gets no host
    // listener and no record, and is reported for as long as it is published
    // that way. The pass succeeds: repeating it cannot forward the port.
    // 127.0.0.1 is the guest's own loopback; 192.0.2.10 is the guest address
    // of the driver, which the host cannot bind.
    let profile = Profile::new(driver);
    let logged = |output: &Output| String::from_utf8_lossy(&output.stderr).into_owned();
    for (address, port) in [("127.0.0.1", 48290_u16), ("192.0.2.10", 48291)] {
        let specification = format!("{address}:{port}:53/udp");
        let published = ["sync", specification.as_str()];
        assert_eq!(
            logged(&profile.ok(&published)),
            format!(
                "cannot forward published udp port {address}:{port}: a UDP port is forwarded only when it is published on all addresses\n"
            )
        );
        profile.assert_no_state();
        assert_eq!(udp_pidfiles(&profile), Vec::<String>::new(), "{address}");
        assert_eq!(failures(&profile), serde_json::json!([unsupported_udp(address, port)]));
        // Nothing holds the host port on any address.
        drop(std::net::UdpSocket::bind(("0.0.0.0", port)).unwrap_or_else(|error| panic!("{address}:{port} is held: {error}")));
        // The observer repeats the snapshot: only the change was logged.
        assert_eq!(logged(&profile.ok(&published)), "", "{address}");
        assert_eq!(failures(&profile), serde_json::json!([unsupported_udp(address, port)]));
        profile.ok(&["sync"]);
        assert_eq!(failures(&profile), serde_json::json!([]), "{address}");
    }

    // The ports beside it are forwarded, and only it is reported.
    let forwarded = ["127.0.0.1:48292:80/tcp", "0.0.0.0:48293:53/udp"];
    profile.ok(&["sync", forwarded[0], forwarded[1], "127.0.0.1:48290:53/udp"]);
    let neighbor = profile.relay(48293);
    assert_eq!(profile.record(48292).ownership, "committed");
    assert_eq!(profile.records().len(), 2, "{:?}", profile.records());
    assert_eq!(failures(&profile), serde_json::json!([unsupported_udp("127.0.0.1", 48290)]));

    // Published on all addresses instead, the port gets its relay and is no
    // longer reported; published on one address again, it loses both.
    profile.ok(&["sync", forwarded[0], forwarded[1], "0.0.0.0:48290:53/udp"]);
    let relay = profile.relay(48290);
    assert_eq!(failures(&profile), serde_json::json!([]));
    profile.ok(&["sync", forwarded[0], forwarded[1], "127.0.0.1:48290:53/udp"]);
    relay.assert_gone();
    assert!(profile.records().iter().all(|record| record.host_port != 48290), "{:?}", profile.records());
    assert_eq!(udp_pidfiles(&profile), ["udp-0-0-0-0-48293.pid"]);
    assert_eq!(failures(&profile), serde_json::json!([unsupported_udp("127.0.0.1", 48290)]));
    assert!(neighbor.running(), "the relay of a forwarded port was replaced");
    profile.ok(&["sync"]);
    profile.assert_no_state();
    neighbor.assert_gone();
    assert_eq!(failures(&profile), serde_json::json!([]));
}

fn udp_relay_recorded_for_one_address_is_stopped_and_reported(driver: &Driver) {
    // An earlier version started a relay for a UDP port published on the
    // guest's loopback and recorded it as forwarded. The pass that finds the
    // record stops that relay like one of a port that is no longer
    // published, and reports the port. The relay here is a real one whose
    // record and pidfile are renamed to the loopback address.
    let profile = Profile::new(driver);
    let published = ["sync", "127.0.0.1:48294:53/udp"];
    let pidfile = profile.path().join("udp-127-0-0-1-48294.pid");
    profile.ok(&["sync", "0.0.0.0:48294:53/udp"]);
    let relay = profile.relay(48294);
    let earlier = profile.line(48294).replacen("udp\t0.0.0.0\t", "udp\t127.0.0.1\t", 1);
    profile.write_state(std::slice::from_ref(&earlier));
    fs::rename(profile.pidfile(48294), &pidfile).unwrap();
    profile.ok(&published);
    relay.assert_gone();
    profile.assert_no_state();
    assert_eq!(udp_pidfiles(&profile), Vec::<String>::new());
    assert_eq!(failures(&profile), serde_json::json!([unsupported_udp("127.0.0.1", 48294)]));

    // A recorded relay that cannot be verified is not signalled. Its record
    // and pidfile stay, the pass fails so that a later one tries again, and
    // the port is still reported for what it is.
    let mut unrelated = Unrelated::start();
    let pid = unrelated.pid();
    let unverified = format!("udp\t127.0.0.1\t48294\t53\t{pid}\t0\t0\tcommitted\t0\t0\t0");
    profile.write_state(std::slice::from_ref(&unverified));
    fs::write(&pidfile, format!("{pid}\n")).unwrap();
    let output = profile.fails(&published);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("refusing to stop unverified UDP forward process"), "{stderr}");
    unrelated.assert_running();
    assert_eq!(profile.lines(), [unverified]);
    assert_eq!(read(&pidfile), format!("{pid}\n"));
    assert_eq!(failures(&profile), serde_json::json!([unsupported_udp("127.0.0.1", 48294)]));
}

fn unconfirmed_tcp_records_are_asked_about_again(driver: &Driver) {
    // Withdrawing the trust in committed TCP records makes them unconfirmed
    // once: the next pass asks the master about each, and a master that
    // holds the forward confirms it. UDP records and records that are
    // already unconfirmed are not touched.
    let profile = Profile::new(driver);
    let added = "add\t127.0.0.1\t48275";
    let snapshot = ["sync", "127.0.0.1:48275:80/tcp", "0.0.0.0:48276:53/udp"];
    profile.ok(&["unconfirm"]);
    profile.assert_no_state();
    profile.ok(&snapshot);
    let relay = profile.relay(48276);
    let udp = profile.line(48276);
    let committed = profile.lines();

    profile.fails_with(&[("HAMN_TEST_FS_FAIL_BEFORE_RENAME", "1")], &["unconfirm"]);
    assert_eq!(profile.lines(), committed, "a failed update changed the state");

    profile.ok(&["unconfirm"]);
    let record = profile.record(48275);
    assert!(record.ownership == "control-locked" && record.owner_pid > 1, "{record:?}");
    assert_eq!(profile.line(48276), udp);
    let unconfirmed = profile.lines();
    profile.ok(&["unconfirm"]);
    assert_eq!(profile.lines(), unconfirmed, "an unconfirmed record was marked again");
    assert_eq!(profile.event_count(added), 1, "{}", profile.events());

    profile.ok(&snapshot);
    assert_eq!(profile.lines(), committed);
    assert_eq!(profile.event_count(added), 2, "{}", profile.events());
    assert!(relay.running(), "the UDP relay was replaced");
    profile.ok(&snapshot);
    assert_eq!(profile.event_count(added), 2, "a confirmed record was asked about again: {}", profile.events());

    // A record that is unconfirmed when its port is no longer published is
    // cancelled, not asked about.
    profile.ok(&["unconfirm"]);
    profile.ok(&["sync"]);
    profile.assert_no_state();
    relay.assert_gone();
    assert_eq!(profile.event_count(added), 2, "{}", profile.events());
    assert_eq!(profile.event_count("cancel\t127.0.0.1\t48275"), 1, "{}", profile.events());
}

fn observer_asks_again_for_inherited_and_replaced_tcp_forwards(driver: &Driver) {
    // The SSH master holds the TCP listeners, and a new master holds none of
    // the one before it. An observer trusts a committed TCP record only for
    // the master it saw confirm it: it asks again once for the records it
    // inherits when it starts, and once when the control socket has been
    // replaced. It does not ask on every pass. The fixture Engine publishes
    // TCP 48250 and UDP 48251, and UDP 48252 on an address that gets no
    // forward.
    let added = "add\t127.0.0.1\t48250";
    let observe = |inherited: &[String], cycles: &str, replace_at_cycle: &str| -> usize {
        let profile = Profile::new(driver);
        fs::write(profile.path().join("ssh.sock"), "").unwrap();
        profile.write_state(inherited);
        profile.ok(&["observe-fixture", profile.path().to_str().unwrap(), cycles, replace_at_cycle]);
        assert_eq!(profile.record(48250).ownership, "committed");
        assert_eq!(profile.record(48251).ownership, "committed");
        let requests = profile.event_count(added);
        // Before the next observer publishes the same ports.
        profile.ok(&["cleanup"]);
        profile.assert_no_state();
        requests
    };
    let nothing: [String; 0] = [];
    assert_eq!(observe(&[committed_tcp(48250, 80)], "1", "0"), 1, "an inherited record was trusted");
    assert_eq!(observe(&nothing, "3", "0"), 1, "one master was asked on more than one pass");
    assert_eq!(observe(&nothing, "2", "2"), 2, "a replaced master was not asked in the pass that saw it");
    assert_eq!(observe(&nothing, "3", "2"), 2, "a replaced master was asked on a later pass too");
}

fn watch_names_an_unreadable_container_list_once(driver: &Driver) {
    // The watch loop retries twice a second. Three passes without a Docker
    // socket name the cause once.
    let profile = Profile::new(driver);
    let output = profile.ok(&["watch-unavailable"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(
        lines,
        ["cannot read a usable container list from the Docker socket; published ports stay as they are until it answers"],
        "{stderr}"
    );
}

fn every_host_address_maps_to_one_guest_listener(driver: &Driver) {
    // Every macOS address maps to the same guest protocol/port, so one host
    // port has one listener. A snapshot that publishes the port on another
    // address replaces the listener. While the old listener cannot be
    // stopped, the new address is refused instead of added beside it.
    let profile = Profile::new(driver);
    profile.ok(&["sync", "0.0.0.0:48102:80/tcp"]);
    profile.ok(&["sync", "127.0.0.1:48102:80/tcp"]);
    assert_eq!(profile.lines(), [committed_tcp(48102, 80)]);
    assert_eq!(profile.event_count("cancel\t0.0.0.0\t48102"), 1, "{}", profile.events());
    assert_eq!(profile.event_count("add\t127.0.0.1\t48102"), 1, "{}", profile.events());

    // The cancel fails while the master is alive and the port is still held.
    let holder = std::net::TcpListener::bind("127.0.0.1:48102").expect("bind the host port of the old listener");
    let moved = ["sync", "127.0.0.2:48102:80/tcp"];
    let output = profile.fails_with(&[("FAIL_CANCEL_PORT", "48102")], &moved);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot stop TCP forward on 127.0.0.1:48102"), "{stderr}");
    assert!(stderr.contains("host tcp port 127.0.0.2:48102 is already published"), "{stderr}");
    assert_eq!(profile.lines(), [committed_tcp(48102, 80)]);
    assert_eq!(profile.event_count("add\t127.0.0.2\t48102"), 0, "{}", profile.events());
    assert_eq!(
        failures(&profile),
        serde_json::json!([{"hostIp": "127.0.0.2", "hostPort": 48102, "protocol": "tcp", "reason": "forwardFailed"}])
    );
    drop(holder);
    profile.ok(&moved);
    let record = profile.record(48102);
    assert!(record.host_ip == "127.0.0.2" && record.ownership == "committed", "{record:?}");
    assert_eq!(failures(&profile), serde_json::json!([]));
    profile.ok(&["sync"]);
    profile.assert_no_state();
}

fn listener_failures_leave_no_reservation(driver: &Driver) {
    // A listener creation failure removes its reservation. A state-save
    // failure is detected before listener creation, so even an injected
    // cancel failure cannot leave a live untracked listener.
    let profile = Profile::new(driver);
    profile.fails_with(&[("FAIL_FORWARD_PORT", "48103")], &["sync", "127.0.0.1:48103:80/tcp"]);
    profile.assert_no_state();
    profile.fails_with(
        &[("HAMN_TEST_FS_FAIL_BEFORE_RENAME", "1"), ("FAIL_CANCEL_PORT", "48104")],
        &["sync", "127.0.0.1:48104:80/tcp"],
    );
    profile.assert_no_state();
    assert_eq!(profile.event_count("add\t127.0.0.1\t48104"), 0, "listener was touched before its state was reserved");
    assert_eq!(profile.event_count("cancel\t127.0.0.1\t48104"), 0, "listener was touched before its state was reserved");
}

fn failed_cancel_of_a_free_port_removes_the_tcp_record(driver: &Driver) {
    // A failed TCP cancel with a free SO_REUSEADDR bind is idempotent
    // evidence that the listener is already absent, even while the SSH
    // master remains alive. While the port is still held, the record stays
    // until the cancel succeeds or the master, which owns every TCP
    // listener, is gone.
    let profile = Profile::new(driver);
    let published = ["sync", "127.0.0.1:48115:80/tcp"];
    let unanswered = [("FAIL_CANCEL_PORT", "48115")];
    profile.ok(&published);
    profile.ok_with(&unanswered, &["sync"]);
    profile.assert_no_state();

    profile.ok(&published);
    let holder = std::net::TcpListener::bind("127.0.0.1:48115").expect("bind the host port in the master's place");
    for args in [&["sync"][..], &["cleanup"]] {
        let output = profile.fails_with(&unanswered, args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("cannot stop TCP forward on 127.0.0.1:48115"), "{args:?}: {stderr}");
        assert_eq!(profile.lines(), [committed_tcp(48115, 80)], "{args:?}");
    }
    profile.ok_with(&[("FAIL_CANCEL_PORT", "48115"), ("SSH_MASTER_GONE", "1")], &["cleanup"]);
    profile.assert_no_state();
    drop(holder);
}

fn udp_pidfile_is_replaced_only_for_a_verified_gone_relay(driver: &Driver) {
    // A missing state file does not authorize replacing the only identity
    // evidence of a live relay.
    let profile = Profile::new(driver);
    let published = ["sync", "0.0.0.0:48119:53/udp"];
    profile.ok(&published);
    let relay = profile.relay(48119);
    let state = profile.state().unwrap();
    let pidfile = read(&profile.pidfile(48119));
    fs::remove_file(profile.state_path()).unwrap();
    profile.fails(&published);
    assert!(relay.running(), "live UDP relay was replaced after state loss");
    assert_eq!(read(&profile.pidfile(48119)), pidfile);
    profile.assert_no_state();
    fs::write(profile.state_path(), state).unwrap();
    profile.ok(&["sync"]);
    profile.assert_no_state();
    relay.assert_gone();

    // An unverifiable (PID-only) pidfile is preserved fail-closed.
    let published = ["sync", "0.0.0.0:48121:53/udp"];
    fs::write(profile.pidfile(48121), "4242\n").unwrap();
    profile.fails(&published);
    assert_eq!(read(&profile.pidfile(48121)), "4242\n", "unverified UDP pidfile was replaced");
    profile.assert_no_state();
    fs::remove_file(profile.pidfile(48121)).unwrap();

    // Only a complete token of a process that is definitely gone may be
    // replaced.
    fs::write(profile.pidfile(48121), format!("{IMPOSSIBLE_PID}\t1\t1\n")).unwrap();
    profile.ok(&published);
    let relay = profile.relay(48121);
    assert_ne!(relay.pid, IMPOSSIBLE_PID);
    profile.ok(&["sync"]);
    profile.assert_no_state();
    relay.assert_gone();
}

fn udp_state_save_failure_creates_no_listener(driver: &Driver) {
    // The reservation cannot be saved, so no relay starts; the port is then
    // immediately bindable by a new relay.
    let profile = Profile::new(driver);
    let published = ["sync", "0.0.0.0:48106:53/udp"];
    profile.fails_with(&[("HAMN_TEST_FS_FAIL_BEFORE_RENAME", "1")], &published);
    profile.assert_no_state();
    assert!(!profile.pidfile(48106).exists());
    profile.ok(&published);
    let relay = profile.relay(48106);
    profile.ok(&["sync"]);
    profile.assert_no_state();
    relay.assert_gone();
}

fn mismatched_udp_start_token_clears_without_signaling(driver: &Driver) {
    // A mismatched start token proves the recorded relay is gone, so stale
    // tracking clears without signaling the process now holding its PID.
    // A verified relay beside it is stopped.
    let profile = Profile::new(driver);
    let mut unrelated = Unrelated::start();
    let pid = unrelated.pid();
    profile.ok(&["sync", "0.0.0.0:48116:53/udp"]);
    let verified = profile.relay(48116);
    let mut lines = profile.lines();
    lines.insert(0, format!("udp\t0.0.0.0\t48108\t53\t{pid}\t1\t1\tcommitted\t0\t0\t0"));
    profile.write_state(&lines);
    fs::write(profile.pidfile(48108), format!("{pid}\n")).unwrap();
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
    let line = format!("udp\t0.0.0.0\t48109\t53\t{pid}\t0\t0\tcommitted\t0\t0\t0");
    profile.write_state(std::slice::from_ref(&line));
    fs::write(profile.pidfile(48109), format!("{pid}\n")).unwrap();
    let output = profile.fails(&["cleanup"]);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("refusing to stop unverified UDP forward process"),
        "{}",
        describe(&output)
    );
    // A snapshot that publishes the port does not report it as forwarded.
    profile.fails(&["sync", "0.0.0.0:48109:53/udp"]);
    assert_eq!(failures(&profile), unforwarded_udp(48109, "forwardFailed"));
    // A snapshot that no longer publishes the port cannot stop it either.
    unrelated.assert_running();
    assert_eq!(profile.lines(), [line.clone()]);
    assert!(profile.pidfile(48109).exists());
    profile.fails(&["sync"]);
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
    await_ready(&ready, "the SIGTERM-resistant relay's readiness");
    let relay = Relay { pid, token };
    assert!(relay.running(), "the SIGTERM-resistant relay exited");
    profile.write_state(&[format!("udp\t0.0.0.0\t48110\t53\t{pid}\t{}\t{}\tcommitted\t0\t0\t0", token.0, token.1)]);
    fs::write(profile.pidfile(48110), format!("{pid}\n")).unwrap();
    profile.ok(&["cleanup"]);
    profile.assert_no_state();
    assert!(!profile.pidfile(48110).exists());
    relay.assert_gone();
}

fn cleanup_removes_mixed_tcp_and_udp_forwards(driver: &Driver) {
    let profile = Profile::new(driver);
    profile.ok(&["sync", "127.0.0.1:48113:80/tcp", "0.0.0.0:48114:53/udp"]);
    let relay = profile.relay(48114);
    profile.ok(&["cleanup"]);
    profile.assert_no_state();
    relay.assert_gone();
    assert_eq!(profile.event_count("cancel\t127.0.0.1\t48113"), 1, "{}", profile.events());
}

fn parallel_syncs_of_one_snapshot_create_each_listener_once(driver: &Driver) {
    // Every synchronization holds the operation lock for its whole pass, so
    // concurrent passes over one snapshot create each TCP listener and each
    // UDP relay exactly once, and every pass ends with the committed state.
    let profile = Profile::new(driver);
    let tcp: Vec<u16> = (48200..=48215).collect();
    let udp: Vec<u16> = (48225..=48228).collect();
    let mut snapshot = vec!["sync".to_owned()];
    snapshot.extend(tcp.iter().map(|port| format!("127.0.0.1:{port}:80/tcp")));
    snapshot.extend(udp.iter().map(|port| format!("0.0.0.0:{port}:53/udp")));
    let codes = profile.concurrently(&[], &vec![snapshot; 8]);
    assert!(codes.iter().all(|code| *code == Some(0)), "parallel sync: {codes:?}");
    let records = profile.records();
    assert_eq!(records.len(), tcp.len() + udp.len(), "{records:?}");
    assert!(records.iter().all(|record| record.ownership == "committed"), "{records:?}");
    for port in &tcp {
        assert_eq!(profile.event_count(&format!("add\t127.0.0.1\t{port}")), 1, "port {port}: {}", profile.events());
    }
    let relays: Vec<Relay> = udp.iter().map(|port| profile.relay(*port)).collect();
    let pids: BTreeSet<i32> = relays.iter().map(|relay| relay.pid).collect();
    assert_eq!(pids.len(), udp.len(), "relays share a PID: {relays:?}");
    profile.ok(&["cleanup"]);
    profile.assert_no_state();
    for relay in relays {
        relay.assert_gone();
    }
}

/// Runs the snapshot `sync` until its first TCP listener exists and its
/// record is still pending, calls `damage`, and lets the pass finish.
fn sync_damaged_after_the_first_request(profile: &Profile, snapshot: &[&str], damage: impl FnOnce()) -> Output {
    let ready_path = profile.path().join("added-ready");
    let release_path = profile.path().join("added-release");
    let ready = pty::fifo(&ready_path);
    // Held open until the pass ends: a FIFO drops what no one holds.
    let mut release = fs::File::from(pty::fifo(&release_path));
    let mut command = profile.command(snapshot);
    command.env("HAMN_TEST_TCP_ADDED_READY_FIFO", &ready_path).env("HAMN_TEST_TCP_ADDED_RELEASE_FIFO", &release_path);
    let pass = std::thread::spawn(move || bounded_process::output(&mut command, TIMEOUT));
    await_ready(&ready, "the first TCP listener of the pass");
    damage();
    release.write_all(b"\n").unwrap();
    let output = pass.join().expect("the synchronization pass");
    fs::remove_file(ready_path).unwrap();
    fs::remove_file(release_path).unwrap();
    output
}

fn state_damaged_during_a_pass_fails_it_and_the_next_pass_recovers(driver: &Driver) {
    // A pass locks and reads the state again for each request and each
    // commit. State that is damaged between the first request and its commit
    // fails that commit, which alone fails the pass, and every later request
    // of the pass: nothing is committed or requested on state that cannot be
    // read or locked, and the damaged file is not rewritten. Once the damage
    // is repaired, the next pass asks for the first forward again and
    // forwards every port.
    let one = ["sync", "127.0.0.1:48267:80/tcp"];
    let two = ["sync", "127.0.0.1:48267:80/tcp", "127.0.0.1:48268:80/tcp"];
    for snapshot in [&one[..], &two[..]] {
        let committed: Vec<String> = [48267, 48268][..snapshot.len() - 1].iter().map(|port| committed_tcp(*port, 80)).collect();
        let later_request = snapshot.len() > 2;

        let profile = Profile::new(driver);
        let output = sync_damaged_after_the_first_request(&profile, snapshot, || {
            assert_eq!(profile.record(48267).ownership, "pending");
            profile.write_state(&["malformed".to_owned()]);
        });
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{snapshot:?}: {}", describe(&output));
        assert_eq!(stderr.contains("cannot read port forward state"), later_request, "{snapshot:?}: {stderr}");
        assert_eq!(profile.state().as_deref(), Some("malformed\n"), "unreadable state was rewritten");
        assert_eq!(profile.event_count("add\t127.0.0.1\t48268"), 0, "{}", profile.events());
        fs::remove_file(profile.state_path()).unwrap();
        profile.ok(snapshot);
        assert_eq!(profile.lines(), committed, "{snapshot:?}");

        let profile = Profile::new(driver);
        let lock = profile.path().join("port-forwards.lock");
        let output = sync_damaged_after_the_first_request(&profile, snapshot, || {
            fs::remove_file(&lock).unwrap();
            fs::create_dir(&lock).unwrap();
        });
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{snapshot:?}: {}", describe(&output));
        assert_eq!(stderr.contains("cannot lock port forward state"), later_request, "{snapshot:?}: {stderr}");
        assert_eq!(profile.record(48267).ownership, "pending", "a record was committed without the state lock");
        assert_eq!(profile.event_count("add\t127.0.0.1\t48268"), 0, "{}", profile.events());
        fs::remove_dir(&lock).unwrap();
        profile.ok(snapshot);
        assert_eq!(profile.lines(), committed, "{snapshot:?}");
        assert_eq!(profile.event_count("add\t127.0.0.1\t48267"), 2, "the pending record was committed without asking: {}", profile.events());
    }
}

fn state_capacity_rejects_one_more_published_port_and_an_oversized_file(driver: &Driver) {
    // The fixed state capacity (128 records) holds a snapshot of 128
    // published ports. A snapshot of one more port is refused whole, and an
    // oversized file is never partially accepted or rewritten.
    let profile = Profile::new(driver);
    let mut lines: Vec<String> = (0..128).map(|offset| committed_tcp(49000 + offset, 80)).collect();
    let mut snapshot = vec!["sync".to_owned()];
    snapshot.extend((0..128).map(|offset| format!("127.0.0.1:{}:80/tcp", 49000 + offset)));
    snapshot.push("127.0.0.1:49200:80/tcp".to_owned());
    profile.fails(&snapshot.iter().map(String::as_str).collect::<Vec<_>>());
    profile.assert_no_state();
    assert_eq!(profile.events(), "", "a snapshot over the capacity touched a listener");
    snapshot.pop();
    profile.ok(&snapshot.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(profile.lines(), lines, "a full snapshot was not forwarded");
    let full = profile.state();

    // A snapshot within the capacity replaces 49127 by 49200, but the
    // listener of 49127 cannot be stopped. Its record keeps the state full,
    // so the new port is refused instead of recorded past the capacity.
    let holder = std::net::TcpListener::bind("127.0.0.1:49127").expect("bind the host port of the listener that stays");
    snapshot.pop();
    snapshot.push("127.0.0.1:49200:80/tcp".to_owned());
    profile.fails_with(&[("FAIL_CANCEL_PORT", "49127")], &snapshot.iter().map(String::as_str).collect::<Vec<_>>());
    drop(holder);
    assert_eq!(profile.state(), full, "port forward state exceeded its fixed capacity");
    assert_eq!(profile.event_count("add\t127.0.0.1\t49200"), 0, "a port over the capacity was requested");
    assert_eq!(failures(&profile), unforwarded_tcp(49200, "forwardFailed"));
    let forwarded = profile.events();
    lines.push(committed_tcp(49201, 80));
    profile.write_state(&lines);
    let oversized = profile.state();
    profile.fails(&["cleanup"]);
    assert_eq!(profile.state(), oversized, "oversized port forward state was partially accepted");
    assert_eq!(profile.events(), forwarded, "an oversized state touched a listener");
}

/// Every state mutation that production code performs, against listener
/// 49202: a snapshot that publishes it, one that publishes nothing, the
/// withdrawal of trust in TCP records, and the cleanup of a stopping VM.
const MUTATIONS: &[&[&str]] = &[&["sync", "127.0.0.1:49202:80/tcp"], &["sync"], &["unconfirm"], &["cleanup"]];

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
        ("a negative relay pid", "udp\t0.0.0.0\t49202\t53\t-1\t0\t0\tcommitted\t0\t0\t0"),
        ("start microseconds out of range", "udp\t0.0.0.0\t49202\t53\t42\t1\t1000000\tcommitted\t0\t0\t0"),
        ("start microseconds without seconds", "udp\t0.0.0.0\t49202\t53\t42\t0\t1\tcommitted\t0\t0\t0"),
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
        ("5-field UDP", format!("udp\t0.0.0.0\t49203\t53\t{pid}")),
        ("7-field UDP", format!("udp\t0.0.0.0\t49203\t53\t{pid}\t{sec}\t{usec}")),
        ("8-field UDP", format!("udp\t0.0.0.0\t49203\t53\t{pid}\t{sec}\t{usec}\tcommitted")),
    ];
    let mutations: [&[&str]; 5] =
        [&["cleanup"], &["sync"], &["unconfirm"], &["sync", "0.0.0.0:49203:53/udp"], &["sync", "127.0.0.1:49205:80/tcp"]];
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
