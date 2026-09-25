//! Automatic resumption and stall policy for explicit acquisitions.
//!
//! A loopback HTTP server follows a per-request script (truncate after N
//! payload bytes, or drop the connection). The curl shim only rewrites the
//! synthetic HTTPS origin/protocol restrictions to loopback HTTP and records
//! the arguments production passed; it never changes transfer policy.
use super::*;
use std::{
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

#[derive(Clone, Copy)]
enum Reply {
    Full,
    /// Send headers and this many payload bytes, then close the connection.
    Truncate(usize),
    /// Close the connection before any response bytes.
    Drop,
}

struct Server {
    ranges: Arc<Mutex<Vec<Option<String>>>>,
    address: std::net::SocketAddr,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    payload: Vec<u8>,
    curl: PathBuf,
    arguments: PathBuf,
}

impl Server {
    fn new(root: &Path, script: Vec<Reply>) -> Self {
        let payload: Vec<u8> = (0..65536).map(|index| (index % 239) as u8).collect();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let script = Arc::new(Mutex::new(script));
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (plan, seen, ended, data) = (
            script.clone(),
            ranges.clone(),
            stop.clone(),
            payload.clone(),
        );
        let thread = thread::spawn(move || {
            for stream in listener.incoming() {
                if ended.load(Ordering::SeqCst) {
                    break;
                }
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap_or(0) == 0 {
                        break;
                    }
                    request.push(byte[0]);
                }
                let text = String::from_utf8(request).unwrap();
                let range = text.lines().find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("Range")
                        .then(|| value.trim().to_owned())
                });
                seen.lock().unwrap().push(range.clone());
                let reply = {
                    let mut plan = plan.lock().unwrap();
                    if plan.is_empty() {
                        Reply::Full
                    } else {
                        plan.remove(0)
                    }
                };
                if matches!(reply, Reply::Drop) {
                    continue;
                }
                let offset = range
                    .as_deref()
                    .and_then(|value| value.strip_prefix("bytes="))
                    .map(|value| value.trim_end_matches('-').parse::<usize>().unwrap())
                    .unwrap_or(0);
                let (status, content_range) = if offset == 0 {
                    (200, String::new())
                } else {
                    (
                        206,
                        format!(
                            "Content-Range: bytes {offset}-{}/{}\r\n",
                            data.len() - 1,
                            data.len()
                        ),
                    )
                };
                let body = &data[offset..];
                let head = format!(
                    "HTTP/1.1 {status} fixture\r\nContent-Length: {}\r\nETag: \"resume-v1\"\r\n{content_range}Connection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let sent = match reply {
                    Reply::Truncate(count) => &body[..count.min(body.len())],
                    _ => body,
                };
                let _ = stream.write_all(sent);
            }
        });
        let arguments = root.join("curl-arguments");
        let curl = root.join("fixture-curl");
        fs::write(
            &curl,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$@" > '{arguments}'
for argument do
    shift
    case $argument in
    =https) argument==http ;;
    https://fixture.test/*) argument=http://{address}/${{argument#https://fixture.test/}} ;;
    esac
    set -- "$@" "$argument"
done
exec /usr/bin/curl "$@"
"#,
                arguments = arguments.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&curl, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            ranges,
            address,
            stop,
            thread: Some(thread),
            payload,
            curl,
            arguments,
        }
    }

    fn artifact(&self, sized: bool) -> Artifact {
        Artifact {
            url: "https://fixture.test/artifact".into(),
            sha256: format!("{:x}", Sha256::digest(&self.payload)),
            size: sized.then_some(self.payload.len() as u64),
        }
    }

    fn ranges(&self) -> Vec<Option<String>> {
        self.ranges.lock().unwrap().clone()
    }

    fn arguments(&self) -> Vec<String> {
        fs::read_to_string(&self.arguments)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn workspace() -> (Temporary, PathBuf) {
    let (root, _) = Temporary::create(&std::env::temp_dir(), true).unwrap();
    let cache = cache_root(&root.path).unwrap();
    (root, cache)
}

fn run(server: &Server, cache: &Path, sized: bool) -> (Result<(PathBuf, Counts)>, String) {
    let mut progress = Progress::new(
        Vec::new(),
        "Downloading guest image",
        sized.then_some(65536),
        false,
    );
    let result = acquire_resuming(
        cache,
        &server.artifact(sized),
        "guestImage",
        &server.curl,
        &mut progress,
        RESUME_ATTEMPTS,
        Duration::ZERO,
    );
    (result, String::from_utf8(progress.into_writer()).unwrap())
}

#[test]
fn interrupted_transfers_resume_automatically_and_count_every_network_byte() {
    let (root, cache) = workspace();
    let server = Server::new(
        &root.path,
        vec![Reply::Truncate(1000), Reply::Truncate(3000)],
    );
    let (result, output) = run(&server, &cache, true);
    let (path, counts) = result.unwrap();
    assert_eq!(fs::read(path).unwrap(), server.payload);
    assert_eq!(
        server.ranges(),
        [None, Some("bytes=1000-".into()), Some("bytes=4000-".into())]
    );
    // Every payload byte crossed the network exactly once in this run.
    assert_eq!(counts.downloaded_bytes, 65536);
    assert_eq!(counts.resumed_bytes, 65536 - 1000);
    assert_eq!(counts.reused_bytes, 0);
    assert!(output.contains("resuming (1 of 3)"), "{output}");
    assert!(output.contains("resuming (2 of 3)"), "{output}");
    assert!(!output.contains("resuming (3 of 3)"), "{output}");
}

#[test]
fn bytes_from_an_earlier_run_stay_reused_while_this_run_resumes() {
    let (root, cache) = workspace();
    let server = Server::new(&root.path, vec![Reply::Truncate(2000)]);
    let artifact = server.artifact(true);
    let downloads = cache.join("downloads");
    directory(&downloads, 0o700).unwrap();
    let partial = downloads.join(format!(".{}.partial", artifact.sha256));
    fs::write(&partial, &server.payload[..500]).unwrap();
    fs::set_permissions(&partial, fs::Permissions::from_mode(0o600)).unwrap();
    save_partial_metadata(
        &downloads.join(format!(".{}.validator", artifact.sha256)),
        &PartialMetadata {
            sha256: artifact.sha256.clone(),
            size: 65536,
            validator: Some("\"resume-v1\"".into()),
        },
    )
    .unwrap();
    let (result, _) = run(&server, &cache, true);
    let (_, counts) = result.unwrap();
    assert_eq!(
        server.ranges(),
        [Some("bytes=500-".into()), Some("bytes=2500-".into())]
    );
    assert_eq!(counts.reused_bytes, 500);
    assert_eq!(counts.downloaded_bytes, 65536 - 500);
    assert_eq!(counts.resumed_bytes, 65536 - 500);
}

#[test]
fn a_transfer_without_progress_fails_immediately() {
    let (root, cache) = workspace();
    let server = Server::new(&root.path, vec![Reply::Drop]);
    let (result, output) = run(&server, &cache, true);
    let error = result.unwrap_err().to_string();
    assert!(error.starts_with("download failed: "), "{error}");
    assert_eq!(server.ranges().len(), 1);
    assert!(!output.contains("resuming"), "{output}");
}

#[test]
fn resumption_is_bounded_and_keeps_the_partial_for_the_next_run() {
    let (root, cache) = workspace();
    let server = Server::new(&root.path, vec![Reply::Truncate(100); 8]);
    let (result, output) = run(&server, &cache, true);
    assert!(result.is_err());
    assert_eq!(server.ranges().len(), 1 + RESUME_ATTEMPTS as usize);
    assert!(output.contains("resuming (3 of 3)"), "{output}");
    let partial = cache.join(format!(
        "downloads/.{}.partial",
        server.artifact(true).sha256
    ));
    assert_eq!(fs::read(partial).unwrap(), server.payload[..400]);
}

#[test]
fn unsized_schema_v2_artifacts_are_not_resumed() {
    let (root, cache) = workspace();
    let server = Server::new(&root.path, vec![Reply::Truncate(1000)]);
    let (result, _) = run(&server, &cache, false);
    assert!(result.is_err());
    assert_eq!(server.ranges(), [None]);
    let partial = cache.join(format!(
        "downloads/.{}.partial",
        server.artifact(false).sha256
    ));
    assert!(!partial.exists());
}

#[test]
fn explicit_transfers_fail_on_stalls_instead_of_a_fixed_total_deadline() {
    let (root, cache) = workspace();
    let server = Server::new(&root.path, Vec::new());
    run(&server, &cache, true).0.unwrap();
    let arguments = server.arguments();
    let value = |name: &str| {
        let index = arguments
            .iter()
            .position(|argument| argument == name)
            .unwrap_or_else(|| panic!("missing {name}"));
        arguments[index + 1].clone()
    };
    assert_eq!(value("--speed-limit"), STALL_BYTES_PER_SECOND);
    assert_eq!(value("--speed-time"), STALL_SECONDS);
    assert_eq!(value("--max-time"), EXPLICIT_MAX_SECONDS);
    assert_eq!(arguments[0], "--disable");

    fetch_manifest_with_curl("https://fixture.test/manifest", true, &server.curl).unwrap();
    let automatic = server.arguments();
    assert!(!automatic.iter().any(|argument| argument == "--speed-limit"));
    assert_eq!(
        automatic[automatic.iter().position(|a| a == "--max-time").unwrap() + 1],
        "5"
    );
}

#[test]
fn curl_failures_are_described_for_people() {
    assert_eq!(
        curl_failure(Some(6)),
        "download failed: could not resolve the release server (curl exit 6)"
    );
    assert_eq!(
        curl_failure(Some(28)),
        "download failed: the connection stalled or timed out (curl exit 28)"
    );
    assert_eq!(
        curl_failure(Some(99)),
        "download failed: the transfer failed (curl exit 99)"
    );
    assert_eq!(curl_failure(None), "download failed: the transfer failed");
}
