//! Bounded HTTPS acquisition and private content-addressed release storage.
//!
//! Sizes and counters are bytes. Only content with the exact declared size
//! and SHA-256 is published; per-digest advisory locks cover lookup, transfer
//! and atomic publication. Network interruption retains the partial for a
//! resumed transfer; integrity failures discard it.
//! No generation, profile, VM or guest-selection mutations belong here.
use super::{
    Result,
    progress::{self, Progress},
    require,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

pub(super) const MANIFEST_LIMIT: u64 = 256 * 1024;
pub(super) const HOST_LIMIT: u64 = 128 * 1024 * 1024;
pub(super) const GUEST_LIMIT: u64 = 2 * 1024 * 1024 * 1024 - 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Artifact {
    pub url: String,
    pub sha256: String,
    /// Exact byte size; acquisition rejects 0 and sizes above the kind's limit.
    pub size: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Counts {
    pub downloaded_bytes: u64,
    /// Subset of downloaded_bytes, never an additional network byte count.
    pub resumed_bytes: u64,
    pub reused_bytes: u64,
    pub source: String,
}

pub(super) fn safe_file(path: &Path, private: bool, limit: Option<u64>) -> Result<Metadata> {
    let info = fs::symlink_metadata(path)?;
    validate_file(&info, private, limit)?;
    Ok(info)
}

fn validate_file(info: &Metadata, private: bool, limit: Option<u64>) -> Result<()> {
    require(
        info.is_file() && info.uid() == unsafe { libc::geteuid() } && info.nlink() == 1,
        "unsafe owned regular file",
    )?;
    require(
        info.mode() & 0o022 == 0 && (!private || info.mode() & 0o7777 == 0o600),
        "unsafe file permissions",
    )?;
    require(
        limit.is_none_or(|limit| info.len() <= limit),
        "file exceeds size limit",
    )
}

fn open_owned(path: &Path, private: bool, limit: Option<u64>) -> Result<File> {
    let expected = safe_file(path, private, limit)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let actual = file.metadata()?;
    validate_file(&actual, private, limit)?;
    require(
        actual.dev() == expected.dev() && actual.ino() == expected.ino(),
        "file changed while opening",
    )?;
    Ok(file)
}

pub(super) fn read_file(path: &Path, limit: u64, private: bool) -> Result<Vec<u8>> {
    let file = open_owned(path, private, Some(limit))?;
    let mut bytes = Vec::new();
    file.take(limit.checked_add(1).ok_or("read limit overflow")?)
        .read_to_end(&mut bytes)?;
    require(bytes.len() as u64 <= limit, "file exceeds size limit")?;
    Ok(bytes)
}

pub(super) fn directory(path: &Path, mode: u32) -> Result<()> {
    match fs::DirBuilder::new().mode(mode).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let info = fs::symlink_metadata(path)?;
    require(
        info.is_dir() && info.uid() == unsafe { libc::geteuid() } && info.mode() & 0o022 == 0,
        "unsafe cache directory",
    )?;
    require(
        mode != 0o700 || info.mode() & 0o7777 == 0o700,
        "cache directory must be private",
    )
}

pub(super) fn cache_root(home: &Path) -> Result<PathBuf> {
    let root = home.join(".hamn");
    directory(&root, 0o700)?;
    let cache = root.join("cache");
    directory(&cache, 0o755)?;
    Ok(cache)
}

pub(super) fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Own one exclusive temporary path; unlink it on every ordinary error path.
struct Temporary {
    path: PathBuf,
    directory: bool,
}

impl Temporary {
    fn create(parent: &Path, directory: bool) -> Result<(Self, Option<File>)> {
        for _ in 0..128 {
            let path = parent.join(format!(
                ".hamn-transfer-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            let result = if directory {
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&path)
                    .map(|_| None)
            } else {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)
                    .map(Some)
            };
            match result {
                Ok(file) => return Ok((Self { path, directory }, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err("cannot create private transfer workspace".into())
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        if self.directory {
            let _ = fs::remove_dir_all(&self.path);
        } else {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn remove_optional(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    atomic_bytes(path, &bytes)
}

/// Private one-line text record (for example a failure reason handoff).
pub(super) fn atomic_text(path: &Path, text: &str) -> Result<()> {
    let mut bytes = text.replace('\n', " ").into_bytes();
    bytes.push(b'\n');
    atomic_bytes(path, &bytes)
}

fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if exists(path)? {
        safe_file(path, true, None)?;
    }
    let parent = path.parent().ok_or("missing metadata parent")?;
    let (stage, file) = Temporary::create(parent, false)?;
    let mut file = file.ok_or("missing temporary file")?;
    file.write_all(bytes)?;
    file.sync_all()?;
    // Recheck the replacement target after serialization; never follow a link.
    if exists(path)? {
        safe_file(path, true, None)?;
    }
    fs::rename(&stage.path, path)?;
    sync_directory(parent)
}

/// Own the critical section, including when another thread forks while its
/// descriptor is open. Closing only our descriptor would let the inherited
/// open-file description keep the lock alive until that child exits or execs.
pub(super) struct DownloadLock {
    file: File,
}

impl Drop for DownloadLock {
    fn drop(&mut self) {
        loop {
            // The descriptor is owned by this guard and remains open throughout
            // Drop. Unlock the shared description before File closes our copy.
            if unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) } == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            assert_eq!(
                error.kind(),
                io::ErrorKind::Interrupted,
                "cannot unlock release cache: {error}"
            );
        }
    }
}

/// The returned guard owns the lock until dropped. Nonblocking contention is
/// an error; callers may treat it as an already-running automatic refresh.
pub(super) fn lock(path: &Path, blocking: bool) -> Result<DownloadLock> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let identity = file.metadata()?;
    validate_file(&identity, true, None)?;
    let operation = libc::LOCK_EX | if blocking { 0 } else { libc::LOCK_NB };
    if unsafe { libc::flock(file.as_raw_fd(), operation) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    // Every error after successful acquisition must explicitly unlock too,
    // including a changed or unsafe path discovered during identity validation.
    let guard = DownloadLock { file };
    let current = safe_file(path, true, None)?;
    require(
        identity.dev() == current.dev() && identity.ino() == current.ino(),
        "cache lock changed while locking",
    )?;
    Ok(guard)
}

pub(super) fn digest(path: &Path) -> Result<String> {
    let mut file = open_owned(path, false, None)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn verified(path: &Path, artifact: &Artifact, limit: u64) -> Result<bool> {
    if !exists(path)? {
        return Ok(false);
    }
    let info = safe_file(path, false, Some(limit))?;
    Ok(artifact.size == info.len() && digest(path)? == artifact.sha256)
}

fn local_source(url: &str) -> Option<PathBuf> {
    if std::env::var("HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS").as_deref() != Ok("1") {
        return None;
    }
    url.strip_prefix("file://")
        .or_else(|| url.starts_with('/').then_some(url))
        .map(PathBuf::from)
}

pub(super) fn validate_url(url: &str) -> Result<()> {
    require(
        !url.is_empty() && url.bytes().all(|byte| (33..=126).contains(&byte)),
        "artifact URL is invalid",
    )?;
    if local_source(url).is_some() {
        return Ok(());
    }
    require(
        !url.starts_with("file://") && !url.starts_with('/'),
        "local artifacts are disabled",
    )?;
    let authority = url
        .strip_prefix("https://")
        .ok_or("artifact URL must use HTTPS")?
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    require(
        !authority.is_empty()
            && !authority.contains('@')
            && !url.contains('#')
            && !url.contains('\\'),
        "artifact URL must use HTTPS",
    )?;
    let port = if let Some(literal) = authority.strip_prefix('[') {
        let (host, suffix) = literal.split_once(']').ok_or("invalid HTTPS authority")?;
        require(
            host.parse::<std::net::Ipv6Addr>().is_ok(),
            "invalid HTTPS host",
        )?;
        if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').ok_or("invalid HTTPS authority")?)
        }
    } else {
        let (host, port) = authority
            .split_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)));
        require(
            !host.is_empty() && !host.contains(['[', ']']),
            "invalid HTTPS host",
        )?;
        port
    };
    require(
        port.is_none_or(|value| value.is_empty() || value.parse::<u16>().is_ok()),
        "invalid HTTPS port",
    )
}

#[derive(Default)]
struct Headers {
    status: u16,
    fields: BTreeMap<String, String>,
}

fn response_headers(path: &Path) -> Result<Headers> {
    // curl bounds individual headers; also bound all redirect response blocks.
    let bytes = read_file(path, 1024 * 1024, true)?;
    let text: String = bytes.iter().map(|byte| char::from(*byte)).collect();
    let mut result = Headers::default();
    for line in text.lines() {
        if line.starts_with("HTTP/") {
            result.fields.clear();
            result.status = line
                .split_whitespace()
                .nth(1)
                .ok_or("invalid HTTP status")?
                .parse()?;
        } else if let Some((name, value)) = line.split_once(':') {
            result
                .fields
                .insert(name.to_ascii_lowercase(), value.trim().into());
        }
    }
    Ok(result)
}

fn valid_validator(value: &str) -> bool {
    !value.is_empty() && value.len() <= 1024 && value.bytes().all(|byte| (32..=126).contains(&byte))
}

fn validator(headers: &Headers) -> Option<String> {
    headers
        .fields
        .get("etag")
        .or_else(|| headers.fields.get("last-modified"))
        .filter(|value| valid_validator(value))
        .cloned()
}

fn valid_range(headers: &Headers, offset: u64, expected: u64) -> bool {
    let Some(value) = headers
        .fields
        .get("content-range")
        .and_then(|value| value.strip_prefix("bytes "))
    else {
        return false;
    };
    let Some((range, total)) = value.split_once('/') else {
        return false;
    };
    let Some((start, end)) = range.split_once('-') else {
        return false;
    };
    headers.status == 206
        && [start, end, total]
            .iter()
            .all(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        && start.parse::<u64>() == Ok(offset)
        && total.parse::<u64>() == Ok(expected)
        && end.parse::<u64>().ok().and_then(|end| end.checked_add(1)) == Some(expected)
}

enum TransferFailure {
    Invalid(Box<dyn std::error::Error>),
    Interrupted(Box<dyn std::error::Error>),
    RangeRejected(u64),
}

impl From<io::Error> for TransferFailure {
    fn from(error: io::Error) -> Self {
        Self::Interrupted(error.into())
    }
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        // Kill only this owned curl. It has no descendants, and must be reaped
        // even on oversized output, header validation, or local write failure.
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

struct Transfer {
    received: u64,
    written: u64,
}

/// Explicit transfers fail when stalled below 1 KiB/s for 60 seconds, not
/// after a fixed total that a slow but healthy link cannot meet. The absolute
/// six-hour bound still terminates a pathological trickle.
const STALL_BYTES_PER_SECOND: &str = "1024";
const STALL_SECONDS: &str = "60";
const EXPLICIT_MAX_SECONDS: &str = "21600";

/// Describe a curl exit status for people; the transfer policy is unchanged.
fn curl_failure(code: Option<i32>) -> String {
    let reason = match code {
        Some(6) => "could not resolve the release server",
        Some(7) => "could not connect to the release server",
        Some(22) => "the release server returned an HTTP error",
        Some(28) => "the connection stalled or timed out",
        Some(35 | 60) => "a secure (TLS) connection could not be established",
        Some(18 | 52 | 56) => "the connection was interrupted",
        _ => "the transfer failed",
    };
    match code {
        Some(code) => format!("download failed: {reason} (curl exit {code})"),
        None => format!("download failed: {reason}"),
    }
}

fn transfer(
    url: &str,
    destination: &Path,
    limit: u64,
    offset: u64,
    saved_validator: Option<&str>,
    automatic: bool,
    on_headers: impl FnMut(&Headers) -> Result<()>,
    curl: &Path,
) -> std::result::Result<Transfer, TransferFailure> {
    transfer_observed(
        url,
        destination,
        limit,
        offset,
        saved_validator,
        automatic,
        on_headers,
        curl,
        &mut |_| {},
    )
}

/// `observe` receives the running count of payload bytes written by this
/// response (excluding `offset`). It must not fail the transfer.
#[allow(clippy::too_many_arguments)]
fn transfer_observed(
    url: &str,
    destination: &Path,
    limit: u64,
    offset: u64,
    saved_validator: Option<&str>,
    automatic: bool,
    mut on_headers: impl FnMut(&Headers) -> Result<()>,
    curl: &Path,
    observe: &mut dyn FnMut(u64),
) -> std::result::Result<Transfer, TransferFailure> {
    use TransferFailure::{Invalid, RangeRejected};
    validate_url(url).map_err(Invalid)?;
    if let Some(local) = local_source(url) {
        if offset != 0 {
            return Err(RangeRejected(0));
        }
        let mut source = open_owned(&local, false, Some(limit)).map_err(Invalid)?;
        let mut target = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(destination)?;
        validate_file(&target.metadata()?, true, None).map_err(Invalid)?;
        let written = io::copy(&mut Read::by_ref(&mut source).take(limit + 1), &mut target)?;
        if written > limit {
            return Err(Invalid("artifact exceeds expected size".into()));
        }
        target.sync_all()?;
        return Ok(Transfer {
            received: 0,
            written,
        });
    }
    let (header, file) = Temporary::create(
        destination
            .parent()
            .ok_or_else(|| Invalid("missing transfer parent".into()))?,
        false,
    )
    .map_err(Invalid)?;
    drop(file);
    let mut command = Command::new(curl);
    command
        .args([
            // Must be first: user curl configuration must not add URLs,
            // redirects, output paths, or other transfer policy.
            "--disable",
            "--fail",
            "--show-error",
            "--silent",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            "--connect-timeout",
            if automatic { "2" } else { "15" },
            "--max-time",
            if automatic { "5" } else { EXPLICIT_MAX_SECONDS },
            "--dump-header",
        ])
        .arg(&header.path)
        .args(["-o", "-"]);
    if !automatic {
        command.args([
            "--speed-limit",
            STALL_BYTES_PER_SECOND,
            "--speed-time",
            STALL_SECONDS,
        ]);
    }
    if offset != 0 {
        command.args(["--range", &format!("{offset}-")]);
        if let Some(value) = saved_validator {
            command.args(["--header", &format!("If-Range: {value}")]);
        }
    }
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut target = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(offset == 0)
        .append(offset != 0)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(destination)?;
    validate_file(&target.metadata()?, true, None).map_err(Invalid)?;
    let mut child = ChildGuard(command.spawn()?);
    let mut output = child
        .0
        .stdout
        .take()
        .ok_or_else(|| Invalid("missing curl stdout".into()))?;
    let mut received = 0u64;
    let mut checked = false;
    let mut range_rejected = false;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = output.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if !checked {
            let headers = response_headers(&header.path).map_err(Invalid)?;
            range_rejected = offset != 0 && !valid_range(&headers, offset, limit);
            if !range_rejected {
                on_headers(&headers).map_err(Invalid)?;
            }
            checked = true;
        }
        received = received
            .checked_add(count as u64)
            .ok_or_else(|| Invalid("transfer counter overflow".into()))?;
        if range_rejected {
            if received > limit {
                return Err(Invalid("ignored Range response exceeds size limit".into()));
            }
        } else {
            if received > limit.saturating_sub(offset) {
                return Err(Invalid("artifact exceeds expected size".into()));
            }
            target.write_all(&buffer[..count])?;
            observe(received);
        }
    }
    target.sync_all()?;
    let status = child.0.wait()?;
    let headers = response_headers(&header.path).map_err(Invalid)?;
    if offset != 0
        && (range_rejected || matches!(headers.status, 200 | 416) || (status.success() && !checked))
    {
        return Err(RangeRejected(received));
    }
    if !status.success() {
        return Err(TransferFailure::Interrupted(
            curl_failure(status.code()).into(),
        ));
    }
    Ok(Transfer {
        received,
        written: received,
    })
}

pub(super) fn fetch_manifest(url: &str, automatic: bool) -> Result<(Vec<u8>, u64)> {
    fetch_manifest_with_curl(url, automatic, Path::new("/usr/bin/curl"))
}

fn fetch_manifest_with_curl(url: &str, automatic: bool, curl: &Path) -> Result<(Vec<u8>, u64)> {
    let (temporary, _) = Temporary::create(&std::env::temp_dir(), true)?;
    let path = temporary.path.join("manifest.json");
    let transferred = transfer(
        url,
        &path,
        MANIFEST_LIMIT,
        0,
        None,
        automatic,
        |_| Ok(()),
        curl,
    )
    .map_err(|error| match error {
        TransferFailure::Invalid(error) | TransferFailure::Interrupted(error) => error,
        TransferFailure::RangeRejected(_) => "unexpected manifest Range response".into(),
    })?;
    Ok((
        read_file(&path, MANIFEST_LIMIT, true)?,
        transferred.received,
    ))
}

/// Shared with the stock-zsh cold bootstrap: exactly four LF-terminated ASCII
/// lines (version marker, digest, canonical byte size, optional HTTP validator).
/// Keeping one non-JSON wire format lets both paths resume the same prefix
/// before the authenticated native executable has been acquired.
#[derive(Debug, PartialEq)]
struct PartialMetadata {
    sha256: String,
    size: u64,
    validator: Option<String>,
}

fn partial_metadata(bytes: &[u8]) -> Option<PartialMetadata> {
    let text = std::str::from_utf8(bytes).ok()?.strip_suffix('\n')?;
    let lines: Vec<_> = text.split('\n').collect();
    if lines.len() != 4
        || lines[0] != "hamn-download 1"
        || lines[1].len() != 64
        || !lines[1]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || lines[2].starts_with('0')
        || !lines[2].bytes().all(|byte| byte.is_ascii_digit())
        || (!lines[3].is_empty() && !valid_validator(lines[3]))
    {
        return None;
    }
    let size = lines[2].parse().ok()?;
    if size == 0 || size > GUEST_LIMIT {
        return None;
    }
    Some(PartialMetadata {
        sha256: lines[1].into(),
        size,
        validator: (!lines[3].is_empty()).then(|| lines[3].into()),
    })
}

fn save_partial_metadata(path: &Path, value: &PartialMetadata) -> Result<()> {
    let bytes = format!(
        "hamn-download 1\n{}\n{}\n{}\n",
        value.sha256,
        value.size,
        value.validator.as_deref().unwrap_or_default()
    );
    require(
        partial_metadata(bytes.as_bytes()).as_ref() == Some(value),
        "invalid partial metadata",
    )?;
    atomic_bytes(path, bytes.as_bytes())
}

/// Resumptions after the first attempt when a transfer is interrupted after
/// persisting new bytes. A transfer that makes no progress fails
/// immediately; integrity failures discard the partial and never retry.
const RESUME_ATTEMPTS: u32 = 3;

/// Explicit install/upgrade acquisition. Progress goes to stderr (see
/// `progress`); the result and counters are the same as one uninterrupted
/// transfer, except that bytes from interrupted attempts count as downloaded.
pub(super) fn acquire(
    cache: &Path,
    artifact: &Artifact,
    name: &str,
    label: &str,
) -> Result<(PathBuf, Counts)> {
    let mut progress = Progress::new(
        io::stderr(),
        label,
        artifact.size,
        progress::live_terminal(),
    );
    acquire_resuming(
        cache,
        artifact,
        name,
        Path::new("/usr/bin/curl"),
        &mut progress,
        RESUME_ATTEMPTS,
        std::time::Duration::from_secs(1),
    )
}

/// Network accounting for one attempt, filled in even when it fails.
#[derive(Default)]
struct AttemptStats {
    /// Payload bytes received, including a discarded rejected-Range body.
    downloaded: u64,
    /// Subset of `downloaded` written after an accepted Range request.
    resumed: u64,
    /// Bytes written to the partial by this attempt and still retained there.
    retained: u64,
}

fn acquire_resuming<W: Write>(
    cache: &Path,
    artifact: &Artifact,
    name: &str,
    curl: &Path,
    progress: &mut Progress<W>,
    attempts: u32,
    pause: std::time::Duration,
) -> Result<(PathBuf, Counts)> {
    let (mut downloaded, mut resumed, mut retained) = (0u64, 0u64, 0u64);
    let mut attempt = 0;
    loop {
        let mut stats = AttemptStats::default();
        match acquire_observed(
            cache,
            artifact,
            name,
            curl,
            Some(&mut *progress),
            &mut stats,
        ) {
            Ok((path, mut counts)) => {
                counts.downloaded_bytes = counts
                    .downloaded_bytes
                    .checked_add(downloaded)
                    .ok_or("transfer counter overflow")?;
                counts.resumed_bytes = counts
                    .resumed_bytes
                    .checked_add(resumed)
                    .ok_or("transfer counter overflow")?;
                // The final attempt counted earlier attempts' bytes as reused.
                counts.reused_bytes = counts.reused_bytes.saturating_sub(retained);
                return Ok((path, counts));
            }
            Err(error) => {
                downloaded = downloaded
                    .checked_add(stats.downloaded)
                    .ok_or("transfer counter overflow")?;
                resumed = resumed
                    .checked_add(stats.resumed)
                    .ok_or("transfer counter overflow")?;
                if stats.retained == 0 || attempt >= attempts {
                    return Err(error);
                }
                retained = retained
                    .checked_add(stats.retained)
                    .ok_or("transfer counter overflow")?;
                attempt += 1;
                progress.note(&format!("{error}; resuming ({attempt} of {attempts})..."));
                std::thread::sleep(pause);
            }
        }
    }
}

/// One attempt without progress: the transport contract exercised by tests.
#[cfg(test)]
fn acquire_with_curl(
    cache: &Path,
    artifact: &Artifact,
    name: &str,
    curl: &Path,
) -> Result<(PathBuf, Counts)> {
    acquire_observed(
        cache,
        artifact,
        name,
        curl,
        None::<&mut Progress<io::Sink>>,
        &mut AttemptStats::default(),
    )
}

/// Run one explicit transfer, recording bytes written so far in `written`
/// (also on failure) and forwarding them to the optional progress display.
#[allow(clippy::too_many_arguments)]
fn observed_transfer<W: Write>(
    url: &str,
    destination: &Path,
    limit: u64,
    offset: u64,
    saved_validator: Option<&str>,
    on_headers: impl FnMut(&Headers) -> Result<()>,
    curl: &Path,
    progress: &mut Option<&mut Progress<W>>,
    written: &mut u64,
) -> std::result::Result<Transfer, TransferFailure> {
    *written = 0;
    transfer_observed(
        url,
        destination,
        limit,
        offset,
        saved_validator,
        false,
        on_headers,
        curl,
        &mut |received| {
            *written = received;
            if let Some(progress) = progress.as_deref_mut() {
                progress.update(offset.saturating_add(received));
            }
        },
    )
}

/// One acquisition attempt: cache lookup, at most one Range retry, verified
/// publication. `progress` is told only about network transfers.
fn acquire_observed<W: Write>(
    cache: &Path,
    artifact: &Artifact,
    name: &str,
    curl: &Path,
    mut progress: Option<&mut Progress<W>>,
    stats: &mut AttemptStats,
) -> Result<(PathBuf, Counts)> {
    let limit = match name {
        "host" => HOST_LIMIT,
        "guestImage" => GUEST_LIMIT,
        _ => return Err("invalid artifact kind".into()),
    };
    validate_url(&artifact.url)?;
    require(
        artifact.sha256.len() == 64
            && artifact
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "artifact SHA-256 is invalid",
    )?;
    require(
        artifact.size > 0 && artifact.size <= limit,
        "artifact size outside permitted range",
    )?;
    let downloads = cache.join("downloads");
    directory(&downloads, 0o700)?;
    let key = &artifact.sha256;
    let final_path = downloads.join(format!("{key}.artifact"));
    let partial = downloads.join(format!(".{key}.partial"));
    let metadata = downloads.join(format!(".{key}.validator"));
    let _lock = lock(&downloads.join(format!(".{key}.lock")), true)?;
    let reused = |path: PathBuf, source: &str| -> Result<(PathBuf, Counts)> {
        let size = safe_file(&path, false, Some(limit))?.len();
        Ok((
            path,
            Counts {
                reused_bytes: size,
                source: source.into(),
                ..Counts::default()
            },
        ))
    };
    if verified(&final_path, artifact, limit)? {
        return reused(final_path, "cache");
    }
    if exists(&final_path)? {
        safe_file(&final_path, true, Some(limit))?;
        fs::remove_file(&final_path)?;
    }
    let guest = cache.join(format!("hamn-guest-{key}.img"));
    if name == "guestImage" && verified(&guest, artifact, limit)? {
        return reused(guest, "guest-cache");
    }
    let mut offset = 0;
    let mut saved_validator = None;
    if exists(&partial)? {
        let info = safe_file(&partial, true, Some(limit))?;
        if artifact.size == info.len() && verified(&partial, artifact, limit)? {
            if exists(&metadata)? {
                safe_file(&metadata, true, Some(4096))?;
            }
            open_owned(&partial, true, Some(limit))?.sync_all()?;
            fs::rename(&partial, &final_path)?;
            sync_directory(&downloads)?;
            remove_optional(&metadata)?;
            return reused(final_path, "partial-cache");
        }
        let saved = read_file(&metadata, 2048, true)
            .ok()
            .and_then(|bytes| partial_metadata(&bytes));
        if let Some(saved) = saved {
            if saved.sha256 == *key
                && artifact.size == saved.size
                && info.len() > 0
                && info.len() < saved.size
                && saved
                    .validator
                    .as_ref()
                    .is_none_or(|value| valid_validator(value))
            {
                offset = info.len();
                saved_validator = saved.validator;
            }
        }
        if offset == 0 {
            fs::remove_file(&partial)?;
        }
    }
    let save_metadata = |validator: Option<String>| -> Result<()> {
        save_partial_metadata(
            &metadata,
            &PartialMetadata {
                sha256: key.clone(),
                size: artifact.size,
                validator,
            },
        )
    };
    save_metadata(saved_validator.clone())?;
    let on_headers = |headers: &Headers| -> Result<()> {
        if let Some(value) = validator(headers) {
            save_metadata(Some(value))?;
        }
        Ok(())
    };
    let expected = artifact.size;
    let mut discarded = 0;
    // Local test artifacts are copied, not transferred; they get no progress.
    let network = local_source(&artifact.url).is_none();
    if network && let Some(progress) = progress.as_deref_mut() {
        progress.start(offset);
    }
    let mut written = 0;
    let mut transferred = observed_transfer(
        &artifact.url,
        &partial,
        expected,
        offset,
        saved_validator.as_deref(),
        on_headers,
        curl,
        &mut progress,
        &mut written,
    );
    if let Err(TransferFailure::RangeRejected(received)) = transferred {
        discarded = received;
        remove_optional(&partial)?;
        offset = 0;
        if let Some(progress) = progress.as_deref_mut() {
            progress.restart(0);
        }
        transferred = observed_transfer(
            &artifact.url,
            &partial,
            expected,
            0,
            None,
            on_headers,
            curl,
            &mut progress,
            &mut written,
        );
    }
    stats.downloaded = discarded
        .checked_add(written)
        .ok_or("transfer counter overflow")?;
    if offset != 0 {
        stats.resumed = written;
    }
    let done = offset.saturating_add(written);
    if network && let Some(progress) = progress.as_deref_mut() {
        progress.finish(done, transferred.is_ok());
    }
    let transferred = match transferred {
        Ok(value) => value,
        Err(TransferFailure::Invalid(error)) => {
            remove_optional(&partial)?;
            remove_optional(&metadata)?;
            return Err(error);
        }
        Err(TransferFailure::Interrupted(error)) => {
            stats.retained = written;
            return Err(error);
        }
        Err(TransferFailure::RangeRejected(_)) => {
            return Err("unexpected repeated Range rejection".into());
        }
    };
    if !verified(&partial, artifact, limit)? {
        remove_optional(&partial)?;
        remove_optional(&metadata)?;
        return Err("artifact size or SHA-256 mismatch".into());
    }
    // Do not discard verified complete bytes when rename/fsync fails. The next
    // owner can promote the full partial or reuse the already published final.
    fs::rename(&partial, &final_path)?;
    sync_directory(&downloads)?;
    remove_optional(&metadata)?;
    let counts = Counts {
        downloaded_bytes: transferred
            .received
            .checked_add(discarded)
            .ok_or("transfer counter overflow")?,
        resumed_bytes: if offset != 0 { transferred.received } else { 0 },
        reused_bytes: if transferred.received == 0 {
            transferred.written
        } else {
            offset
        },
        source: if offset != 0 {
            "resumed"
        } else if transferred.received == 0 {
            "local"
        } else {
            "download"
        }
        .into(),
    };
    Ok((final_path, counts))
}

#[cfg(test)]
#[path = "download_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "download_resume_tests.rs"]
mod resume_tests;
