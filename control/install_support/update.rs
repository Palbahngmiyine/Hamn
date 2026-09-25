//! `hamn __install-support update`: one HTTPS/digest-verified release
//! transaction. `hamn upgrade` and `hamn --headless system upgrade` reach it
//! from the core worker (the C control API runs the managed generation's own
//! executable); the release installer `install.sh` runs the downloaded
//! release's executable with `--bootstrap`.
//!
//! Arguments: `--bindir DIR --datadir DIR` (the install roots),
//! `--manifest URL_OR_PATH` (default: the invoking or active generation's
//! `share/hamn/update-manifest-url`), `--bootstrap` (a first installation,
//! or a reinstallation that then proceeds as an update), `--check-only`
//! (read-only status; conflicts with `--force`), `--force` (reinstall the
//! same release), `--current-version VERSION --generation TARGET` (given
//! together by a frontend: its version and its own generation's `bin/hamn`),
//! `--output-json` and `--result-file FILE` (an owned 0600 file the frontend
//! reads: the JSON result on success, otherwise one failure reason).
//!
//! Output: progress sentences on stderr; the JSON result on stdout or in the
//! result file. Exit status 0 (done or up to date), 1 (one reason: in the
//! result file, else `hamn upgrade: REASON` or `hamn install: REASON` on
//! stderr), 2 (usage), or 129/130/143 after HUP/INT/TERM interrupted a
//! journaled transaction (rolled back, or left for recovery).
//!
//! Transaction. Under both roots' transaction locks and the HOME's update
//! lock: finish or recover any earlier transaction; validate the active
//! installation, the manifest, platform, sizes, digests and extracted
//! version; stage the guest image; publish the journal; install the host
//! generation (unless only the guest image needs repair); verify it and write
//! its receipt; commit the guest image selection; retire the journal; then
//! collect obsolete generations. Every failure after the journal exists
//! rolls back through it; a SIGKILL leaves it for the next run, and VM start
//! refuses to run while it is pending. Existing profiles and VMs are never
//! touched. HTTPS acquisition is bounded by `download`.
use super::{
    Result, archive, files,
    generation::{self, POINTER, Payload},
    interrupt,
    journal::{self, Plan, RollbackError},
    locks::{self, Roots},
    manifest::{self, Manifest},
    retention,
    upgrade::{self},
};
use crate::install_support::download::{self, Counts};
use std::{
    collections::BTreeMap,
    ffi::CStr,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const USAGE: &str = "usage: hamn __install-support update --bindir DIR --datadir DIR [--manifest URL_OR_PATH] \
                     [--bootstrap] [--check-only] [--force] [--current-version VERSION --generation TARGET] \
                     [--output-json] [--result-file FILE]";
const INSTALL_URL: &str = "https://github.com/Palbahngmiyine/Hamn#install";
/// Bound on running an installed or extracted executable's `--version`.
const VERSION_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Default)]
struct Options {
    bindir: Option<PathBuf>,
    datadir: Option<PathBuf>,
    manifest: Option<String>,
    bootstrap: bool,
    check_only: bool,
    force: bool,
    output_json: bool,
    result_file: Option<PathBuf>,
    current_version: Option<String>,
    generation: Option<String>,
}

fn parse(args: &[String]) -> Option<Options> {
    let mut options = Options::default();
    let mut words = args.iter();
    while let Some(word) = words.next() {
        let mut value = |slot_empty: bool| -> Option<String> {
            if slot_empty {
                words.next().cloned()
            } else {
                None
            }
        };
        match word.as_str() {
            "--bindir" => options.bindir = Some(value(options.bindir.is_none())?.into()),
            "--datadir" => options.datadir = Some(value(options.datadir.is_none())?.into()),
            "--manifest" => options.manifest = Some(value(options.manifest.is_none())?),
            "--result-file" => {
                options.result_file = Some(value(options.result_file.is_none())?.into())
            }
            "--current-version" => {
                options.current_version = Some(value(options.current_version.is_none())?)
            }
            "--generation" => options.generation = Some(value(options.generation.is_none())?),
            "--bootstrap" if !options.bootstrap => options.bootstrap = true,
            "--check-only" if !options.check_only => options.check_only = true,
            "--force" if !options.force => options.force = true,
            "--output-json" if !options.output_json => options.output_json = true,
            _ => return None,
        }
    }
    let frontend_complete = options.current_version.is_some() == options.generation.is_some();
    (frontend_complete && !(options.check_only && options.force)).then_some(options)
}

/// Why a transaction stopped.
enum Stop {
    /// One reason for the caller; exit status 1.
    Fail(String),
    Usage,
    /// Interrupted by this signal after rollback handling.
    Signal(i32),
}

type Step<T> = std::result::Result<T, Stop>;

fn fail<T>(reason: impl Into<String>) -> Step<T> {
    Err(Stop::Fail(reason.into()))
}

/// Human output (stderr) and the failure channel.
struct Report {
    prefix: &'static str,
    result_file: Option<PathBuf>,
}

/// Writes one line to stderr. A closed or failing diagnostic stream never
/// stops a transaction or its rollback.
fn line(text: &str) {
    let _ = writeln!(std::io::stderr(), "{text}");
}

impl Report {
    fn note(&self, text: &str) {
        line(&format!("{}: {text}", self.prefix));
    }

    /// The final failure: the reason goes to the result file when one was
    /// accepted, otherwise to stderr with the command prefix.
    fn failure(&self, reason: &str) -> i32 {
        if let Some(file) = &self.result_file {
            if write_in_place(file, format!("{reason}\n").as_bytes()).is_ok() {
                return 1;
            }
        }
        self.note(reason);
        1
    }
}

/// Replaces the contents of `path` without replacing the file: the frontend
/// reads it through the descriptor it created.
fn write_in_place(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.flush()
}

pub(super) fn run(args: &[String]) -> i32 {
    let Some(options) = parse(args) else {
        line(USAGE);
        return 2;
    };
    let mut report = Report {
        prefix: if options.bootstrap {
            "hamn install"
        } else {
            "hamn upgrade"
        },
        result_file: None,
    };
    match transaction(&options, &mut report) {
        Ok(()) => 0,
        Err(Stop::Usage) => {
            line(USAGE);
            2
        }
        Err(Stop::Fail(reason)) => report.failure(&reason),
        Err(Stop::Signal(signal)) => interrupt::exit_status(signal),
    }
}

/// `uname -m`.
fn machine() -> Result<String> {
    // uname fills this zeroed local structure; the returned field is a
    // NUL-terminated array inside it.
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut name) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { CStr::from_ptr(name.machine.as_ptr()) }
        .to_str()?
        .to_owned())
}

/// The release information could not be used: say why and what to do next.
fn manifest_failure(reason: &str) -> Stop {
    let reason = if reason.is_empty() {
        "unknown error"
    } else {
        reason
    };
    Stop::Fail(match reason.strip_prefix("download failed: ") {
        Some(rest) => {
            format!("could not check for updates: {rest}. Check your connection and try again.")
        }
        None => format!(
            "the latest release information is not usable by this Hamn ({reason}). Reinstall with the official installer: {INSTALL_URL}"
        ),
    })
}

/// The release manifest pointer of the generation whose `bin/hamn` is
/// `target`.
fn pointer_of(target: &str) -> Option<PathBuf> {
    Some(Path::new(target).parent()?.parent()?.join(POINTER))
}

fn read_pointer(target: Option<&str>) -> Option<String> {
    let pointer = pointer_of(target?)?;
    files::safe_regular(&pointer)
        .then(|| files::line(&pointer, 4096).ok())
        .flatten()
}

fn read_link(link: &Path) -> Option<String> {
    fs::read_link(link).ok()?.to_str().map(str::to_owned)
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Standard output of `binary --version` (trailing newlines removed) and
/// whether it exited successfully; `None` when it could not run in time.
fn reported_version(binary: &Path) -> Option<(bool, String)> {
    let (ok, output) =
        files::bounded_output(Command::new(binary).arg("--version"), VERSION_TIMEOUT).ok()?;
    Some((
        ok,
        String::from_utf8(output)
            .ok()?
            .trim_end_matches('\n')
            .to_owned(),
    ))
}

fn check_only(options: &Options, report: &Report) -> Step<()> {
    let Some(current) = options.current_version.as_deref() else {
        return Err(Stop::Usage);
    };
    let target = options
        .bindir
        .as_ref()
        .and_then(|bindir| read_link(&bindir.join("hamn")));
    let manifest_ref = match &options.manifest {
        Some(manifest) => manifest.clone(),
        None => match read_pointer(options.generation.as_deref().or(target.as_deref())) {
            Some(pointer) => pointer,
            None => return fail("missing manifest pointer"),
        },
    };
    // Precedes directories, locks, journal recovery and installation
    // inspection: a check never repairs or changes anything.
    let value = (|| -> Result<serde_json::Value> {
        let (macos, architecture) = (upgrade::system_macos()?, machine()?);
        upgrade::check(
            &manifest_ref,
            current,
            &macos,
            &architecture,
            &home(),
            target.as_deref(),
        )
    })()
    .map_err(|error| manifest_failure(&error.to_string()))?;
    emit(report, &value)
}

/// Writes the JSON result to the result file or stdout.
fn emit(report: &Report, value: &serde_json::Value) -> Step<()> {
    let text = format!("{value}\n");
    let written = match &report.result_file {
        Some(file) => {
            if !files::safe_private_regular(file) {
                return fail("unsafe upgrade result file");
            }
            write_in_place(file, text.as_bytes())
        }
        None => std::io::stdout()
            .write_all(text.as_bytes())
            .and_then(|()| std::io::stdout().flush()),
    };
    written.or_else(|error| fail(format!("cannot report the upgrade result: {error}")))
}

/// The installation as found under the locks.
struct Installation {
    /// No command link exists: a first installation.
    bootstrap: bool,
    /// The active `bin/hamn` target (not for a first installation).
    old_target: Option<String>,
}

/// Validates the managed installation of `roots` (the former
/// `refresh_installation`). A bootstrap that finds a command link proceeds
/// as an update with the same validation and rollback obligations.
fn refresh(bootstrap_entry: bool, roots: &Roots) -> Step<Installation> {
    let link = roots.bindir.join("hamn");
    let is_link = || fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink());
    let mut bootstrap = bootstrap_entry;
    if bootstrap && !files::absent(&link) {
        if !is_link() {
            return fail(format!(
                "{} is not a managed Hamn generation link (an older standalone Hamn or another program); move it aside and run the installer again",
                link.display()
            ));
        }
        bootstrap = false;
    }
    if bootstrap {
        return Ok(Installation {
            bootstrap,
            old_target: None,
        });
    }
    let (bindir, datadir) = (&roots.bindir, &roots.datadir);
    if !files::safe_directory(bindir) {
        return fail(format!(
            "unsafe managed binary directory: {}",
            bindir.display()
        ));
    }
    if !files::safe_directory(datadir) {
        return fail(format!(
            "unsafe managed data directory: {}",
            datadir.display()
        ));
    }
    let marker = datadir.join(".hamn-managed");
    if !files::safe_regular(&marker) {
        return fail("managed data marker is missing or unsafe");
    }
    match files::line(&marker, 4096) {
        Ok(text) if text.is_empty() => {
            return fail(format!(
                "{} is a pre-release Hamn install (empty data marker), which is no longer migrated; move it aside and run the installer again",
                datadir.display()
            ));
        }
        Ok(text) if text == "version=1" => {}
        _ => return fail("managed data marker is invalid"),
    }
    if !is_link() {
        return fail("managed hamn link is missing");
    }
    let Some(target) = read_link(&link) else {
        return fail("cannot read managed hamn link");
    };
    let root = format!("{}/.hamn-generations/", datadir.display());
    if !target.starts_with(&root) || !target.ends_with("/bin/hamn") {
        return fail("managed hamn link points outside its generation root");
    }
    if generation::active_is_previous_layout(roots) {
        return fail(generation::previous_layout_message(&link, datadir));
    }
    Ok(Installation {
        bootstrap: false,
        old_target: Some(target),
    })
}

/// The owner-only directory `path` with exactly `mode`, created if missing.
fn runtime_directory(path: &Path, mode: u32) -> bool {
    if !fs::metadata(path).is_ok_and(|m| m.is_dir())
        && fs::DirBuilder::new().mode(mode).create(path).is_ok()
    {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
    files::owned(path, true, Some(mode)).is_ok()
}

/// Prints one rollback's diagnostics; returns its summary.
fn rollback(paths: &journal::Paths, report: &Report) -> Option<&'static str> {
    match journal::rollback(paths) {
        Ok(summary) => Some(summary),
        Err(RollbackError::Failed(lines)) => {
            for text in lines {
                report.note(&text);
            }
            None
        }
        Err(RollbackError::Legacy(_)) => None,
    }
}

/// HUP/INT/TERM after the journal was published: roll back, then stop.
fn interrupted<T>(signal: i32, paths: &journal::Paths, report: &Report) -> Step<T> {
    let name = interrupt::name(signal);
    match rollback(paths, report) {
        Some(summary) => report.note(&format!("interrupted by {name}; {summary}")),
        None => report.note(&format!(
            "interrupted by {name}; recovery journal remains for a later safe recovery"
        )),
    }
    Err(Stop::Signal(signal))
}

/// A recorded signal, checked between journaled steps.
fn checkpoint(paths: &journal::Paths, report: &Report) -> Step<()> {
    match interrupt::pending() {
        Some(signal) => interrupted(signal, paths, report),
        None => Ok(()),
    }
}

fn signal_of(error: &(dyn std::error::Error + 'static)) -> Option<i32> {
    error
        .downcast_ref::<interrupt::Interrupted>()
        .map(|stopped| stopped.0)
}

/// Rolls back after a failed journaled step and fails with `unapplied` (the
/// journal remains) or `applied` followed by the rollback summary.
fn roll_back_and_fail<T>(
    paths: &journal::Paths,
    report: &Report,
    unapplied: &str,
    applied: &str,
) -> Step<T> {
    match rollback(paths, report) {
        Some(summary) => fail(format!("{applied}; {summary}")),
        None => fail(unapplied),
    }
}

/// A test barrier inside the journaled transaction.
fn journaled_barrier(
    name: &str,
    paths: &journal::Paths,
    report: &Report,
    unapplied: &str,
    applied: &str,
) -> Step<()> {
    match interrupt::barrier(name) {
        Ok(()) => checkpoint(paths, report),
        Err(error) => match signal_of(error.as_ref()) {
            Some(signal) => interrupted(signal, paths, report),
            None => roll_back_and_fail(paths, report, unapplied, applied),
        },
    }
}

/// A private workspace removed when dropped.
struct Workspace(PathBuf);

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Stages the verified guest image as `hamn-guest-<sha256>.img` in the cache
/// with its `.verified` marker; returns the selection to commit.
fn stage_guest(cache: &Path, download: &Path, hash: &str) -> Step<Vec<u8>> {
    let name = format!("hamn-guest-{hash}.img");
    let target = cache.join(&name);
    let digest = |path: &Path| files::digest(path).ok();
    if !files::absent(&target) {
        if !files::safe_regular(&target) {
            return fail("cached guest image is unsafe");
        }
        if digest(&target).as_deref() != Some(hash) && fs::remove_file(&target).is_err() {
            return fail("cannot remove damaged owned guest image");
        }
    }
    if files::absent(&target) {
        let stage = cache.join(format!(".{name}.update.{}", std::process::id()));
        let _ = fs::remove_file(&stage);
        let staged = fs::copy(download, &stage)
            .and_then(|_| fs::set_permissions(&stage, fs::Permissions::from_mode(0o644)))
            .is_ok()
            && digest(&stage).as_deref() == Some(hash);
        if !staged || fs::rename(&stage, &target).is_err() {
            let _ = fs::remove_file(&stage);
            return fail("staged guest image SHA-256 mismatch");
        }
    }
    if !files::safe_regular(&target) {
        return fail("staged guest image is unsafe");
    }
    if digest(&target).as_deref() != Some(hash) {
        return fail("cached guest image SHA-256 mismatch");
    }
    let marked = (|| -> Result<()> {
        let (stage, mut file) = files::temp_file(cache, &format!(".{name}.verified."))?;
        let written = file
            .write_all(format!("{hash}\n").as_bytes())
            .and_then(|()| fs::set_permissions(&stage, fs::Permissions::from_mode(0o644)))
            .and_then(|()| fs::rename(&stage, cache.join(format!("{name}.verified"))));
        if written.is_err() {
            let _ = fs::remove_file(&stage);
        }
        Ok(written?)
    })();
    if marked.is_err() {
        return fail("cannot stage guest image verification marker");
    }
    Ok(format!("{{\"schemaVersion\":1,\"file\":\"{name}\",\"sha256\":\"{hash}\"}}\n").into_bytes())
}

/// Collects obsolete generations; failure only defers cleanup.
fn prune(transaction: &locks::Transaction, keep: &[Option<&str>], report: &Report) {
    let keep: Vec<&str> = keep.iter().flatten().copied().collect();
    match retention::collect(transaction, &keep) {
        Ok(collection) => {
            for deferred in collection.deferred {
                line(&format!("hamn: {deferred}"));
            }
        }
        Err(error) => {
            line(&format!("hamn: {error}"));
            report.note("obsolete generation cleanup deferred");
        }
    }
}

fn transaction(options: &Options, report: &mut Report) -> Step<()> {
    if let Some(file) = &options.result_file {
        if !files::safe_private_regular(file) {
            return fail("unsafe upgrade result file");
        }
        report.result_file = Some(file.clone());
    }
    let report = &*report;
    if options.check_only {
        return check_only(options, report);
    }
    if let Some(current) = &options.current_version {
        if manifest::stable_version(current).is_err() {
            return fail(
                "upgrade requires a stable managed release; reinstall with the official installer",
            );
        }
    }
    let (Some(bindir), Some(datadir)) = (&options.bindir, &options.datadir) else {
        return Err(Stop::Usage);
    };
    let roots = Roots::prepare(bindir, datadir).or_else(|error| fail(error.to_string()))?;
    interrupt::barrier("BEFORE_LOCK")
        .or_else(|error| fail(format!("test barrier failed: {error}")))?;
    let transaction = locks::Transaction::acquire(&roots)
        .or_else(|error| fail(format!("cannot lock the installation roots: {error}")))?;
    refresh(options.bootstrap, &roots)?;

    let home = home();
    if home.as_os_str().is_empty() {
        return fail("HOME is not set");
    }
    let runtime = home.join(".hamn");
    if !runtime_directory(&runtime, 0o700) {
        return fail("unsafe Hamn runtime root");
    }
    let cache = runtime.join("cache");
    if !runtime_directory(&cache, 0o755) {
        return fail("unsafe Hamn image cache");
    }
    // One HOME has one journal path, shared by every install root.
    let _cache_lock = locks::cache(&cache.join(".hamn-upgrade.lock"), &transaction)
        .or_else(|error| fail(format!("cannot lock the update cache: {error}")))?;
    let paths = journal::Paths::new(&cache, &roots.bindir, &roots.datadir);
    let link = paths.link.clone();

    if journal::cleanup_deferred(&paths).is_err() {
        return fail("a deferred update transaction cleanup is unsafe or could not be cleaned");
    }
    if let Err(error) = journal::cleanup_retired(&paths) {
        return match error.legacy {
            Some((version, path)) => fail(format!(
                "a finished v{version} update journal from Hamn 0.1.2 or earlier remains at {}; move it aside and run the command again",
                path.display()
            )),
            None => fail("a retired update transaction is unsafe or could not be cleaned"),
        };
    }
    if !files::absent(&paths.journal) {
        let pending_bootstrap = matches!(journal::load(&paths.journal, &paths.datadir), journal::Loaded::Valid(ref j) if j.bootstrap);
        match journal::rollback(&paths) {
            Ok(summary) if pending_bootstrap => line(&format!("Recovered interrupted bootstrap: {summary}")),
            Ok(_) => report.note("recovered the previous binary and guest image selection after an interrupted update"),
            Err(error) => {
                if let RollbackError::Failed(lines) = &error {
                    for text in lines {
                        report.note(text);
                    }
                }
                report.note("incomplete prior update could not be safely recovered");
                return match error {
                    RollbackError::Legacy(version) => fail(format!(
                        "an interrupted update from Hamn 0.1.2 or earlier left a v{version} journal at {}, which this Hamn cannot recover. \
                         Check that {} and the guest image selection are as you want them (the journal's old-target and previous-selection hold the prior values), \
                         then move the journal aside and run the command again",
                        paths.journal.display(),
                        link.display()
                    )),
                    RollbackError::Failed(_) => fail("previous update recovery failed; no new update was installed"),
                };
            }
        }
    }
    if journal::cleanup_retired(&paths).is_err() {
        return fail("the recovered update transaction could not be cleaned");
    }
    if journal::cleanup_deferred(&paths).is_err() {
        return fail("the recovered transaction cleanup is unsafe or could not be cleaned");
    }

    // Recovery or an earlier lock owner can change the installation. Bind
    // rollback, downgrade checks and bootstrap mode to the state now.
    let installation = refresh(options.bootstrap, &roots)?;
    let current = match &installation.old_target {
        Some(old) => {
            if !journal::managed_target(old, &roots.datadir) {
                return fail("recovered hamn link is unsafe");
            }
            if options.current_version.is_some()
                && !options.bootstrap
                && options.generation.as_deref() != Some(old.as_str())
            {
                return fail(
                    "managed generation changed while waiting or recovering; rerun the managed hamn command",
                );
            }
            match reported_version(Path::new(old)) {
                Some((true, output)) => output.strip_prefix("hamn ").unwrap_or(&output).to_owned(),
                _ => return fail("cannot read installed version"),
            }
        }
        None => "0.0.0".to_owned(),
    };
    if manifest::stable_version(&current).is_err() {
        return fail(
            "active installation is not a stable managed release; reinstall with the official installer",
        );
    }
    let manifest_ref = match &options.manifest {
        Some(manifest) => manifest.clone(),
        None => match read_pointer(
            options
                .generation
                .as_deref()
                .or(installation.old_target.as_deref()),
        ) {
            Some(pointer) => pointer,
            None => return fail("this build has no configured update manifest URL"),
        },
    };
    if manifest_ref.is_empty() {
        return fail("manifest URL is empty");
    }

    let temporary = std::env::var_os("TMPDIR")
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    let work = match files::temp_directory(&temporary, "hamn-update.") {
        Ok(path) => Workspace(path),
        Err(error) => return fail(format!("cannot create a private update workspace: {error}")),
    };
    // The installer supplies a verified local manifest; only updates fetch.
    if !options.bootstrap {
        line("Checking for updates...");
    }
    let (manifest, manifest_bytes) = (|| -> Result<(Manifest, u64)> {
        let (bytes, amount) = download::fetch_manifest(&manifest_ref, false)?;
        Ok((
            manifest::parse(&bytes, &upgrade::system_macos()?, &machine()?)?,
            amount,
        ))
    })()
    .map_err(|error| manifest_failure(&error.to_string()))?;
    let mut counts = BTreeMap::from([(
        "manifest".to_owned(),
        Counts {
            downloaded_bytes: manifest_bytes,
            source: "manifest".into(),
            ..Counts::default()
        },
    )]);
    let release = manifest.version.trim_start_matches('v').to_owned();
    let (host_hash, guest_hash) = (
        manifest.artifacts.host.sha256.clone(),
        manifest.artifacts.guest_image.sha256.clone(),
    );
    let Ok(status) = upgrade::version_status(
        &current,
        &manifest,
        &cache,
        installation.old_target.as_deref(),
    ) else {
        return fail("unsupported installed version");
    };
    if status == "ahead" {
        return fail(if options.bootstrap {
            format!(
                "stable downgrade is not permitted: Hamn {current} is already installed, which is newer than this installer ({release}); run hamn upgrade to stay current"
            )
        } else {
            format!(
                "stable downgrade is not permitted: installed Hamn {current} is newer than the latest release ({release})"
            )
        });
    }
    let finish = |status: &str, counts: &BTreeMap<String, Counts>| -> Step<()> {
        if !options.output_json {
            return Ok(());
        }
        let value = upgrade::result(&current, &manifest, status, counts.clone())
            .or_else(|error| fail(format!("cannot report the upgrade result: {error}")))?;
        emit(report, &value)
    };
    let keep = [
        installation.old_target.as_deref(),
        options.generation.as_deref(),
    ];

    // A receipt is advisory: absent, malformed, unsafe or stale evidence
    // forces the verified install. A version string alone never permits
    // skipping downloads. Each generation owns its receipt.
    if let Some(old) = &installation.old_target {
        if !options.force
            && status == "up-to-date"
            && upgrade::receipt_run("check", old, &manifest, &cache).is_ok()
            && read_link(&link).as_deref() == Some(old.as_str())
            && files::absent(&paths.journal)
        {
            line(&if options.bootstrap {
                format!("Hamn {release} is already installed.")
            } else {
                format!("Hamn {release} is up to date.")
            });
            counts.extend(
                upgrade::installed_counts(&manifest, &["host", "guestImage"])
                    .or_else(|error| fail(error.to_string()))?,
            );
            finish("up-to-date", &counts)?;
            prune(&transaction, &keep, report);
            return Ok(());
        }
    }
    let previous_version = installation.old_target.as_ref().map(|_| current.clone());
    let mut host_mutation = true;
    if !options.force && status == "repair-required" {
        if let Some(old) = &installation.old_target {
            if upgrade::receipt_run("host-check", old, &manifest, &cache).is_ok() {
                host_mutation = false;
                counts.extend(
                    upgrade::installed_counts(&manifest, &["host"])
                        .or_else(|error| fail(error.to_string()))?,
                );
            }
        }
    }
    match &previous_version {
        Some(_) if !host_mutation => line(&format!("Repairing the Hamn {release} guest image...")),
        Some(previous) if *previous == release => line(&format!("Reinstalling Hamn {release}...")),
        Some(previous) => line(&format!("Updating Hamn {previous} → {release}...")),
        None => {}
    }
    // The downloader prints its own progress (nothing for a verified cache
    // hit) and returns the transfer reason on failure.
    let mut acquire = |name: &str, what: &str| -> Step<PathBuf> {
        let label = if name == "host" {
            format!("Downloading Hamn {release}")
        } else {
            "Downloading guest image".to_owned()
        };
        let acquired = manifest
            .artifact(name)
            .and_then(|artifact| download::acquire(&cache, &artifact.acquisition(), name, &label));
        match acquired {
            Ok((path, record)) => {
                counts.insert(name.to_owned(), record);
                Ok(path)
            }
            Err(error) => {
                let mut reason = error.to_string().replace('\n', " ");
                if reason.len() > 1024 {
                    let mut end = 1024;
                    while !reason.is_char_boundary(end) {
                        end -= 1;
                    }
                    reason.truncate(end);
                }
                match reason.strip_prefix("download failed: ") {
                    Some(rest) if options.bootstrap => fail(format!(
                        "could not download {what}: {rest}. Check your connection and run the installer again to resume."
                    )),
                    Some(rest) => fail(format!(
                        "could not download {what}: {rest}. Check your connection and run hamn upgrade again to resume."
                    )),
                    None if reason.is_empty() => {
                        fail(format!("could not obtain {what}: unknown error"))
                    }
                    None => fail(format!("could not obtain {what}: {reason}")),
                }
            }
        }
    };
    let host_archive = if host_mutation {
        Some(acquire("host", &format!("Hamn {release}"))?)
    } else {
        None
    };
    let guest_download = acquire("guestImage", "the guest image")?;
    line("Installing...");
    if files::digest(&guest_download).ok().as_deref() != Some(guest_hash.as_str()) {
        return fail("guest image SHA-256 mismatch");
    }
    let artifact = match &host_archive {
        None => None,
        Some(archive) => {
            if files::digest(archive).ok().as_deref() != Some(host_hash.as_str()) {
                return fail("host artifact SHA-256 mismatch");
            }
            let Ok(root) = archive::extract(archive, &work.0.join("extract")) else {
                return fail("host artifact validation or extraction failed");
            };
            let artifact = work.0.join("extract").join(root);
            let binary = artifact.join("bin/hamn");
            let complete = fs::metadata(&binary)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                && fs::symlink_metadata(artifact.join(POINTER)).is_ok_and(|m| m.is_file());
            if !complete {
                return fail("extracted host artifact is incomplete");
            }
            if reported_version(&binary).map(|(_, output)| output)
                != Some(format!("hamn {release}"))
            {
                return fail("host binary version does not match the release manifest");
            }
            Some(artifact)
        }
    };
    let new_selection = stage_guest(&cache, &guest_download, &guest_hash)?;

    let prepared = journal::prepare(
        &paths,
        &Plan {
            bootstrap: installation.bootstrap,
            host_mutation,
            old_target: installation.old_target.as_deref(),
            new_selection: &new_selection,
        },
    );
    let prepared = match prepared {
        Ok(journal) => journal,
        Err(error) => {
            report.note(&error.to_string());
            return fail("cannot record a durable update rollback transaction");
        }
    };
    interrupt::arm();
    journaled_barrier(
        "PREPARED",
        &paths,
        report,
        "prepared transaction barrier recovery failed",
        "prepared transaction barrier failed",
    )?;

    if let Some(artifact) = &artifact {
        let payload = Payload {
            binary: artifact.join("bin/hamn"),
            pointer: Some(artifact.join(POINTER)),
        };
        let journaled = generation::Journaled {
            paths: &paths,
            attempt: &prepared.attempt,
        };
        if let Err(error) = generation::install(&payload, &transaction, Some(&journaled)) {
            if let Some(signal) = signal_of(error.as_ref()) {
                return interrupted(signal, &paths, report);
            }
            // A closed diagnostic stream must not prevent transaction recovery.
            line(&format!("hamn: {error}"));
            return roll_back_and_fail(
                &paths,
                report,
                "host install failed and the recovery journal could not be applied",
                "host install failed",
            );
        }
        checkpoint(&paths, report)?;
        journaled_barrier(
            "AFTER_HOST_INSTALL",
            &paths,
            report,
            "update interruption barrier failed and the recovery journal could not be applied",
            "update interruption barrier failed",
        )?;
        let installed = read_link(&link);
        let verified = (|| -> Result<bool> {
            let Some(installed) = &installed else {
                return Ok(false);
            };
            let journal::Loaded::Valid(journal) = journal::load(&paths.journal, &paths.datadir)
            else {
                return Ok(false);
            };
            if journal.new_target.as_deref() != Some(installed.as_str())
                || !journal::managed_target(installed, &paths.datadir)
                || files::digest(Path::new(installed))? != files::digest(&payload.binary)?
            {
                return Ok(false);
            }
            interrupt::fault("receipt-write")?;
            upgrade::receipt_run("write", installed, &manifest, &cache)?;
            Ok(true)
        })();
        if !matches!(verified, Ok(true)) {
            return match rollback(&paths, report) {
                Some(_) => fail("release receipt failed; update transaction recovered"),
                None => fail("release receipt failed and recovery could not be applied"),
            };
        }
        files::sync();
        checkpoint(&paths, report)?;
    }

    if journal::commit_selection(&paths).is_err() {
        return roll_back_and_fail(
            &paths,
            report,
            "guest image commit failed and the recovery journal could not be applied",
            "guest image commit failed",
        );
    }
    checkpoint(&paths, report)?;
    match interrupt::barrier("AFTER_GUEST_SELECTION") {
        Ok(()) => checkpoint(&paths, report)?,
        Err(error) => match signal_of(error.as_ref()) {
            Some(signal) => return interrupted(signal, &paths, report),
            None => {
                return match rollback(&paths, report) {
                    Some(_) => fail("guest selection barrier failed"),
                    None => fail("guest selection barrier recovery failed"),
                };
            }
        },
    }
    if journal::retire(&paths, "completed").is_err() {
        return roll_back_and_fail(
            &paths,
            report,
            "update commit could not clear its recovery journal; retry the same command with all original options (including --manifest) before starting a VM",
            "update commit metadata could not be cleared",
        );
    }
    // Committed: a signal from now on ends the process with the committed
    // binary and selection active; only cleanup and reporting remain.
    interrupt::disarm();
    if interrupt::barrier("AFTER_JOURNAL_RETIRE").is_err() {
        return fail(
            "update completion barrier failed; the completed transaction is safely retired",
        );
    }
    if journal::cleanup_retired(&paths).is_err() {
        report.note("completed transaction cleanup is deferred; the committed binary and guest image selection are active");
    }
    if journal::cleanup_deferred(&paths).is_err() {
        report.note("completed transaction cleanup remains deferred; the committed binary and guest image selection are active");
    }
    prune(&transaction, &keep, report);
    line(&match &previous_version {
        None => format!("Installed Hamn {release}."),
        Some(_) if !host_mutation => {
            format!("Repaired the Hamn {release} guest image. Existing VMs were not restarted.")
        }
        Some(previous) if *previous == release => {
            format!("Reinstalled Hamn {release}. Existing VMs were not restarted.")
        }
        Some(previous) => {
            format!("Updated Hamn {previous} → {release}. Existing VMs were not restarted.")
        }
    });
    // The notice is advisory and never authorizes installation. Drop it only
    // after a successful transaction; an unsafe entry is left for repair.
    let notice = cache.join("update-notice-v1.json");
    if files::safe_private_regular(&notice) {
        let _ = fs::remove_file(notice);
    }
    finish(if host_mutation { "updated" } else { "repaired" }, &counts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn options_are_single_valued_and_frontend_identity_is_complete() {
        let accepted = parse(&words(
            "--bindir b --datadir d --manifest m --bootstrap --output-json --result-file r",
        ))
        .unwrap();
        assert!(
            accepted.bootstrap && accepted.output_json && accepted.manifest.as_deref() == Some("m")
        );
        for rejected in [
            "--bindir a --bindir b",
            "--bootstrap --bootstrap",
            "--check-only --force",
            "--current-version 1.2.3",
            "--generation /x/bin/hamn",
            "--manifest",
            "--unknown",
        ] {
            assert!(parse(&words(rejected)).is_none(), "{rejected}");
        }
        assert!(
            parse(&words(
                "--current-version 1.2.3 --generation /x/bin/hamn --check-only"
            ))
            .is_some()
        );
    }

    #[test]
    fn manifest_failures_distinguish_transfer_from_unusable_metadata() {
        let Stop::Fail(transfer) =
            manifest_failure("download failed: the connection stalled or timed out (curl exit 28)")
        else {
            panic!()
        };
        assert_eq!(
            transfer,
            "could not check for updates: the connection stalled or timed out (curl exit 28). Check your connection and try again."
        );
        let Stop::Fail(unusable) = manifest_failure("") else {
            panic!()
        };
        assert!(
            unusable.contains("(unknown error)") && unusable.ends_with(INSTALL_URL),
            "{unusable}"
        );
    }

    #[test]
    fn machine_reports_the_running_architecture() {
        let expected = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            std::env::consts::ARCH
        };
        assert_eq!(machine().unwrap(), expected);
    }
}
