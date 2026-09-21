use super::*;
use std::{
    net::{TcpListener, TcpStream},
    os::unix::fs::{PermissionsExt, symlink},
    sync::{Arc, Mutex, atomic::AtomicBool},
    thread,
    time::Duration,
};

#[test]
fn https_authority_requires_a_host_and_rejects_credentials_and_fragments() {
    for url in [
        "https://example.com/file",
        "https://example.com:443/file",
        "https://[::1]:443/file",
    ] {
        assert!(validate_url(url).is_ok(), "{url}");
    }
    for url in [
        "https://:443/file",
        "https:///file",
        "https://[]/file",
        "https://[invalid]/file",
        "https://host:bad/file",
        "https://user@host/file",
        "https://host/file#fragment",
        "http://host/file",
    ] {
        assert!(validate_url(url).is_err(), "{url}");
    }
}

struct Workspace(Temporary);
impl Workspace {
    fn new() -> Self {
        Self(Temporary::create(&std::env::temp_dir(), true).unwrap().0)
    }
    fn path(&self) -> &Path {
        &self.0.path
    }
}

#[test]
fn forked_child_cannot_extend_a_completed_download_lock() {
    let root = Workspace::new();
    let path = root.path().join("lock");
    let held = lock(&path, false).unwrap();
    assert!(lock(&path, false).is_err());
    let mut release = [0; 2];
    let mut ready = [0; 2];
    assert_eq!(unsafe { libc::pipe(release.as_mut_ptr()) }, 0);
    assert_eq!(unsafe { libc::pipe(ready.as_mut_ptr()) }, 0);
    for fd in release.into_iter().chain(ready) {
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
            0
        );
    }
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        // A multithreaded test runner may own Rust/allocator locks at fork.
        // The child uses only async-signal-safe libc and exits without Drop.
        unsafe {
            libc::close(release[1]);
            libc::close(ready[0]);
            let byte = 1_u8;
            if libc::write(ready[1], (&byte as *const u8).cast(), 1) != 1 {
                libc::_exit(2);
            }
            let mut event = libc::pollfd {
                fd: release[0],
                events: libc::POLLIN,
                revents: 0,
            };
            let received = libc::poll(&mut event, 1, 5000);
            if received <= 0 {
                libc::_exit(3);
            }
            if libc::write(ready[1], (&byte as *const u8).cast(), 1) != 1 {
                libc::_exit(4);
            }
            libc::_exit(0);
        }
    }
    unsafe {
        libc::close(release[0]);
        libc::close(ready[1]);
    }
    let mut event = libc::pollfd {
        fd: ready[0],
        events: libc::POLLIN,
        revents: 0,
    };
    let signaled = unsafe { libc::poll(&mut event, 1, 5000) };
    let mut byte = 0_u8;
    let read = if signaled > 0 {
        unsafe { libc::read(ready[0], (&mut byte as *mut u8).cast(), 1) }
    } else {
        -1
    };
    drop(held);
    // The child still holds the inherited open-file description. Releasing
    // our completed critical section must not depend on that child's lifetime.
    let reacquired = lock(&path, false);
    // Use bytes rather than EOF: other concurrent fork tests can inherit these
    // pipe writers too, but must not extend either synchronization barrier.
    let released = unsafe { libc::write(release[1], (&byte as *const u8).cast(), 1) };
    unsafe { libc::close(release[1]) };
    event.revents = 0;
    let finished = unsafe { libc::poll(&mut event, 1, 5000) };
    let acknowledged = if finished > 0 {
        unsafe { libc::read(ready[0], (&mut byte as *mut u8).cast(), 1) }
    } else {
        -1
    };
    if acknowledged != 1 {
        unsafe { libc::kill(child, libc::SIGKILL) };
    }
    unsafe { libc::close(ready[0]) };
    let mut status = 0;
    let reaped = loop {
        let result = unsafe { libc::waitpid(child, &mut status, 0) };
        if result >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            break result;
        }
    };
    assert_eq!((signaled, read, byte), (1, 1, 1));
    assert_eq!((released, finished, acknowledged), (1, 1, 1));
    assert_eq!(reaped, child);
    assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
    assert!(
        reacquired.is_ok(),
        "completed lock remained busy: {:?}",
        reacquired.err()
    );
}

#[derive(Clone, Copy)]
enum Mode {
    Normal,
    Interrupt,
    Disconnect,
    Ignore,
    Reject,
    Oversize,
}

struct Fixture {
    state: Arc<Mutex<(Mode, Vec<(Option<String>, Option<String>, usize)>)>>,
    stop: Arc<AtomicBool>,
    address: std::net::SocketAddr,
    thread: Option<thread::JoinHandle<()>>,
    curl: PathBuf,
    payload: Vec<u8>,
}

impl Fixture {
    fn new(root: &Path) -> Self {
        Self::with_payload(root, (0..262144).map(|index| (index % 251) as u8).collect())
    }

    fn with_payload(root: &Path, payload: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new((Mode::Normal, Vec::new())));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, ended, data) = (state.clone(), stop.clone(), payload.clone());
        let thread = thread::spawn(move || {
            for stream in listener.incoming() {
                if ended.load(Ordering::SeqCst) {
                    break;
                }
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut input = Vec::new();
                let mut byte = [0u8];
                while !input.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap_or(0) == 0 {
                        break;
                    }
                    input.push(byte[0]);
                    assert!(input.len() <= 16384);
                }
                let text = String::from_utf8(input).unwrap();
                if matches!(shared.lock().unwrap().0, Mode::Disconnect) {
                    shared.lock().unwrap().1.push((None, None, 0));
                    continue;
                }
                let header = |name: &str| {
                    text.lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case(name)
                            .then(|| value.trim().to_string())
                    })
                };
                let range = header("Range");
                let if_range = header("If-Range");
                let mode = shared.lock().unwrap().0;
                let mut offset = 0;
                let mut status = 200;
                if let Some(range) = &range {
                    if matches!(mode, Mode::Reject) {
                        status = 416;
                    } else if !matches!(mode, Mode::Ignore) {
                        offset = range
                            .strip_prefix("bytes=")
                            .unwrap()
                            .trim_end_matches('-')
                            .parse::<usize>()
                            .unwrap();
                        status = 206;
                    }
                }
                let mut body = if status == 416 {
                    Vec::new()
                } else {
                    data[offset..].to_vec()
                };
                if matches!(mode, Mode::Oversize) {
                    body.push(1);
                }
                let advertised = body.len();
                if matches!(mode, Mode::Interrupt) {
                    body.truncate(1000);
                }
                let content_range = if status == 206 {
                    format!(
                        "Content-Range: bytes {offset}-{}/{}\r\n",
                        data.len() - 1,
                        data.len()
                    )
                } else {
                    String::new()
                };
                let response = format!(
                    "HTTP/1.1 {status} fixture\r\nContent-Length: {advertised}\r\nETag: \"native-v1\"\r\n{content_range}Connection: close\r\n\r\n"
                );
                stream.write_all(response.as_bytes()).unwrap();
                let _ = stream.write_all(&body);
                shared.lock().unwrap().1.push((range, if_range, body.len()));
            }
        });
        // Test transport only: production always invokes absolute system curl
        // with HTTPS restrictions. Assert those flags before loopback rewrite.
        let curl = root.join("fixture-curl");
        fs::write(
            &curl,
            format!(
                r#"#!/usr/bin/python3
import os, sys
args=sys.argv[1:]
assert args[0] == '--disable'
for name in ('--proto','--proto-redir'):
    assert args[args.index(name)+1] == '=https'
    args[args.index(name)+1] = '=http'
assert args[-1].startswith('https://fixture.test/')
args[-1]=args[-1].replace('https://fixture.test','http://{address}')
os.execv('/usr/bin/curl',['curl']+args)
"#
            ),
        )
        .unwrap();
        fs::set_permissions(&curl, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            state,
            stop,
            address,
            thread: Some(thread),
            curl,
            payload,
        }
    }
    fn mode(&self, mode: Mode) {
        self.state.lock().unwrap().0 = mode;
    }
    fn requests(&self) -> Vec<(Option<String>, Option<String>, usize)> {
        self.state.lock().unwrap().1.clone()
    }
    fn artifact(&self) -> Artifact {
        Artifact {
            url: "https://fixture.test/artifact".into(),
            sha256: format!("{:x}", Sha256::digest(&self.payload)),
            size: Some(self.payload.len() as u64),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn partial(cache: &Path, artifact: &Artifact, bytes: &[u8]) -> PathBuf {
    let downloads = cache.join("downloads");
    directory(&downloads, 0o700).unwrap();
    let path = downloads.join(format!(".{}.partial", artifact.sha256));
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    save_partial_metadata(
        &downloads.join(format!(".{}.validator", artifact.sha256)),
        &PartialMetadata {
            sha256: artifact.sha256.clone(),
            size: artifact.size.unwrap(),
            validator: Some("\"native-v1\"".into()),
        },
    )
    .unwrap();
    path
}

#[test]
fn cold_warm_and_corrupt_cache_have_independent_network_counts() {
    let root = Workspace::new();
    let cache = cache_root(root.path()).unwrap();
    let fixture = Fixture::new(root.path());
    let artifact = fixture.artifact();
    let (path, counts) = acquire_with_curl(&cache, &artifact, "guestImage", &fixture.curl).unwrap();
    assert_eq!(fs::read(&path).unwrap(), fixture.payload);
    assert_eq!(counts.downloaded_bytes, fixture.payload.len() as u64);
    assert_eq!(fixture.requests().len(), 1);
    let (_, counts) = acquire_with_curl(&cache, &artifact, "guestImage", &fixture.curl).unwrap();
    assert_eq!(counts.downloaded_bytes, 0);
    assert_eq!(counts.reused_bytes, fixture.payload.len() as u64);
    assert_eq!(fixture.requests().len(), 1);
    fs::write(path, b"corrupt").unwrap();
    acquire_with_curl(&cache, &artifact, "guestImage", &fixture.curl).unwrap();
    assert_eq!(fixture.requests().len(), 2);
}

#[test]
fn existing_verified_guest_content_is_reused_without_a_download_marker() {
    let root = Workspace::new();
    let cache = cache_root(root.path()).unwrap();
    let fixture = Fixture::new(root.path());
    let artifact = fixture.artifact();
    let guest = cache.join(format!("hamn-guest-{}.img", artifact.sha256));
    fs::write(&guest, &fixture.payload).unwrap();
    let (path, counts) = acquire_with_curl(&cache, &artifact, "guestImage", &fixture.curl).unwrap();
    assert_eq!(path, guest);
    assert_eq!(counts.source, "guest-cache");
    assert_eq!(counts.downloaded_bytes, 0);
    assert_eq!(counts.reused_bytes, fixture.payload.len() as u64);
    assert!(fixture.requests().is_empty());
}

#[test]
fn interrupted_transfer_resumes_only_missing_bytes_with_validator() {
    let root = Workspace::new();
    let cache = cache_root(root.path()).unwrap();
    let fixture = Fixture::new(root.path());
    let artifact = fixture.artifact();
    fixture.mode(Mode::Interrupt);
    assert!(acquire_with_curl(&cache, &artifact, "host", &fixture.curl).is_err());
    fixture.mode(Mode::Normal);
    let (_, counts) = acquire_with_curl(&cache, &artifact, "host", &fixture.curl).unwrap();
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].0.as_deref(), Some("bytes=1000-"));
    assert_eq!(requests[1].1.as_deref(), Some("\"native-v1\""));
    assert_eq!(counts.downloaded_bytes, fixture.payload.len() as u64 - 1000);
    assert_eq!(counts.resumed_bytes, counts.downloaded_bytes);
    assert_eq!(counts.reused_bytes, 1000);
}

#[test]
fn incomplete_validator_schema_restarts_without_a_range_request() {
    let root = Workspace::new();
    let cache = cache_root(root.path()).unwrap();
    let fixture = Fixture::new(root.path());
    let artifact = fixture.artifact();
    partial(&cache, &artifact, &fixture.payload[..1000]);
    let metadata = cache.join(format!("downloads/.{}.validator", artifact.sha256));
    fs::write(
        &metadata,
        format!(
            "hamn-download 1\n{}\n{}\n",
            artifact.sha256,
            artifact.size.unwrap()
        ),
    )
    .unwrap();
    let (_, counts) = acquire_with_curl(&cache, &artifact, "host", &fixture.curl).unwrap();
    assert_eq!(fixture.requests()[0].0, None);
    assert_eq!(counts.reused_bytes, 0);
    assert_eq!(counts.downloaded_bytes, fixture.payload.len() as u64);
    let without_validator = format!(
        "hamn-download 1\n{}\n{}\n\n",
        artifact.sha256,
        artifact.size.unwrap()
    );
    assert!(
        partial_metadata(without_validator.as_bytes())
            .unwrap()
            .validator
            .is_none()
    );
}

fn bootstrap(root: &Path, fixture: &Fixture, success: bool) {
    let (stage, _) = Temporary::create(root, true).unwrap();
    let source = include_str!("../../packaging/release/install.sh.in")
        .split_once("<<'HAMN_BOOTSTRAP_ACQUIRE'\n")
        .unwrap()
        .1
        .split_once("\nHAMN_BOOTSTRAP_ACQUIRE")
        .unwrap()
        .0;
    let script = stage.path.join("bootstrap.zsh");
    // Only the test-owned origin transport is replaced. It checks HTTPS policy
    // and executes real system curl against our independently counted server.
    fs::write(
        &script,
        source.replace(
            "exec /usr/bin/curl",
            &format!("exec '{}'", fixture.curl.display()),
        ),
    )
    .unwrap();
    let artifact = fixture.artifact();
    let output = Command::new("/bin/zsh")
        .arg("-f")
        .arg(script)
        .arg(root)
        .arg(&artifact.url)
        .arg(&artifact.sha256)
        .arg(artifact.size.unwrap().to_string())
        .arg(&stage.path)
        .arg("0")
        .env_clear()
        .env("HOME", root)
        .env("LC_ALL", "C")
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .output()
        .unwrap();
    assert_eq!(
        output.status.success(),
        success,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if success {
        let published = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        assert_eq!(fs::read(published).unwrap(), fixture.payload);
    }
}

#[test]
fn native_interruption_then_bootstrap_preserves_range_validator_and_bytes() {
    let root = Workspace::new();
    let cache = cache_root(root.path()).unwrap();
    let fixture = Fixture::new(root.path());
    fixture.mode(Mode::Interrupt);
    let artifact = fixture.artifact();
    assert!(acquire_with_curl(&cache, &artifact, "host", &fixture.curl).is_err());
    fixture.mode(Mode::Disconnect);
    bootstrap(root.path(), &fixture, false);
    let partial_path = cache.join(format!("downloads/.{}.partial", artifact.sha256));
    assert_eq!(fs::read(partial_path).unwrap(), fixture.payload[..1000]);
    fixture.mode(Mode::Normal);
    bootstrap(root.path(), &fixture, true);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2].0.as_deref(), Some("bytes=1000-"));
    assert_eq!(requests[2].1.as_deref(), Some("\"native-v1\""));
    assert_eq!(
        requests.iter().map(|request| request.2).sum::<usize>(),
        fixture.payload.len()
    );
}

#[test]
fn bootstrap_interruption_then_native_preserves_range_validator_and_bytes() {
    let root = Workspace::new();
    let cache = cache_root(root.path()).unwrap();
    let fixture = Fixture::new(root.path());
    fixture.mode(Mode::Interrupt);
    bootstrap(root.path(), &fixture, false);
    let artifact = fixture.artifact();
    fixture.mode(Mode::Disconnect);
    assert!(acquire_with_curl(&cache, &artifact, "host", &fixture.curl).is_err());
    let partial_path = cache.join(format!("downloads/.{}.partial", artifact.sha256));
    assert_eq!(fs::read(partial_path).unwrap(), fixture.payload[..1000]);
    fixture.mode(Mode::Normal);
    let (path, counts) = acquire_with_curl(&cache, &artifact, "host", &fixture.curl).unwrap();
    assert_eq!(fs::read(path).unwrap(), fixture.payload);
    assert_eq!(counts.downloaded_bytes, fixture.payload.len() as u64 - 1000);
    assert_eq!(counts.resumed_bytes, counts.downloaded_bytes);
    assert_eq!(counts.reused_bytes, 1000);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[2].0.as_deref(), Some("bytes=1000-"));
    assert_eq!(requests[2].1.as_deref(), Some("\"native-v1\""));
    assert_eq!(
        requests.iter().map(|request| request.2).sum::<usize>(),
        fixture.payload.len()
    );
}

#[test]
fn common_partial_metadata_rejects_ambiguous_or_unbounded_lines() {
    let valid = format!(
        "hamn-download 1\n{}\n123\n\"quoted\\validator\"\n",
        "a".repeat(64)
    );
    assert!(partial_metadata(valid.as_bytes()).is_some());
    for invalid in [
        valid.replace("download 1", "download 2"),
        valid.replace("123", "0123"),
        valid.replace("123", "0"),
        valid.replace("123", "2147483648"),
        valid.replace("123", "-1"),
        valid.replace(&"a".repeat(64), &"A".repeat(64)),
        valid.replace("123\n", "123\nextra\n"),
        valid.trim_end_matches('\n').to_owned(),
        valid.clone() + "extra\n",
        valid.replace("quoted", "bad\rvalue"),
        valid.replace("quoted", &"v".repeat(1025)),
        valid.replace("quoted", "bad\0value"),
    ] {
        assert!(
            partial_metadata(invalid.as_bytes()).is_none(),
            "accepted {invalid:?}"
        );
    }
}

#[test]
fn generated_native_acquisition_preserves_content_ranges_and_accounting() {
    // Feature: automatic-upgrade-and-image-optimization, Properties 4, 6, 12.
    // Seed 20260921; 100 bounded real acquisitions with independent HTTP counts
    // and system OpenSSL digest checks. This is not an all-input proof.
    let mut seed = 20260921u64;
    let mut next = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        seed
    };
    for case in 0..100 {
        let root = Workspace::new();
        let cache = cache_root(root.path()).unwrap();
        let size = match case {
            0 => 1,
            1 => 2,
            2 => 511,
            3 => 512,
            4 => 4096,
            _ => 2 + (next() % 65535) as usize,
        };
        let payload: Vec<u8> = (0..size).map(|_| (next() >> 32) as u8).collect();
        let fixture = Fixture::with_payload(root.path(), payload);
        let artifact = fixture.artifact();
        let prefix = if size == 1 {
            0
        } else {
            1 + next() as usize % (size - 1)
        };
        let mut expected_reused = 0;
        let mut prior_network = 0;
        match case % 5 {
            1 => {
                partial(&cache, &artifact, &fixture.payload[..prefix]);
                expected_reused = prefix;
            }
            2 => {
                let outside = root.path().join("preserved");
                fs::write(&outside, b"owned sentinel").unwrap();
                let partial_path = partial(&cache, &artifact, &fixture.payload[..prefix]);
                fs::remove_file(&partial_path).unwrap();
                symlink(&outside, &partial_path).unwrap();
                assert!(acquire_with_curl(&cache, &artifact, "host", &fixture.curl).is_err());
                assert!(
                    fixture.requests().is_empty(),
                    "unsafe partial requested network: {case}"
                );
                assert_eq!(fs::read(outside).unwrap(), b"owned sentinel");
                fs::remove_file(partial_path).unwrap();
            }
            3 => {
                let mut corrupt = fixture.payload[..prefix].to_vec();
                corrupt[0] ^= 1;
                let partial_path = partial(&cache, &artifact, &corrupt);
                assert!(acquire_with_curl(&cache, &artifact, "host", &fixture.curl).is_err());
                assert!(!partial_path.exists());
                assert!(
                    !cache
                        .join(format!("downloads/{}.artifact", artifact.sha256))
                        .exists()
                );
                let requests = fixture.requests();
                assert_eq!(
                    requests[0].0.as_deref(),
                    Some(format!("bytes={prefix}-").as_str())
                );
                assert_eq!(requests[0].2, size - prefix);
                prior_network = size - prefix;
            }
            4 => {
                let mut oversized = artifact.clone();
                oversized.size = Some(u64::MAX - next() % 1024);
                assert!(acquire_with_curl(&cache, &oversized, "host", &fixture.curl).is_err());
                assert!(
                    fixture.requests().is_empty(),
                    "overflowing size requested network: {case}"
                );
            }
            _ => {}
        }
        let (path, counts) = acquire_with_curl(&cache, &artifact, "host", &fixture.curl).unwrap();
        assert_eq!(
            fs::read(&path).unwrap(),
            fixture.payload,
            "seed 20260921 case {case}"
        );
        let digest = Command::new("/usr/bin/openssl")
            .args(["dgst", "-sha256", "-r"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(digest.status.success());
        assert_eq!(
            String::from_utf8(digest.stdout)
                .unwrap()
                .split_whitespace()
                .next(),
            Some(artifact.sha256.as_str())
        );
        assert_eq!(counts.downloaded_bytes, (size - expected_reused) as u64);
        assert_eq!(counts.reused_bytes, expected_reused as u64);
        assert_eq!(
            counts.resumed_bytes,
            if expected_reused > 0 {
                counts.downloaded_bytes
            } else {
                0
            }
        );
        assert_eq!(counts.downloaded_bytes + counts.reused_bytes, size as u64);
        let requests = fixture.requests();
        assert_eq!(
            requests.iter().map(|request| request.2).sum::<usize>(),
            prior_network + size - expected_reused
        );
        if expected_reused > 0 {
            assert_eq!(
                requests.last().unwrap().0.as_deref(),
                Some(format!("bytes={prefix}-").as_str())
            );
            assert_eq!(requests.last().unwrap().1.as_deref(), Some("\"native-v1\""));
        }
        let (_, warm) = acquire_with_curl(&cache, &artifact, "host", &fixture.curl).unwrap();
        assert_eq!(warm.downloaded_bytes, 0);
        assert_eq!(warm.reused_bytes, size as u64);
        assert_eq!(
            fixture.requests().len(),
            requests.len(),
            "warm request: {case}"
        );
    }
}

#[test]
fn user_curl_configuration_cannot_redirect_artifact_output() {
    let root = Workspace::new();
    let cache = cache_root(root.path()).unwrap();
    let fixture = Fixture::new(root.path());
    let injected = root.path().join("injected-output");
    fs::write(
        root.path().join(".curlrc"),
        format!("output = \"{}\"\n", injected.display()),
    )
    .unwrap();
    let original = fs::read_to_string(&fixture.curl).unwrap();
    fs::write(
        &fixture.curl,
        original.replace(
            "args=sys.argv[1:]",
            &format!(
                "args=sys.argv[1:]\nos.environ['CURL_HOME'] = {:?}",
                root.path().to_str().unwrap()
            ),
        ),
    )
    .unwrap();
    let (path, _) = acquire_with_curl(&cache, &fixture.artifact(), "host", &fixture.curl).unwrap();
    assert_eq!(fs::read(path).unwrap(), fixture.payload);
    assert!(
        !injected.exists(),
        "curl loaded user configuration before explicit policy"
    );
}

#[test]
fn ignored_and_rejected_ranges_retry_once_and_count_discarded_bytes() {
    for mode in [Mode::Ignore, Mode::Reject] {
        let root = Workspace::new();
        let cache = cache_root(root.path()).unwrap();
        let fixture = Fixture::new(root.path());
        let artifact = fixture.artifact();
        partial(&cache, &artifact, &fixture.payload[..1000]);
        fixture.mode(mode);
        let (_, counts) = acquire_with_curl(&cache, &artifact, "host", &fixture.curl).unwrap();
        let requests = fixture.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].0.as_deref(), Some("bytes=1000-"));
        assert_eq!(requests[1].0, None);
        assert_eq!(
            counts.downloaded_bytes,
            requests.iter().map(|request| request.2 as u64).sum::<u64>()
        );
        assert_eq!(counts.resumed_bytes, 0);
        assert_eq!(counts.reused_bytes, 0);
    }
}

#[test]
fn complete_partial_recovers_without_network_and_bad_digest_is_removed() {
    let root = Workspace::new();
    let cache = cache_root(root.path()).unwrap();
    let fixture = Fixture::new(root.path());
    let artifact = fixture.artifact();
    let partial_path = partial(&cache, &artifact, &fixture.payload);
    let (_, counts) = acquire_with_curl(&cache, &artifact, "guestImage", &fixture.curl).unwrap();
    assert_eq!(counts.source, "partial-cache");
    assert_eq!(counts.reused_bytes, fixture.payload.len() as u64);
    assert!(fixture.requests().is_empty());
    assert!(!partial_path.exists());
    let bad = Artifact {
        sha256: "0".repeat(64),
        ..artifact
    };
    assert!(acquire_with_curl(&cache, &bad, "host", &fixture.curl).is_err());
    assert!(
        !cache
            .join(format!("downloads/.{}.partial", bad.sha256))
            .exists()
    );
    assert!(
        !cache
            .join(format!("downloads/{}.artifact", bad.sha256))
            .exists()
    );
}

#[test]
fn oversize_and_unsafe_cache_are_never_published() {
    let root = Workspace::new();
    let cache = cache_root(root.path()).unwrap();
    let fixture = Fixture::new(root.path());
    let artifact = fixture.artifact();
    fixture.mode(Mode::Oversize);
    assert!(acquire_with_curl(&cache, &artifact, "host", &fixture.curl).is_err());
    let final_path = cache.join(format!("downloads/{}.artifact", artifact.sha256));
    assert!(!final_path.exists());
    let external = root.path().join("external");
    fs::write(&external, b"preserve").unwrap();
    symlink(&external, &final_path).unwrap();
    assert!(acquire_with_curl(&cache, &artifact, "host", &fixture.curl).is_err());
    assert_eq!(fs::read(external).unwrap(), b"preserve");
    assert_eq!(fixture.requests().len(), 1);
}

#[test]
fn legacy_partial_restarts_full_download_and_checks_digest() {
    let root = Workspace::new();
    let cache = cache_root(root.path()).unwrap();
    let fixture = Fixture::new(root.path());
    let mut artifact = fixture.artifact();
    partial(&cache, &artifact, &fixture.payload[..1000]);
    artifact.size = None;
    let (_, counts) = acquire_with_curl(&cache, &artifact, "host", &fixture.curl).unwrap();
    assert_eq!(fixture.requests()[0].0, None);
    assert_eq!(counts.downloaded_bytes, fixture.payload.len() as u64);
    assert_eq!(counts.reused_bytes, 0);
}

#[test]
fn automatic_manifest_uses_short_deadlines_and_enforces_the_256_kib_limit() {
    let root = Workspace::new();
    let fixture = Fixture::new(root.path());
    let original = fs::read_to_string(&fixture.curl).unwrap();
    fs::write(&fixture.curl, original.replace("args=sys.argv[1:]", "args=sys.argv[1:]\nassert args[args.index('--connect-timeout')+1] == '2'\nassert args[args.index('--max-time')+1] == '5'")).unwrap();
    let (bytes, count) =
        fetch_manifest_with_curl("https://fixture.test/manifest", true, &fixture.curl).unwrap();
    assert_eq!(bytes, fixture.payload);
    assert_eq!(count, MANIFEST_LIMIT);
    fixture.mode(Mode::Oversize);
    assert!(
        fetch_manifest_with_curl("https://fixture.test/manifest", true, &fixture.curl).is_err()
    );
}

#[test]
fn private_metadata_rejects_modes_links_and_keeps_original_on_unsafe_replace() {
    let root = Workspace::new();
    let path = root.path().join("record");
    atomic_json(&path, &serde_json::json!({"version":1})).unwrap();
    assert_eq!(
        safe_file(&path, true, Some(64)).unwrap().mode() & 0o7777,
        0o600
    );
    let linked = root.path().join("linked");
    fs::hard_link(&path, &linked).unwrap();
    assert!(read_file(&path, 64, true).is_err());
    fs::remove_file(linked).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
    let original = fs::read(&path).unwrap();
    assert!(atomic_json(&path, &serde_json::json!({"changed":true})).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
#[ignore = "invoked by the cross-process single-flight parent"]
fn acquire_process() {
    let cache = PathBuf::from(std::env::var_os("HAMN_TEST_NATIVE_CACHE").unwrap());
    let curl = PathBuf::from(std::env::var_os("HAMN_TEST_NATIVE_CURL").unwrap());
    let artifact: Artifact =
        serde_json::from_str(&std::env::var("HAMN_TEST_NATIVE_ARTIFACT").unwrap()).unwrap();
    let (_, counts) = acquire_with_curl(&cache, &artifact, "host", &curl).unwrap();
    atomic_json(
        &PathBuf::from(std::env::var_os("HAMN_TEST_NATIVE_COUNTS").unwrap()),
        &counts,
    )
    .unwrap();
}

#[test]
fn simultaneous_processes_download_one_payload() {
    let root = Workspace::new();
    let cache = cache_root(root.path()).unwrap();
    let fixture = Fixture::new(root.path());
    let artifact = fixture.artifact();
    let mut children = Vec::new();
    for index in 0..3 {
        let output = root.path().join(format!("counts-{index}.json"));
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                &format!(
                    "{}::acquire_process",
                    module_path!().split_once("::").unwrap().1
                ),
            ])
            .env("HAMN_TEST_NATIVE_CACHE", &cache)
            .env("HAMN_TEST_NATIVE_CURL", &fixture.curl)
            .env(
                "HAMN_TEST_NATIVE_ARTIFACT",
                serde_json::to_string(&artifact).unwrap(),
            )
            .env("HAMN_TEST_NATIVE_COUNTS", &output)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        children.push((ChildGuard(child), output));
    }
    let mut downloaded = 0;
    for (mut child, output) in children {
        assert!(child.0.wait().unwrap().success());
        let counts: Counts =
            serde_json::from_slice(&read_file(&output, 4096, true).unwrap()).unwrap();
        downloaded += counts.downloaded_bytes;
    }
    assert_eq!(fixture.requests().len(), 1);
    assert_eq!(downloaded, fixture.payload.len() as u64);
}
