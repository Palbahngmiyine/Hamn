//! The installer's real pre-helper acquisition program (the stock-zsh
//! `acquire-host.zsh` embedded in packaging/release/install.sh.in), without a
//! shared build. Only its curl origin is redirected to a loopback HTTP
//! fixture, by a curl stand-in that first checks the HTTPS restrictions; the
//! full, unmodified installer has its own system-tools gate.
use crate::runner::{self, case};
use crate::support::http::{Options, Reply, Request, Server};
use crate::support::tmp::TempDir;
use crate::support::tui::install_fixture;
use crate::support::upgrade::{self, Group, Output, digest, write_executable};
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "bootstrap-acquire",
        "bootstrap acquisition resumes, verifies, locks and publishes only complete private artifacts",
        vec![
            case("cold_warm_corruption_and_private_publication", cold_warm_corruption_and_private_publication),
            case(
                "interrupted_transfer_resumes_only_missing_bytes_with_validator",
                interrupted_transfer_resumes_only_missing_bytes_with_validator,
            ),
            case("connection_failure_during_resume_preserves_prior_prefix", connection_failure_during_resume_preserves_prior_prefix),
            case("ignored_malformed_or_rejected_range_retries_once", ignored_malformed_or_rejected_range_retries_once),
            case("integrity_and_unbounded_response_fail_closed", integrity_and_unbounded_response_fail_closed),
            case("shared_metadata_requires_exact_versioned_lines", shared_metadata_requires_exact_versioned_lines),
            case("unsafe_paths_and_multilinks_are_preserved_without_network", unsafe_paths_and_multilinks_are_preserved_without_network),
            case("unsafe_parent_and_lock_are_rejected_before_transfer", unsafe_parent_and_lock_are_rejected_before_transfer),
            case(
                "failed_publication_reuses_complete_partial_without_another_request",
                failed_publication_reuses_complete_partial_without_another_request,
            ),
            case("parallel_installers_and_native_flock_share_one_transfer", parallel_installers_and_native_flock_share_one_transfer),
            case(
                "killed_lock_owner_keeps_bounded_partial_and_next_call_recovers",
                killed_lock_owner_keeps_bounded_partial_and_next_call_recovers,
            ),
        ],
        filters,
    )
}

const URL: &str = "https://bootstrap.test/host";
const ORIGIN: &str = "HAMN_DEV_BOOTSTRAP_ORIGIN";

/// The installer's curl, as the acquisition program runs it: the HTTPS-only
/// policy must be present; then the fixture origin replaces the URL (plain
/// HTTP on loopback) and the real /usr/bin/curl runs.
pub fn curl_fixture(_program: &str, args: &[String]) -> ExitCode {
    let mut args = args.to_vec();
    let value = |args: &[String], key: &str| {
        let index = args.iter().position(|arg| arg == key).unwrap_or_else(|| panic!("curl without {key}"));
        index + 1
    };
    for key in ["--proto", "--proto-redir"] {
        let index = value(&args, key);
        assert_eq!(args[index], "=https", "{key}");
        args[index] = "=http".into();
    }
    for required in ["--tlsv1.2", "--max-time", "--max-filesize"] {
        assert!(args.iter().any(|arg| arg == required), "curl without {required}");
    }
    assert_eq!(args.last().map(String::as_str), Some(URL));
    let origin = std::env::var(ORIGIN).expect("fixture origin");
    *args.last_mut().unwrap() = format!("{origin}/host");
    crate::support::real_cli::exec(Command::new("/usr/bin/curl").arg0("curl").args(&args))
}

/// The acquisition program exactly as install.sh.in embeds it.
fn embedded_program() -> String {
    let template = fs::read_to_string(upgrade::checkout().join("packaging/release/install.sh.in")).unwrap();
    let start = "<<'HAMN_BOOTSTRAP_ACQUIRE'\n";
    let body = &template[template.find(start).expect("embedded acquisition program") + start.len()..];
    body[..body.find("\nHAMN_BOOTSTRAP_ACQUIRE").expect("program end")].to_owned()
}

/// Server behaviors, one per scenario.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Normal,
    /// Close without a response.
    Disconnect,
    /// Ignore a Range request and send the whole body with 200.
    Ignore,
    /// Answer a Range request with a Content-Range that starts one byte late.
    BadRange,
    /// Answer a Range request with 416.
    Reject,
    /// Send 64 KiB, then wait for `release` before the rest.
    Pause,
    /// Send only the first KiB of the declared body.
    Interrupt,
    /// Send no length and more bytes than the artifact.
    Oversize,
}

struct State {
    mode: Mode,
    /// Each request's Range and If-Range headers.
    requests: Vec<(Option<String>, Option<String>)>,
    released: bool,
}

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    payload: Arc<Vec<u8>>,
    key: String,
    store: PathBuf,
    final_path: PathBuf,
    partial: PathBuf,
    lock: PathBuf,
    program: PathBuf,
    state: Arc<(Mutex<State>, Condvar)>,
    children: std::cell::RefCell<Vec<Option<Group>>>,
    scratch: std::cell::Cell<usize>,
    origin: String,
    _server: Server,
    _directory: TempDir,
}

fn respond(stream: &mut dyn crate::support::http::Stream, status: &str, headers: &[String]) {
    let mut head = format!("HTTP/1.0 {status}\r\n");
    for header in headers {
        head.push_str(header);
        head.push_str("\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    let _ = stream.write_all(head.as_bytes());
}

fn handle(request: &Request, payload: &[u8], state: &Arc<(Mutex<State>, Condvar)>) -> Reply {
    let requested = request.header("Range").map(str::to_owned);
    let mode = {
        let mut guard = state.0.lock().unwrap();
        guard.requests.push((requested.clone(), request.header("If-Range").map(str::to_owned)));
        guard.mode
    };
    let payload = payload.to_vec();
    let state = Arc::clone(state);
    Reply::Raw(Box::new(move |stream| {
        if mode == Mode::Disconnect {
            return;
        }
        let offset = requested.as_deref().map_or(0, |range| {
            range.strip_prefix("bytes=").and_then(|range| range.strip_suffix('-')).expect("open range").parse::<usize>().unwrap()
        });
        let ignored = mode == Mode::Ignore && requested.is_some();
        let partial = requested.is_some() && !ignored;
        if mode == Mode::Reject && requested.is_some() {
            respond(stream, "416 Requested Range Not Satisfiable", &["Content-Length: 0".into()]);
            return;
        }
        let mut body = payload[if ignored { 0 } else { offset }..].to_vec();
        let mut headers = Vec::new();
        if mode != Mode::Oversize {
            headers.push(format!("Content-Length: {}", body.len()));
        }
        headers.push("ETag: \"bootstrap-fixture\"".into());
        if partial {
            let start = offset + usize::from(mode == Mode::BadRange);
            headers.push(format!("Content-Range: bytes {start}-{}/{}", payload.len() - 1, payload.len()));
        }
        respond(stream, if partial { "206 Partial Content" } else { "200 OK" }, &headers);
        match mode {
            Mode::Pause => {
                let _ = stream.write_all(&body[..65536]).and_then(|()| stream.flush());
                let (lock, released) = &*state;
                let guard = lock.lock().unwrap();
                let _ = released.wait_timeout_while(guard, Duration::from_secs(15), |state| !state.released).unwrap();
                body.drain(..65536);
            }
            Mode::Interrupt => body.truncate(1024),
            Mode::Oversize => body.extend_from_slice(b"overflow"),
            _ => {}
        }
        let _ = stream.write_all(&body).and_then(|()| stream.flush());
    }))
}

impl Fixture {
    fn new() -> Self {
        let directory = TempDir::new("hamn-bootstrap-");
        let root = fs::canonicalize(directory.path()).unwrap();
        let home = root.join("home");
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let payload: Arc<Vec<u8>> = Arc::new((0..=255u8).cycle().take(256 * 1024).collect());
        let key = digest(&payload);
        let store = home.join(".hamn/cache/downloads");
        let state = Arc::new((Mutex::new(State { mode: Mode::Normal, requests: Vec::new(), released: false }), Condvar::new()));
        let (served, shared) = (Arc::clone(&payload), Arc::clone(&state));
        let server = Server::tcp(Options::default(), move |request| handle(request, &served, &shared));
        install_fixture(&root, "curl");
        let program = root.join("acquire.zsh");
        let source = embedded_program();
        fs::write(&program, source.replace("exec /usr/bin/curl", &format!("exec '{}'", root.join("curl").display()))).unwrap();
        Self {
            final_path: store.join(format!("{key}.artifact")),
            partial: store.join(format!(".{key}.partial")),
            lock: store.join(format!(".{key}.lock")),
            origin: server.url("http"),
            root,
            home,
            payload,
            key,
            store,
            program,
            state,
            children: Default::default(),
            scratch: Default::default(),
            _server: server,
            _directory: directory,
        }
    }

    fn mode(&self, mode: Mode) {
        self.state.0.lock().unwrap().mode = mode;
    }

    fn requests(&self) -> Vec<(Option<String>, Option<String>)> {
        self.state.0.lock().unwrap().requests.clone()
    }

    fn release(&self) {
        self.state.0.lock().unwrap().released = true;
        self.state.1.notify_all();
    }

    /// Starts the program in its own process group; returns its index.
    fn spawn_with(&self, key: &str, size: usize) -> usize {
        let index = self.scratch.get();
        self.scratch.set(index + 1);
        let scratch = self.root.join(format!("scratch-{index}"));
        fs::DirBuilder::new().mode(0o700).create(&scratch).unwrap();
        let mut command = Command::new("/bin/zsh");
        command
            .arg("-f")
            .arg(&self.program)
            .arg(&self.home)
            .arg(URL)
            .arg(key)
            .arg(size.to_string())
            .arg(&scratch)
            .arg("0")
            .env_clear()
            .env("HOME", &self.home)
            .env("LC_ALL", "C")
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HAMN_DEV_FIXTURE", "bootstrap-curl")
            .env(ORIGIN, &self.origin);
        let mut children = self.children.borrow_mut();
        children.push(Some(Group::spawn(&mut command)));
        children.len() - 1
    }

    fn spawn(&self) -> usize {
        self.spawn_with(&self.key.clone(), self.payload.len())
    }

    fn finish(&self, index: usize, success: bool) -> Output {
        let child = self.children.borrow_mut()[index].take().expect("a running child");
        let result = child.finish(Duration::from_secs(20));
        assert_eq!(result.returncode == 0, success, "{}: {}", result.returncode, result.stderr());
        result
    }

    fn acquire(&self, success: bool) -> Output {
        let index = self.spawn();
        self.finish(index, success)
    }

    fn acquire_with(&self, key: &str, success: bool) -> Output {
        let index = self.spawn_with(key, self.payload.len());
        self.finish(index, success)
    }

    /// Waits until the child's partial has bytes (bounded, 10 ms polls: the
    /// partial file is the only observable sign of this state).
    fn wait_for_partial(&self, index: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(self.children.borrow_mut()[index].as_mut().unwrap().running(), "worker exited before its partial");
            if fs::metadata(&self.partial).is_ok_and(|info| info.len() > 0) {
                return;
            }
            assert!(Instant::now() < deadline, "the partial never appeared");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn private_partial(&self, bytes: &[u8]) {
        fs::write(&self.partial, bytes).unwrap();
        fs::set_permissions(&self.partial, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Children are killed (their groups) and reaped before the server
        // and the directory go.
        self.release();
        self.children.borrow_mut().clear();
    }
}

fn cold_warm_corruption_and_private_publication() {
    let f = Fixture::new();
    f.acquire(true);
    assert_eq!(fs::read(&f.final_path).unwrap(), *f.payload);
    assert_eq!(fs::metadata(&f.final_path).unwrap().permissions().mode() & 0o7777, 0o600);
    f.acquire(true);
    assert_eq!(f.requests().len(), 1);
    fs::write(&f.final_path, "corrupt").unwrap();
    f.acquire(true);
    assert_eq!(fs::read(&f.final_path).unwrap(), *f.payload);
    assert_eq!(f.requests().len(), 2);
}

fn resumed_request() -> (Option<String>, Option<String>) {
    (Some("bytes=1024-".into()), Some("\"bootstrap-fixture\"".into()))
}

fn interrupted_transfer_resumes_only_missing_bytes_with_validator() {
    let f = Fixture::new();
    f.mode(Mode::Interrupt);
    f.acquire(false);
    assert_eq!(fs::read(&f.partial).unwrap(), f.payload[..1024]);
    assert!(!f.final_path.exists());
    f.mode(Mode::Normal);
    f.acquire(true);
    assert_eq!(f.requests(), [(None, None), resumed_request()]);
    assert_eq!(fs::read(&f.final_path).unwrap(), *f.payload);
}

fn connection_failure_during_resume_preserves_prior_prefix() {
    let f = Fixture::new();
    f.mode(Mode::Interrupt);
    f.acquire(false);
    f.mode(Mode::Disconnect);
    f.acquire(false);
    assert_eq!(fs::read(&f.partial).unwrap(), f.payload[..1024]);
    assert_eq!(f.requests().len(), 2, "a transport failure must not trigger a full retry");
    f.mode(Mode::Normal);
    f.acquire(true);
    assert_eq!(f.requests().last().cloned().unwrap(), resumed_request());
    assert_eq!(fs::read(&f.final_path).unwrap(), *f.payload);
}

fn ignored_malformed_or_rejected_range_retries_once() {
    for mode in [Mode::Ignore, Mode::BadRange, Mode::Reject] {
        let f = Fixture::new();
        f.mode(Mode::Interrupt);
        f.acquire(false);
        f.mode(mode);
        f.acquire(true);
        let ranges: Vec<_> = f.requests().into_iter().map(|request| request.0).collect();
        assert_eq!(ranges, [None, Some("bytes=1024-".into()), None], "{mode:?}");
        assert_eq!(fs::read(&f.final_path).unwrap(), *f.payload, "{mode:?}");
    }
}

fn integrity_and_unbounded_response_fail_closed() {
    let f = Fixture::new();
    let wrong = "0".repeat(64);
    f.acquire_with(&wrong, false);
    assert!(!f.store.join(format!(".{wrong}.partial")).exists());
    f.mode(Mode::Oversize);
    f.acquire(false);
    assert!(!f.final_path.exists());
    assert!(!f.partial.exists());
}

fn shared_metadata_requires_exact_versioned_lines() {
    let f = Fixture::new();
    f.acquire(true);
    let valid = format!("hamn-download 1\n{}\n{}\n\"bootstrap-fixture\"\n", f.key, f.payload.len());
    let size = f.payload.len().to_string();
    for invalid in [
        format!("{valid}extra"),
        format!("{valid}\n"),
        valid.trim_end_matches('\n').to_owned(),
        valid.replace("download 1", "download 2"),
        valid.replace(&size, &format!("0{size}")),
        valid.replace("bootstrap-fixture", "bad\rvalue"),
        valid.replace("bootstrap-fixture", &"x".repeat(1025)),
    ] {
        fs::remove_file(&f.final_path).unwrap();
        f.private_partial(&f.payload[..1024]);
        let metadata = f.store.join(format!(".{}.validator", f.key));
        fs::write(&metadata, &invalid).unwrap();
        fs::set_permissions(&metadata, fs::Permissions::from_mode(0o600)).unwrap();
        f.acquire(true);
        assert_eq!(f.requests().last().unwrap().0, None, "malformed metadata was reused: {:?}", &invalid[invalid.len().saturating_sub(80)..]);
    }
}

fn unsafe_paths_and_multilinks_are_preserved_without_network() {
    let f = Fixture::new();
    f.acquire(true);
    let before = f.requests().len();
    let saved = f.root.join("saved");
    fs::rename(&f.final_path, &saved).unwrap();
    std::os::unix::fs::symlink(&saved, &f.final_path).unwrap();
    f.acquire(false);
    fs::remove_file(&f.final_path).unwrap();
    fs::hard_link(&saved, &f.final_path).unwrap();
    f.acquire(false);
    fs::remove_file(&f.final_path).unwrap();
    fs::rename(&saved, &f.final_path).unwrap();
    fs::set_permissions(&f.final_path, fs::Permissions::from_mode(0o666)).unwrap();
    f.acquire(false);
    assert_eq!(fs::read(&f.final_path).unwrap(), *f.payload);
    assert_eq!(f.requests().len(), before);
}

fn unsafe_parent_and_lock_are_rejected_before_transfer() {
    let f = Fixture::new();
    let outside = f.root.join("outside");
    fs::DirBuilder::new().mode(0o700).create(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, f.home.join(".hamn")).unwrap();
    f.acquire(false);
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    fs::remove_file(f.home.join(".hamn")).unwrap();
    f.acquire(true);
    fs::remove_file(&f.final_path).unwrap();
    let before = f.requests().len();
    fs::set_permissions(&f.lock, fs::Permissions::from_mode(0o644)).unwrap();
    f.acquire(false);
    assert_eq!(f.requests().len(), before);
    assert_eq!(fs::metadata(&f.lock).unwrap().permissions().mode() & 0o7777, 0o644);
}

fn failed_publication_reuses_complete_partial_without_another_request() {
    let f = Fixture::new();
    let mover = f.root.join("move");
    write_executable(&mover, "#!/bin/bash\ncase \"$2\" in *.artifact) exit 75;; esac\nexec /bin/mv \"$@\"\n");
    let production = fs::read_to_string(&f.program).unwrap();
    let publication = "/bin/mv \"$partial\" \"$final\"";
    assert_eq!(production.matches(publication).count(), 1);
    fs::write(&f.program, production.replace(publication, &format!("'{}' \"$partial\" \"$final\"", mover.display()))).unwrap();
    f.acquire(false);
    assert!(!f.final_path.exists());
    assert_eq!(fs::read(&f.partial).unwrap(), *f.payload);
    fs::write(&f.program, production).unwrap();
    f.acquire(true);
    assert_eq!(fs::read(&f.final_path).unwrap(), *f.payload);
    assert_eq!(f.requests().len(), 1);
}

/// Tries to lock `path` exclusively without blocking.
fn try_lock(file: &fs::File) -> bool {
    // SAFETY: flock only affects the open file description of `file`.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

fn parallel_installers_and_native_flock_share_one_transfer() {
    let f = Fixture::new();
    f.mode(Mode::Pause);
    let first = f.spawn();
    f.wait_for_partial(first);
    {
        let held = fs::OpenOptions::new().read(true).write(true).open(&f.lock).unwrap();
        assert!(!try_lock(&held), "the downloading installer does not hold the digest lock");
    }
    let others = [f.spawn(), f.spawn()];
    f.release();
    for index in [first, others[0], others[1]] {
        f.finish(index, true);
    }
    assert_eq!(f.requests().len(), 1);
    // Reverse direction, including after the shell's publication/exit: a
    // native lock holder blocks the program.
    fs::remove_file(&f.final_path).unwrap();
    let held = fs::OpenOptions::new().read(true).write(true).open(&f.lock).unwrap();
    // SAFETY: flock only affects the open file description of `held`.
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    let blocked = upgrade::run(
        Command::new("/bin/zsh").args(["-fc", "zmodload zsh/system; zsystem flock -t 0 \"$1\"", "probe"]).arg(&f.lock),
        Duration::from_secs(5),
    );
    assert_ne!(blocked.returncode, 0);
    let child = f.spawn();
    // SAFETY: as above.
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_UN) }, 0);
    f.finish(child, true);
    assert_eq!(f.requests().len(), 2);
}

fn killed_lock_owner_keeps_bounded_partial_and_next_call_recovers() {
    let f = Fixture::new();
    f.mode(Mode::Pause);
    let index = f.spawn();
    f.wait_for_partial(index);
    f.children.borrow()[index].as_ref().unwrap().signal_group(libc::SIGKILL);
    f.finish(index, false);
    let preserved = fs::metadata(&f.partial).unwrap().len() as usize;
    assert!(preserved > 0 && preserved <= 65536, "{preserved}");
    assert_eq!(fs::read(&f.partial).unwrap(), f.payload[..preserved]);
    f.release();
    f.mode(Mode::Normal);
    f.acquire(true);
    assert_eq!(f.requests().last().unwrap().0, Some(format!("bytes={preserved}-")));
    assert_eq!(fs::read(&f.final_path).unwrap(), *f.payload);
}
