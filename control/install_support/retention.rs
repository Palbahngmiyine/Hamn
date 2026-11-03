//! Obsolete-generation collection, under the caller's transaction locks for
//! both canonical install roots. Keeps the active generation, its recorded
//! predecessor, the caller's `keep` targets, anything a process has open, and
//! generations named by a pending recovery root. A generation is collected
//! only with the exact marker for these roots, of this layout or of the
//! migrated Hamn 0.1.x layout; generations of unknown ownership remain;
//! marker-last retirement is retryable. Reports removals and deferrals to the
//! caller, which decides where they are shown.
//!
//! After an update, `collect_cache` also removes the image-cache entries that
//! the selected guest image and the generations `collect` kept do not use,
//! under the HOME's updater lock; see its contract.
use super::{
    Result, download, files, generation, locks, manifest::hexadecimal, receipt, require, upgrade,
};
use std::{
    collections::BTreeSet,
    fs,
    io::{ErrorKind, Read},
    os::unix::{fs::MetadataExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn children(path: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = fs::read_dir(path)?
        .map(|p| p.map(|p| p.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort();
    Ok(entries)
}
fn tree_owned(path: &Path) -> Result<bool> {
    let m = fs::symlink_metadata(path)?;
    if files::owned(path, m.is_dir(), None).is_err() || m.mode() & 0o022 != 0 {
        return Ok(false);
    }
    if m.is_dir() {
        for child in children(path)? {
            if !tree_owned(&child)? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}
fn pending(cache: &Path) -> Result<bool> {
    match fs::symlink_metadata(cache) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
        Ok(_) => (),
    }
    let m = match files::owned(cache, true, None) {
        Ok(m) => m,
        Err(_) => return Ok(true),
    };
    if m.mode() & 0o022 != 0 {
        return Ok(true);
    }
    Ok(children(cache)?.iter().any(|p| {
        p.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".hamn-update-")
    }))
}
fn recovery_pending(path: &Path) -> Result<bool> {
    let value = files::text(path)?;
    if !value.starts_with('/')
        || value.contains('\n')
        || path.file_name().unwrap()
            != format!(".hamn-recovery-root-{}", files::hash(value.as_bytes())).as_str()
    {
        return Ok(true);
    }
    pending(Path::new(&value))
}
fn generation_name(name: &str) -> bool {
    name.is_ascii()
        && name.len() == 71
        && hexadecimal(&name[..64], 64)
        && name.as_bytes()[64] == b'-'
        && name.as_bytes()[65..].iter().all(u8::is_ascii_alphanumeric)
}
fn generation_target(path: &Path, root: &Path) -> bool {
    path.file_name().is_some_and(|p| p == "hamn")
        && path.parent().is_some_and(|p| {
            p.file_name().is_some_and(|p| p == "bin")
                && p.parent().is_some_and(|g| {
                    g.parent() == Some(root)
                        && g.file_name()
                            .and_then(|p| p.to_str())
                            .is_some_and(generation_name)
                })
        })
}
fn erase(path: &Path) -> Result<()> {
    for p in children(path)? {
        if p.file_name().unwrap() == ".hamn-generation" {
            continue;
        }
        if fs::symlink_metadata(&p)?.is_dir() {
            erase(&p)?;
        } else {
            fs::remove_file(p)?;
        }
    }
    match fs::remove_file(path.join(".hamn-generation")) {
        Ok(()) => (),
        Err(e) if e.kind() == ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    fs::remove_dir(path)?;
    Ok(())
}

/// Drain both streams concurrently; timeout kills and reaps the scanner process
/// group, rather than leaving a child alive behind an outer async deadline.
fn scan(command: &mut Command, timeout: Duration) -> Result<Vec<u8>> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let readers: Vec<_> = [Box::new(stdout) as Box<dyn Read + Send>, Box::new(stderr)]
        .into_iter()
        .enumerate()
        .map(|(index, mut pipe)| {
            let send = send.clone();
            thread::spawn(move || {
                let mut data = Vec::new();
                let result = pipe.read_to_end(&mut data).map(|_| data);
                let _ = send.send((index, result));
            })
        })
        .collect();
    drop(send);
    let deadline = Instant::now() + timeout;
    let mut output = [Vec::new(), Vec::new()];
    let mut complete = true;
    for _ in 0..2 {
        match receive.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok((index, Ok(data))) => output[index] = data,
            _ => {
                complete = false;
                break;
            }
        }
    }
    if !complete {
        // Do not reap the leader until pipe completion/deadline: its PID still
        // identifies this exclusively owned process group, including descendants.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    // Pipes can close before a child finishes. Retain a bounded wait in that case.
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            complete = false;
            break child.wait()?;
        }
        thread::sleep(Duration::from_millis(10));
    };
    for reader in readers {
        reader.join().map_err(|_| "scanner reader failed")?;
    }
    require(
        complete && status.success() && output[1].is_empty(),
        "open-file scan unavailable or incomplete",
    )?;
    Ok(std::mem::take(&mut output[0]))
}

/// Absolute paths that any process of this user has open (lsof `n` fields).
fn open_files(timeout: Duration) -> Result<Vec<String>> {
    let stdout = scan(
        Command::new("/usr/sbin/lsof").args(["-nP", "-F", "n"]),
        timeout,
    )?;
    Ok(open_paths(&String::from_utf8(stdout)?))
}
fn open_paths(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.strip_prefix('n').filter(|p| p.starts_with('/')))
        .map(str::to_owned)
        .collect()
}

/// What one collection did.
#[derive(Debug, Default)]
pub(super) struct Collection {
    /// Names of removed generations (`collect`) or cache entries
    /// (`collect_cache`).
    pub(super) removed: Vec<String>,
    /// Why collection (or one generation's or entry's collection) was
    /// deferred.
    pub(super) deferred: Vec<String>,
    /// `collect` only: the directories of the generations it kept for the
    /// active link, the recorded predecessor and the caller's `keep` targets
    /// within these roots. `None` when it stopped before deciding (pending
    /// recovery metadata), which is not the same as keeping nothing.
    pub(super) retained: Option<Vec<PathBuf>>,
}

/// The generation directories of the `keep` targets that lie in `root`.
fn retained_generations(keep: &[PathBuf], root: &Path) -> Result<Vec<PathBuf>> {
    let mut retained = BTreeSet::new();
    for target in keep.iter().filter(|p| generation_target(p, root)) {
        retained.insert(files::parent(files::parent(target)?)?.to_owned());
    }
    Ok(retained.into_iter().collect())
}

/// Collects obsolete generations of `transaction`'s roots (see the module
/// documentation). An error means nothing more could be decided safely.
pub(super) fn collect(transaction: &locks::Transaction, keep: &[&str]) -> Result<Collection> {
    let roots = transaction.roots();
    let (bin, data) = (roots.bindir.as_path(), roots.datadir.as_path());
    let mut report = Collection::default();
    let root = data.join(".hamn-generations");
    files::owned(data, true, Some(0o755))?;
    files::owned(&root, true, Some(0o755))?;
    let root_text = root.to_str().ok_or("invalid generation path")?;
    require(
        root_text
            .bytes()
            .all(|c| (32..=126).contains(&c) && c != b'\\'),
        "generation path cannot be matched safely in process output",
    )?;
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    if pending(&PathBuf::from(home).join(".hamn/cache"))? {
        report
            .deferred
            .push("generation cleanup deferred while recovery metadata exists".into());
        return Ok(report);
    }
    let active = fs::read_link(bin.join("hamn"))?;
    require(
        generation_target(&active, &root),
        "active link is outside managed generation root",
    )?;
    let mut keep: Vec<PathBuf> = keep.iter().map(PathBuf::from).collect();
    keep.push(active.clone());
    let predecessor = files::parent(files::parent(&active)?)?.join(".hamn-previous-target");
    match fs::symlink_metadata(&predecessor) {
        Ok(_) => {
            files::owned(&predecessor, false, Some(0o600))?;
            let value = files::text(&predecessor)?;
            let target = PathBuf::from(value.trim_end_matches('\n'));
            require(
                value.ends_with('\n')
                    && value.bytes().filter(|c| *c == b'\n').count() == 1
                    && generation_target(&target, &root),
                "invalid predecessor reference",
            )?;
            keep.push(target);
        }
        Err(e) if e.kind() == ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    report.retained = Some(retained_generations(&keep, &root)?);
    let opened = open_files(Duration::from_secs(30))?;
    for mut path in children(&root)? {
        let leaf = path
            .file_name()
            .and_then(|p| p.to_str())
            .unwrap_or_default()
            .to_owned();
        let retired = leaf.starts_with(".retired-");
        let name = leaf.strip_prefix(".retired-").unwrap_or(&leaf);
        if !generation_name(name) {
            continue;
        }
        let result = (|| -> Result<bool> {
            if !tree_owned(&path)?
                || keep.iter().any(|p| p.starts_with(&path))
                || opened.iter().any(|p| Path::new(p).starts_with(&path))
            {
                return Ok(false);
            }
            if retired && children(&path)?.is_empty() {
                fs::remove_dir(&path)?;
                return Ok(false);
            }
            let marker = path.join(".hamn-generation");
            if files::owned(&marker, false, Some(0o600)).is_err() {
                return Ok(false);
            }
            // Only the exact marker for these roots, of this layout or of
            // the migrated 0.1.x layout, proves ownership; other layouts and
            // roots are left alone.
            let text = files::text(&marker)?;
            if text != generation::marker_text(&name[..64], roots)?
                && text != generation::released_marker_text(&name[..64], roots)?
            {
                return Ok(false);
            }
            for p in children(&path)? {
                if p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".hamn-recovery-root-")
                    && recovery_pending(&p)?
                {
                    return Ok(false);
                }
            }
            if !retired {
                let binary = path.join("bin/hamn");
                files::owned(&binary, false, Some(0o755))?;
                if files::digest(&binary)? != name[..64] {
                    return Ok(false);
                }
                let destination = root.join(format!(".retired-{name}"));
                match fs::symlink_metadata(&destination) {
                    Err(e) if e.kind() == ErrorKind::NotFound => (),
                    _ => return Ok(false),
                }
                fs::rename(&path, &destination)?;
                path = destination;
            }
            erase(&path)?;
            Ok(true)
        })();
        match result {
            Ok(true) => report.removed.push(name.to_owned()),
            Ok(false) => {}
            Err(error) => report
                .deferred
                .push(format!("generation cleanup deferred: {error}")),
        }
    }
    Ok(report)
}

/// The digests that image-cache entries name: `downloads/<d>.artifact`,
/// `hamn-guest-<d>.img` and its `.verified` marker, `raw-v1-<d>` and a
/// leftover `raw-v1-<d>.retired`.
#[derive(Default)]
struct Inventory {
    artifacts: BTreeSet<String>,
    images: BTreeSet<String>,
    bundles: BTreeSet<String>,
    retired: BTreeSet<String>,
}

fn digest_in<'a>(name: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    name.strip_prefix(prefix)?
        .strip_suffix(suffix)
        .filter(|digest| hexadecimal(digest, 64))
}

fn names(directory: &Path) -> Result<Vec<String>> {
    match fs::read_dir(directory) {
        Ok(entries) => Ok(entries
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<_>>()?),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

fn defer(report: &mut Collection, text: impl std::fmt::Display) {
    report.deferred.push(format!("image cache cleanup {text}"));
}

/// Reports that the entry `shown` stays, and why.
fn left(report: &mut Collection, shown: &str, why: impl std::fmt::Display) {
    defer(report, format_args!("left {shown} in place: {why}"));
}

impl Inventory {
    /// Lists `cache`; an unsafe `downloads` directory is deferred, not read.
    fn of(cache: &Path, report: &mut Collection) -> Result<Self> {
        let mut found = Self::default();
        let downloads = cache.join("downloads");
        match unsafe_entry(&downloads, true) {
            Ok(None) => match names(&downloads) {
                Ok(names) => found.artifacts.extend(
                    names
                        .iter()
                        .filter_map(|name| digest_in(name, "", ".artifact"))
                        .map(str::to_owned),
                ),
                Err(e) => left(report, "downloads", e),
            },
            Ok(Some(why)) => left(report, "downloads", why),
            Err(e) if is_absent(e.as_ref()) => (),
            Err(e) => left(report, "downloads", e),
        }
        for name in names(cache)? {
            if let Some(digest) = digest_in(&name, "hamn-guest-", ".img")
                .or_else(|| digest_in(&name, "hamn-guest-", ".img.verified"))
            {
                found.images.insert(digest.to_owned());
            } else if let Some(digest) = digest_in(&name, "raw-v1-", "") {
                found.bundles.insert(digest.to_owned());
            } else if let Some(digest) = digest_in(&name, "raw-v1-", ".retired") {
                found.retired.insert(digest.to_owned());
            }
        }
        Ok(found)
    }
}

fn is_absent(error: &(dyn std::error::Error + 'static)) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == ErrorKind::NotFound)
}

/// The digests that the receipts of the retained generations record. A
/// receipt that names nothing is not skipped silently: `unusable` says why,
/// and then the sets may lack digests, so they can only prove that an entry
/// is used, never that it is not.
#[derive(Default)]
struct Recorded {
    hosts: BTreeSet<String>,
    guests: BTreeSet<String>,
    unusable: Option<String>,
}

impl Recorded {
    fn of(retained: &[PathBuf]) -> Self {
        let mut recorded = Self::default();
        for generation in retained {
            match receipt::digests(generation) {
                Ok(digests) => {
                    recorded.hosts.insert(digests.host);
                    recorded.guests.insert(digests.guest);
                }
                Err(e) => {
                    let why = format!("the receipt of {}: {e}", generation.display());
                    recorded.unusable.get_or_insert(why);
                }
            }
        }
        recorded
    }
}

/// Why `path` is unsafe to remove: a symbolic link, an entry the current
/// user does not own, or (for a file) not a regular file with one link. An
/// error means it could not be inspected.
fn unsafe_entry(path: &Path, directory: bool) -> Result<Option<&'static str>> {
    let info = fs::symlink_metadata(path)?;
    Ok(if info.file_type().is_symlink() {
        Some("it is a symbolic link")
    } else if files::owned(path, directory, None).is_err() {
        Some(if directory {
            "it is not a directory owned by you"
        } else {
            "it is not a regular file owned by you with one link"
        })
    } else {
        None
    })
}

/// Why `path` must stay, or `None` when it may be removed: it is unsafe, a
/// raw base (`directory`) holds anything but its two files, or a process
/// has it (or a file in it) open.
fn stays(path: &Path, directory: bool, opened: &[String]) -> Result<Option<&'static str>> {
    if let Some(why) = unsafe_entry(path, directory)? {
        return Ok(Some(why));
    }
    if directory {
        for name in names(path)? {
            if !matches!(name.as_str(), "disk.raw" | "verified") {
                return Ok(Some("it holds files that are not part of a raw base"));
            }
            if let Some(why) = unsafe_entry(&path.join(&name), false)? {
                return Ok(Some(why));
            }
        }
    }
    Ok(opened
        .iter()
        .any(|open| Path::new(open).starts_with(path))
        .then_some("a process has it open"))
}

/// Records the verdict on `path`: `true` when it may be removed.
fn removable(
    report: &mut Collection,
    shown: &str,
    path: &Path,
    directory: bool,
    opened: &[String],
) -> bool {
    match stays(path, directory, opened) {
        Ok(None) => true,
        Ok(Some(why)) => {
            left(report, shown, why);
            false
        }
        Err(e) => {
            left(report, shown, e);
            false
        }
    }
}

fn remove_cached_file(report: &mut Collection, cache: &Path, shown: &str, opened: &[String]) {
    let path = cache.join(shown);
    if !removable(report, shown, &path, false, opened) {
        return;
    }
    match fs::remove_file(&path) {
        Ok(()) => report.removed.push(shown.to_owned()),
        Err(e) if e.kind() == ErrorKind::NotFound => (),
        Err(e) => left(report, shown, e),
    }
}

/// Removes `downloads/<digest>.artifact` while holding the downloader's
/// digest lock without waiting, so that it is not removed during a lookup,
/// transfer or publication of the same digest.
fn remove_artifact(report: &mut Collection, cache: &Path, digest: &str, opened: &[String]) {
    let shown = format!("downloads/{digest}.artifact");
    let _lock = match download::artifact_lock(&cache.join("downloads"), digest, false) {
        Ok(lock) => lock,
        Err(e) => {
            let why = format_args!("its download lock is held or unusable: {e}");
            left(report, &shown, why);
            return;
        }
    };
    remove_cached_file(report, cache, &shown, opened);
}

/// Removes the raw base `name` (`verified`, `disk.raw`, then the directory),
/// reporting it as `shown` once it is gone. Returns whether it is gone.
fn erase_bundle(report: &mut Collection, cache: &Path, name: &str, shown: &str) -> bool {
    let path = cache.join(name);
    let erased = ["verified", "disk.raw"]
        .iter()
        .try_for_each(|file| match fs::remove_file(path.join(file)) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        })
        .and_then(|()| fs::remove_dir(&path));
    let gone = erased.is_ok();
    match erased {
        Ok(()) => report.removed.push(shown.to_owned()),
        Err(e) => left(report, name, e),
    }
    gone
}

/// Removes the raw base of `digest` while holding its digest lock without
/// waiting (`raw_cache.c` holds the same lock to create and clone it).
/// `unused` says the live base is not kept. A live base is first renamed to
/// `raw-v1-<d>.retired`: `raw_cache.c` trusts a live name only when its
/// contents are complete, so an erase that stops midway must not leave a
/// partial base under it. A leftover retired base is garbage whatever the
/// digest. The lock file stays: a second process could otherwise lock a new
/// file of the same name while this one holds the old.
fn remove_raw(
    report: &mut Collection,
    cache: &Path,
    digest: &str,
    unused: bool,
    opened: &[String],
) {
    let (live, retired) = (
        format!("raw-v1-{digest}"),
        format!("raw-v1-{digest}.retired"),
    );
    let _lock = match download::lock(&cache.join(format!(".raw-v1-{digest}.lock")), false) {
        Ok(lock) => lock,
        Err(e) => {
            let shown = if unused { &live } else { &retired };
            let why = format_args!("its digest lock is held or unusable: {e}");
            left(report, shown, why);
            return;
        }
    };
    if fs::symlink_metadata(cache.join(&retired)).is_ok()
        && (!removable(report, &retired, &cache.join(&retired), true, opened)
            || !erase_bundle(report, cache, &retired, &retired))
    {
        return;
    }
    if unused
        && fs::symlink_metadata(cache.join(&live)).is_ok()
        && removable(report, &live, &cache.join(&live), true, opened)
    {
        match files::rename_exclusive(&cache.join(&live), &cache.join(&retired)) {
            Ok(()) => {
                erase_bundle(report, cache, &retired, &live);
            }
            Err(e) => left(report, &live, e),
        }
    }
}

/// Removes image-cache entries of the HOME's `cache` that the selection and
/// the `retained` generations do not use, after a committed update:
/// - `downloads/<d>.artifact`, unless `<d>` is the host archive recorded by
///   a retained generation's release receipt: that archive stays, so that
///   repairing or forcing a reinstall of a kept generation needs no network.
///   Any other completed download is redundant: a guest image's is replaced
///   by its staged `hamn-guest-<d>.img` (which acquisition reuses), and an
///   earlier release's host archive is no longer installed. It is removed
///   under the downloader's digest lock, which a concurrent installer holds
///   only while it looks the file up. Partial downloads, validators and
///   locks stay.
/// - `hamn-guest-<d>.img` with its `.verified` marker, and `raw-v1-<d>`,
///   when `<d>` is neither the selected image nor the guest recorded by a
///   retained generation's release receipt.
/// - A leftover `raw-v1-<d>.retired`, a raw base whose removal stopped,
///   whatever the keep set.
///
/// A path is removed only when the current user owns it, no symbolic link or
/// extra hard link is involved and no process has it open (`lsof` finds the
/// open files, bounded to 30 s; a failed scan removes nothing). A raw base
/// also needs its digest lock (see `remove_raw`). Anything else stays and is
/// reported in `deferred`. A missing or unusable selection or receipt defers
/// images and live raw bases. An unusable receipt may also name any download
/// as its host archive, so it defers every download except one that a usable
/// receipt or the selection shows to be a guest image's. Pending recovery
/// metadata in `cache` defers everything. Nothing outside `cache` changes,
/// and profile disks are never opened.
///
/// `retained` must be non-empty: it is `collect`'s `retained` of this
/// update. The caller holds the HOME's updater lock (`_updater`), so no
/// other updater stages or selects an image meanwhile. An error means the
/// cache could not be inspected; nothing was removed. A removal that stops
/// midway leaves only entries the next run removes by the same rules.
pub(super) fn collect_cache(
    cache: &Path,
    retained: &[PathBuf],
    _updater: &locks::Held,
) -> Result<Collection> {
    collect_cache_with(cache, retained, || open_files(Duration::from_secs(30)))
}

/// The digests in `digests` that `keep` does not contain.
fn unused(digests: &BTreeSet<String>, keep: &BTreeSet<String>) -> Vec<String> {
    digests.difference(keep).cloned().collect()
}

/// `collect_cache` with the open-file scan supplied. It runs at most once,
/// and only when there is something to remove.
fn collect_cache_with(
    cache: &Path,
    retained: &[PathBuf],
    scan: impl FnOnce() -> Result<Vec<String>>,
) -> Result<Collection> {
    require(
        !retained.is_empty(),
        "no retained generation to derive the image cache keep set from",
    )?;
    let mut report = Collection::default();
    if pending(cache)? {
        defer(
            &mut report,
            "deferred while recovery metadata exists or the cache directory is unsafe",
        );
        return Ok(report);
    }
    // lsof names files by their canonical path.
    let cache = fs::canonicalize(cache)?;
    let found = Inventory::of(&cache, &mut report)?;
    let recorded = Recorded::of(retained);
    let selected = upgrade::selected_guest(&cache);
    let mut guests = recorded.guests.clone();
    guests.extend(selected.as_ref().ok().cloned());
    let (mut images, mut bundles) = (
        unused(&found.images, &guests),
        unused(&found.bundles, &guests),
    );
    if !(images.is_empty() && bundles.is_empty()) {
        let unknown = selected
            .as_ref()
            .err()
            .map(ToString::to_string)
            .or_else(|| recorded.unusable.clone());
        if let Some(why) = unknown {
            defer(
                &mut report,
                format_args!("of guest images and raw bases deferred: {why}"),
            );
            (images, bundles) = (Vec::new(), Vec::new());
        }
    }
    let mut artifacts = unused(&found.artifacts, &recorded.hosts);
    if let Some(why) = &recorded.unusable {
        let candidates = artifacts.len();
        artifacts.retain(|digest| guests.contains(digest));
        if artifacts.len() < candidates {
            defer(&mut report, format_args!("of downloads deferred: {why}"));
        }
    }
    if artifacts.is_empty() && images.is_empty() && bundles.is_empty() && found.retired.is_empty() {
        return Ok(report);
    }
    let opened = match scan() {
        Ok(opened) => opened,
        Err(e) => {
            defer(&mut report, format_args!("deferred: {e}"));
            return Ok(report);
        }
    };
    for digest in &artifacts {
        remove_artifact(&mut report, &cache, digest, &opened);
    }
    for digest in &images {
        for suffix in [".img.verified", ".img"] {
            let name = format!("hamn-guest-{digest}{suffix}");
            if fs::symlink_metadata(cache.join(&name)).is_ok() {
                remove_cached_file(&mut report, &cache, &name, &opened);
            }
        }
    }
    let raw: BTreeSet<&String> = bundles.iter().chain(&found.retired).collect();
    for digest in raw {
        let unused = bundles.contains(digest);
        remove_raw(&mut report, &cache, digest, unused, &opened);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scanner_deadline_covers_exited_leader_and_inherited_pipes() {
        let start = Instant::now();
        let result = scan(
            Command::new("/bin/sh").args(["-c", "sleep 30 & exit 0"]),
            Duration::from_millis(100),
        );
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(5));
    }
    #[test]
    fn scanner_failure_and_closed_pipes_are_bounded() {
        for script in ["echo error >&2", "exit 1", "exec 1>&- 2>&-; sleep 30"] {
            assert!(
                scan(
                    Command::new("/bin/sh").args(["-c", script]),
                    Duration::from_millis(100)
                )
                .is_err()
            );
        }
        assert_eq!(
            scan(
                Command::new("/bin/echo").arg("result"),
                Duration::from_secs(2)
            )
            .unwrap(),
            b"result\n"
        );
    }
    #[test]
    fn open_paths_keep_only_absolute_names() {
        // Process (`p`) and descriptor (`f`) fields and non-path names
        // (pipes, sockets) never select a generation.
        let text = "p1\nn/a/share/hamn/update-manifest-url\nnpipe\np2\nn/b/.hamn-generations/x/bin/hamn\nf5\n";
        assert_eq!(
            open_paths(text),
            [
                "/a/share/hamn/update-manifest-url",
                "/b/.hamn-generations/x/bin/hamn"
            ]
        );
    }
    #[test]
    fn unicode_generation_names_do_not_panic() {
        assert!(!generation_name(&format!("{}é-12345", "a".repeat(63))));
    }
}

#[cfg(test)]
#[path = "retention_cache_tests.rs"]
mod cache_tests;
