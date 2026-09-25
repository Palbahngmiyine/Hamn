//! Opt-in measurements of the private native guest-image acquisition
//! (`hamn __install-support upgrade acquire`) with real image files and a
//! loopback HTTPS server: a cold and a warm cache, a transfer interrupted on
//! every attempt (the command's automatic resumptions included) and its
//! resumption by a rerun, and three concurrent clients sharing one transfer.
//! Not a gate; it writes an evidence file:
//!
//! ```text
//! hamn-dev test measure-upgrade-download --binary PATH --image LABEL=PATH \
//!     [--image LABEL=PATH]... [--partial-bytes N] --output EVIDENCE.json
//! ```
//!
//! HTTP payload bytes exclude TLS and HTTP framing. Logical bytes and
//! st_blocks*512 are file accounting, not APFS physical sharing or exclusive
//! disk use. Only private native acquisition runs: never installation, a VM
//! or a public URL.
use crate::support::http::{self, Options, Reply, Request, Server, Stream};
use crate::support::tmp::TempDir;
use crate::support::upgrade::{self, Group};
use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CHUNK: usize = 64 * 1024;
/// An interrupted transfer that persisted new bytes is resumed with Range
/// up to three more times in the same command (docs/INSTALLATION.md).
const AUTOMATIC_RESUMPTIONS: u64 = 3;
const USAGE: &str = "usage: hamn-dev test measure-upgrade-download --binary PATH --image LABEL=PATH [--image LABEL=PATH]... \
                     [--partial-bytes N] --output EVIDENCE.json";

struct Arguments {
    binary: PathBuf,
    images: Vec<(String, PathBuf)>,
    partial_bytes: u64,
    output: PathBuf,
}

pub fn main(args: &[String]) -> ExitCode {
    match parse(args) {
        Ok(arguments) => crate::support::exec::python_exit(|| measure_all(&arguments)),
        Err(message) => {
            eprintln!("measure-upgrade-download: {message}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn parse(args: &[String]) -> Result<Arguments, String> {
    let (mut binary, mut images, mut partial_bytes, mut output) = (None, Vec::<(String, PathBuf)>::new(), 64 << 20, None);
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => (name, Some(value.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| rest.next().cloned()).ok_or_else(|| format!("{name} requires a value"));
        match name {
            "--binary" => binary = Some(canonical(&value()?)?),
            "--image" => {
                let item = value()?;
                let (label, path) = item.split_once('=').ok_or_else(|| format!("--image {item:?} is not LABEL=PATH"))?;
                if label.is_empty() || !label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-') {
                    return Err(format!("image label {label:?} must be ASCII letters, digits or '-'"));
                }
                if images.iter().any(|(existing, _)| existing == label) {
                    return Err(format!("image label {label:?} is repeated"));
                }
                images.push((label.to_owned(), canonical(path)?));
            }
            "--partial-bytes" => {
                partial_bytes = value()?.parse::<u64>().map_err(|error| format!("--partial-bytes: {error}"))?;
                if partial_bytes == 0 {
                    return Err("--partial-bytes must be positive".into());
                }
            }
            "--output" => output = Some(PathBuf::from(value()?)),
            _ => return Err(format!("unexpected argument {arg:?}")),
        }
    }
    if images.is_empty() {
        return Err("at least one --image is required".into());
    }
    Ok(Arguments {
        binary: binary.ok_or("--binary is required")?,
        images,
        partial_bytes,
        output: output.ok_or("--output is required")?,
    })
}

fn canonical(path: &str) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|error| format!("{path}: {error}"))
}

fn measure_all(arguments: &Arguments) -> ExitCode {
    let root = upgrade::checkout();
    let binary = &arguments.binary;
    let version = upgrade::run(Command::new(binary).arg("--version"), Duration::from_secs(10));
    assert_eq!(version.returncode, 0, "{version:?}");
    let sources = [
        "tools/hamn-dev/src/suites/measure_upgrade_download.rs",
        "control/install_support/download.rs",
        "control/install_support/manifest.rs",
        "control/install_support/upgrade.rs",
    ];
    let mut evidence = json!({
        "schemaVersion": 1,
        "measuredAt": utc_now(),
        "candidateBinarySHA256": upgrade::file_digest(binary),
        "candidateVersion": version.stdout().trim(),
        "platform": platform(),
        "scope": "private native acquire only; no bootstrap, installer, or VM",
        "networkAccounting": "HTTP response payload bytes; excludes HTTP/TLS framing",
        "storageAccounting": "logical size and st_blocks*512; not exclusive physical APFS allocation",
        "sourceBinding": "source hashes describe the observed checkout; supplied binary build provenance is external",
        "sourceSHA256": sources.iter().map(|path| (path.to_string(), upgrade::file_digest(&root.join(path)).into()))
            .collect::<serde_json::Map<String, Value>>(),
        "artifacts": [],
    });
    let temporary = TempDir::new("hamn-acquire-measure-");
    let work = fs::canonicalize(temporary.path()).unwrap();
    let (cert, key) = http::certificate(&work);
    for (label, image) in &arguments.images {
        let report = measure(binary, image, label, &work, (&cert, &key), arguments.partial_bytes);
        evidence["artifacts"].as_array_mut().unwrap().push(report);
        fs::write(&arguments.output, format!("{}\n", serde_json::to_string_pretty(&evidence).unwrap()))
            .unwrap_or_else(|error| panic!("{}: {error}", arguments.output.display()));
    }
    let artifacts: Vec<Value> = evidence["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| json!({"label": item["label"], "imageSHA256": item["imageSHA256"], "compressedBytes": item["compressedBytes"]}))
        .collect();
    let summary = json!({
        "evidence": fs::canonicalize(&arguments.output).unwrap(),
        "candidateBinarySHA256": evidence["candidateBinarySHA256"],
        "artifacts": artifacts,
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
    ExitCode::SUCCESS
}

/// The loopback HTTPS artifact server's shared state: the scenario that
/// requests are attributed to, an optional truncation of each response
/// body, the recorded requests, and a barrier that holds every request
/// until released (a gated scenario).
#[derive(Default)]
struct Shared {
    case: String,
    interrupt_at: Option<u64>,
    requests: Vec<Value>,
    ready: bool,
    released: bool,
}

struct TransferServer {
    url: String,
    shared: Arc<(Mutex<Shared>, Condvar)>,
    _server: Server,
}

impl TransferServer {
    fn start(artifact: &Path, sha256: &str, (cert, key): (&Path, &Path)) -> Self {
        let size = fs::metadata(artifact).unwrap().len();
        let shared = Arc::new((Mutex::new(Shared { released: true, ..Shared::default() }), Condvar::new()));
        let (state, file, tag) = (Arc::clone(&shared), artifact.to_path_buf(), format!("\"{sha256}\""));
        let options = Options { tls: Some((cert.to_path_buf(), key.to_path_buf())), ..Options::default() };
        let server = Server::tcp(options, move |request| {
            let (state, file, tag, request) = (Arc::clone(&state), file.clone(), tag.clone(), request.clone());
            Reply::Raw(Box::new(move |stream| serve(&state, &file, size, &tag, &request, stream)))
        });
        Self { url: format!("{}/artifact", server.url("https")), shared, _server: server }
    }

    /// Starts attributing requests to `case`.
    fn begin(&self, case: &str, interrupt_at: Option<u64>, gated: bool) {
        let mut state = self.shared.0.lock().unwrap();
        state.case = case.to_owned();
        state.interrupt_at = interrupt_at;
        state.ready = false;
        state.released = !gated;
        self.shared.1.notify_all();
    }

    fn wait_ready(&self, timeout: Duration) -> bool {
        let state = self.shared.0.lock().unwrap();
        self.shared.1.wait_timeout_while(state, timeout, |state| !state.ready).unwrap().0.ready
    }

    fn release(&self) {
        self.shared.0.lock().unwrap().released = true;
        self.shared.1.notify_all();
    }

    fn observations(&self, case: &str) -> Value {
        let state = self.shared.0.lock().unwrap();
        let requests: Vec<Value> = state.requests.iter().filter(|item| item["case"] == case).cloned().collect();
        assert!(!requests.iter().any(|item| item.get("error").is_some()), "{requests:?}");
        let payload: u64 = requests.iter().map(|item| item["payloadBytes"].as_u64().unwrap()).sum();
        json!({"requests": requests, "requestCount": requests.len(), "httpPayloadBytes": payload})
    }
}

impl Drop for TransferServer {
    fn drop(&mut self) {
        self.release();
    }
}

/// Answers one artifact request (optionally a `bytes=START-` range guarded by
/// the artifact's ETag) and records it, with any failure, in `shared`.
fn serve(shared: &(Mutex<Shared>, Condvar), file: &Path, size: u64, tag: &str, request: &Request, stream: &mut dyn Stream) {
    let (lock, changed) = shared;
    let (case, interrupt_at) = {
        let state = lock.lock().unwrap();
        (state.case.clone(), state.interrupt_at)
    };
    let range = request.header("Range").map(str::to_owned);
    let mut record = json!({"case": case, "range": range, "ifRange": request.header("If-Range"), "payloadBytes": 0});
    let mut respond = || -> Result<(), String> {
        if request.path() != "/artifact" {
            return Err(format!("unexpected path {}", request.target));
        }
        let (start, status) = match &range {
            Some(range) => {
                let start = range
                    .strip_prefix("bytes=")
                    .and_then(|range| range.strip_suffix('-'))
                    .and_then(|start| start.parse::<u64>().ok())
                    .ok_or_else(|| format!("unexpected range {range:?}"))?;
                if !(0 < start && start < size) {
                    return Err(format!("range start {start} outside 1..{size}"));
                }
                if request.header("If-Range") != Some(tag) {
                    return Err(format!("If-Range {:?} is not {tag}", request.header("If-Range")));
                }
                (start, 206)
            }
            None => (0, 200),
        };
        record["status"] = status.into();
        {
            let mut state = lock.lock().unwrap();
            state.ready = true;
            changed.notify_all();
            let (state, _) = changed.wait_timeout_while(state, Duration::from_secs(30), |state| !state.released).unwrap();
            if !state.released {
                return Err("concurrent client barrier timed out".into());
            }
        }
        let mut head = format!(
            "HTTP/1.1 {status} {}\r\nContent-Length: {}\r\nETag: {tag}\r\nConnection: close\r\n",
            if status == 206 { "Partial Content" } else { "OK" },
            size - start
        );
        if status == 206 {
            head.push_str(&format!("Content-Range: bytes {start}-{}/{size}\r\n", size - 1));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).map_err(|error| error.to_string())?;
        let mut remaining = (size - start).min(interrupt_at.unwrap_or(u64::MAX));
        let mut source = File::open(file).map_err(|error| error.to_string())?;
        source.seek(SeekFrom::Start(start)).map_err(|error| error.to_string())?;
        let mut buffer = vec![0; CHUNK];
        while remaining > 0 {
            let wanted = remaining.min(CHUNK as u64) as usize;
            let count = source.read(&mut buffer[..wanted]).map_err(|error| error.to_string())?;
            if count == 0 {
                return Err("artifact truncated during measurement".into());
            }
            stream.write_all(&buffer[..count]).map_err(|error| error.to_string())?;
            record["payloadBytes"] = (record["payloadBytes"].as_u64().unwrap() + count as u64).into();
            remaining -= count as u64;
        }
        stream.flush().map_err(|error| error.to_string())
    };
    if let Err(error) = respond() {
        record["error"] = error.into();
    }
    lock.lock().unwrap().requests.push(record);
}

/// Every regular file below `path`, with its logical and allocated sizes.
fn storage(path: &Path) -> Value {
    fn walk(root: &Path, directory: &Path, entries: &mut Vec<(String, u64, u64)>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let info = fs::symlink_metadata(&path).unwrap();
            assert!(!info.file_type().is_symlink(), "unexpected cache symlink {}", path.display());
            if info.is_dir() {
                walk(root, &path, entries);
            } else if info.is_file() {
                let name = path.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                entries.push((name, info.len(), info.blocks() * 512));
            }
        }
    }
    let mut entries = Vec::new();
    walk(path, path, &mut entries);
    entries.sort();
    let files: Vec<Value> = entries
        .iter()
        .map(|(file, logical, allocated)| json!({"file": file, "logicalBytes": logical, "allocatedBlockBytes": allocated}))
        .collect();
    json!({
        "logicalBytes": entries.iter().map(|entry| entry.1).sum::<u64>(),
        "allocatedBlockBytes": entries.iter().map(|entry| entry.2).sum::<u64>(),
        "files": files,
    })
}

fn measure(binary: &Path, image: &Path, label: &str, root: &Path, tls: (&Path, &Path), partial_bytes: u64) -> Value {
    let size = fs::metadata(image).unwrap().len();
    let sha256 = upgrade::file_digest(image);
    assert!(1 < size && size < 2 << 30, "measurement requires a guest artifact below 2 GiB");
    // Every response of the interrupted scenario stops after `offset` bytes,
    // so each automatic resumption persists new bytes and the transfer
    // fails only once they are exhausted, short of the whole artifact.
    let offset = partial_bytes.min((size - 1) / (1 + AUTOMATIC_RESUMPTIONS));
    assert!(offset > 0, "the artifact is too small to interrupt {} times", 1 + AUTOMATIC_RESUMPTIONS);
    let retained = (1 + AUTOMATIC_RESUMPTIONS) * offset;
    let server = TransferServer::start(image, &sha256, tls);
    let manifest = root.join(format!("manifest-{label}.json"));
    upgrade::write_json(
        &manifest,
        &json!({"schemaVersion": 3, "channel": "stable", "version": "v1.0.0", "commit": "a".repeat(40),
            "validationMode": "github-hosted-no-vm",
            "compatibility": {"os": "darwin", "architecture": "arm64", "minimumMacOS": "13.0"},
            "artifacts": {"host": {"url": server.url, "sha256": "0".repeat(64), "size": 1},
                "guestImage": {"url": server.url, "sha256": sha256, "size": size, "format": "qcow2",
                    "compression": "zlib", "virtualSize": 8_u64 << 30}}}),
    );
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o600)).unwrap();
    let mut report = json!({"label": label, "imageSHA256": sha256, "compressedBytes": size, "scenarios": {}});
    let spawn = |cache: &Path, counts: &Path| {
        let mut command = Command::new(binary);
        command
            .args(["__install-support", "upgrade", "acquire"])
            .arg(&manifest)
            .arg("guestImage")
            .arg(cache)
            .arg(counts)
            .env("HOME", root)
            .env("CURL_CA_BUNDLE", tls.0)
            .env("SSL_CERT_FILE", tls.0)
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1");
        Group::spawn(&mut command)
    };
    let complete = |child: Group, counts: &Path, success: bool| -> Value {
        let output = child.finish(Duration::from_secs(180));
        assert_eq!(output.returncode == 0, success, "{output:?}");
        if !success {
            assert!(!counts.exists(), "a failed transfer reported completed counts");
            return json!({"exitCode": output.returncode, "error": output.stderr().trim()});
        }
        let result: Value = serde_json::from_slice(&fs::read(counts).unwrap()).unwrap();
        let artifact = PathBuf::from(output.stdout().trim());
        assert_eq!(fs::metadata(&artifact).unwrap().len(), size);
        assert_eq!(upgrade::file_digest(&artifact), sha256);
        result
    };
    let case_root = |name: &str| {
        let directory = TempDir::new_in(root, &format!("{label}-{name}-"));
        let cache = directory.path().join("cache");
        fs::create_dir(&cache).unwrap();
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o755)).unwrap();
        (directory, cache)
    };
    let mut record = |name: &str, cache: &Path, counts: Value, started: Instant, extra: Value| -> Value {
        let mut value = server.observations(name);
        eprintln!("{label}/{name}: {} requests, {} payload bytes", value["requestCount"], value["httpPayloadBytes"]);
        value["nativeCounts"] = counts;
        value["cache"] = storage(cache);
        value["elapsedSeconds"] = ((started.elapsed().as_secs_f64() * 1e6).round() / 1e6).into();
        for (key, item) in extra.as_object().unwrap() {
            value[key] = item.clone();
        }
        report["scenarios"][name] = value.clone();
        value
    };

    let (directory, cache) = case_root("cache");
    for name in ["cold", "warm"] {
        server.begin(name, None, false);
        let started = Instant::now();
        let path = directory.path().join(format!("{name}-counts.json"));
        let counts = complete(spawn(&cache, &path), &path, true);
        let result = record(name, &cache, counts.clone(), started, json!({}));
        let expected = if name == "cold" { size } else { 0 };
        assert_eq!(result["httpPayloadBytes"], expected);
        assert_eq!(counts["downloadedBytes"], expected);
        assert_eq!(result["requestCount"], if name == "cold" { 1 } else { 0 });
        assert_eq!(counts["source"], if name == "cold" { "download" } else { "cache" });
        assert_eq!(counts["reusedBytes"], if name == "cold" { 0 } else { size });
    }
    drop(directory);

    let (directory, cache) = case_root("resume");
    server.begin("interrupted", Some(offset), false);
    let started = Instant::now();
    let path = directory.path().join("interrupted-counts.json");
    let failure = complete(spawn(&cache, &path), &path, false);
    let partial = cache.join("downloads").join(format!(".{sha256}.partial"));
    assert_eq!(fs::metadata(&partial).unwrap().len(), retained);
    let interrupted = record(
        "interrupted",
        &cache,
        Value::Null,
        started,
        json!({"failure": failure, "responseBytes": offset, "offsetBytes": retained}),
    );
    assert_eq!(interrupted["requestCount"], 1 + AUTOMATIC_RESUMPTIONS);
    assert_eq!(interrupted["httpPayloadBytes"], retained);
    let ranges: Vec<Value> = (0..=AUTOMATIC_RESUMPTIONS)
        .map(|attempt| if attempt == 0 { Value::Null } else { format!("bytes={}-", attempt * offset).into() })
        .collect();
    let observed: Vec<Value> = interrupted["requests"].as_array().unwrap().iter().map(|item| item["range"].clone()).collect();
    assert_eq!(observed, ranges);
    // Rerunning the command resumes the safe partial.
    server.begin("resumed", None, false);
    let started = Instant::now();
    let path = directory.path().join("resumed-counts.json");
    let counts = complete(spawn(&cache, &path), &path, true);
    let result = record("resumed", &cache, counts.clone(), started, json!({"offsetBytes": retained}));
    assert_eq!(result["requestCount"], 1);
    assert_eq!(result["httpPayloadBytes"], size - retained);
    assert_eq!(counts["downloadedBytes"], size - retained);
    assert_eq!(counts["resumedBytes"], size - retained);
    assert_eq!(counts["reusedBytes"], retained);
    assert_eq!(counts["source"], "resumed");
    assert_eq!(result["requests"][0]["range"], format!("bytes={retained}-"));
    assert!(!partial.exists());
    drop(directory);

    let (directory, cache) = case_root("concurrent");
    server.begin("concurrent", None, true);
    let started = Instant::now();
    let mut peers: Vec<(PathBuf, Group)> = (0..3)
        .map(|index| {
            let path = directory.path().join(format!("counts-{index}.json"));
            let child = spawn(&cache, &path);
            (path, child)
        })
        .collect();
    assert!(server.wait_ready(Duration::from_secs(15)), "native clients never reached the loopback server");
    assert!(peers.iter_mut().all(|(_, child)| child.running()), "a peer exited before the shared transfer began");
    server.release();
    let counts: Vec<Value> = peers.into_iter().map(|(path, child)| complete(child, &path, true)).collect();
    let result = record("concurrent", &cache, Value::from(counts.clone()), started, json!({}));
    assert_eq!(result["requestCount"], 1);
    assert_eq!(result["httpPayloadBytes"], size);
    assert_eq!(counts.iter().map(|value| value["downloadedBytes"].as_u64().unwrap()).sum::<u64>(), size);
    let mut sources: Vec<&str> = counts.iter().map(|value| value["source"].as_str().unwrap()).collect();
    sources.sort();
    assert_eq!(sources, ["cache", "cache", "download"]);
    assert_eq!(counts.iter().map(|value| value["reusedBytes"].as_u64().unwrap()).sum::<u64>(), 2 * size);
    let artifacts = fs::read_dir(cache.join("downloads"))
        .unwrap()
        .filter(|entry| entry.as_ref().unwrap().file_name().to_string_lossy().ends_with(".artifact"))
        .count();
    assert_eq!(artifacts, 1);
    drop(directory);
    report
}

/// The current UTC time as ISO 8601 with microseconds and a +00:00 offset.
fn utc_now() -> String {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let seconds = elapsed.as_secs() as i64;
    let (days, time) = (seconds.div_euclid(86400), seconds.rem_euclid(86400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:06}+00:00",
        time / 3600,
        time % 3600 / 60,
        time % 60,
        elapsed.subsec_micros()
    )
}

/// The macOS product version and machine, for example `macOS-15.0-arm64`.
fn platform() -> String {
    let product = upgrade::run(Command::new("/usr/bin/sw_vers").arg("-productVersion"), Duration::from_secs(10));
    let machine = upgrade::run(Command::new("/usr/bin/uname").arg("-m"), Duration::from_secs(10));
    format!("macOS-{}-{}", product.stdout().trim(), machine.stdout().trim())
}
