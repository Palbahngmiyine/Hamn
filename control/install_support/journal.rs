//! The durable update journal (version 3) and its recovery.
//!
//! Layout. `~/.hamn/cache/.hamn-update-transaction/` (0700, owned) holds
//! only owned 0600 single-link files:
//! - `state`: `version=3`, `bootstrap=0|1`, `selection=present|absent`,
//!   `hostMutation=0|1`, one per line;
//! - `attempt`: the six-character `[A-Za-z0-9]` transaction identity;
//! - `new-selection`: the guest image selection to commit;
//! - `new-target`: empty until the attempted generation is recorded, then
//!   exactly its `bin/hamn` path and a newline;
//! - `old-target` (updates only) and `previous-selection` (when a selection
//!   existed): what rollback restores.
//!
//! A bootstrap always mutates the host; selection-only repair never records a
//! target. Every target must be `DATADIR/.hamn-generations/<sha256>-<suffix>/
//! bin/hamn` of the canonical data root, an owned single-link regular file.
//!
//! Recovery changes state only when the active command link is the recorded
//! previous or attempted generation (a first install may have no link yet);
//! otherwise it changes nothing and reports that ownership cannot be proven.
//! A finished journal is renamed `.hamn-update-{completed,recovered}.<attempt>`
//! and then removed through `.hamn-update-cleanup.<outcome>.<attempt>`, so
//! each step is retryable. Journals of versions 1 and 2 (Hamn 0.1.2 and
//! earlier, pre-release builds) name no attempted target and are refused.
//!
//! The caller holds the transaction and cache locks for every operation.
use super::{Result, files, interrupt, require};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub(super) const NAME: &str = ".hamn-update-transaction";
const ENTRIES: [&str; 6] = [
    "state",
    "attempt",
    "new-selection",
    "previous-selection",
    "old-target",
    "new-target",
];

/// The paths one transaction works on; `bindir`/`datadir` are canonical.
pub(super) struct Paths {
    pub(super) cache: PathBuf,
    pub(super) journal: PathBuf,
    pub(super) selection: PathBuf,
    pub(super) link: PathBuf,
    pub(super) bindir: PathBuf,
    pub(super) datadir: PathBuf,
}

impl Paths {
    pub(super) fn new(cache: &Path, bindir: &Path, datadir: &Path) -> Self {
        Self {
            cache: cache.to_path_buf(),
            journal: cache.join(NAME),
            selection: cache.join("guest-image.json"),
            link: bindir.join("hamn"),
            bindir: bindir.to_path_buf(),
            datadir: datadir.to_path_buf(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Journal {
    pub(super) bootstrap: bool,
    pub(super) selection_present: bool,
    pub(super) host_mutation: bool,
    pub(super) attempt: String,
    pub(super) old_target: Option<String>,
    pub(super) new_target: Option<String>,
}

#[derive(Debug, PartialEq)]
pub(super) enum Loaded {
    Valid(Journal),
    /// A version 1 or 2 journal (no attempted-target identity).
    Legacy(u32),
    Invalid,
}

/// `DATADIR/.hamn-generations/<sha256>-<suffix>/bin/hamn` of the canonical
/// `datadir`, as an owned single-link regular file.
pub(super) fn managed_target(target: &str, datadir: &Path) -> bool {
    if !files::safe_directory(datadir) {
        return false;
    }
    let Ok(root) = fs::canonicalize(datadir) else {
        return false;
    };
    let Some(root) = root.to_str() else {
        return false;
    };
    let Some(relative) = target.strip_prefix(&format!("{root}/.hamn-generations/")) else {
        return false;
    };
    let Some(name) = relative.strip_suffix("/bin/hamn") else {
        return false;
    };
    generation_name(name) && files::safe_regular(Path::new(target))
}

/// `<64 lowercase hex>-<6 [A-Za-z0-9]>`.
pub(super) fn generation_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 71
        && bytes[..64]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
        && bytes[64] == b'-'
        && bytes[65..].iter().all(u8::is_ascii_alphanumeric)
}

fn attempt_valid(value: &str) -> bool {
    value.len() == 6 && value.bytes().all(|c| c.is_ascii_alphanumeric())
}

/// Reads and validates the journal directory `directory` (see the module
/// documentation); `datadir` is the canonical data root.
pub(super) fn load(directory: &Path, datadir: &Path) -> Loaded {
    match load_checked(directory, datadir) {
        Ok(loaded) => loaded,
        Err(_) => Loaded::Invalid,
    }
}

fn load_checked(directory: &Path, datadir: &Path) -> Result<Loaded> {
    let file = |name: &str| directory.join(name);
    if !files::safe_private_directory(directory) || !files::safe_private_regular(&file("state")) {
        return Ok(Loaded::Invalid);
    }
    let state = files::line(&file("state"), 4096)?;
    for version in [1, 2] {
        if state.starts_with(&format!("version={version}\n")) {
            return Ok(Loaded::Legacy(version));
        }
    }
    let lines: Vec<&str> = state.split('\n').collect();
    let field = |index: usize, key: &str, values: &[&str]| -> Option<usize> {
        let value = lines.get(index)?.strip_prefix(key)?;
        values.iter().position(|allowed| *allowed == value)
    };
    let (Some(0), Some(bootstrap), Some(selection), Some(host), 4) = (
        field(0, "version=", &["3"]),
        field(1, "bootstrap=", &["0", "1"]),
        field(2, "selection=", &["absent", "present"]),
        field(3, "hostMutation=", &["0", "1"]),
        lines.len(),
    ) else {
        return Ok(Loaded::Invalid);
    };
    let (bootstrap, selection_present, host_mutation) = (bootstrap == 1, selection == 1, host == 1);
    for name in ["attempt", "new-selection", "new-target"] {
        if !files::safe_private_regular(&file(name)) {
            return Ok(Loaded::Invalid);
        }
    }
    let attempt = files::line(&file("attempt"), 64)?;
    let new_target = files::line(&file("new-target"), 4096)?;
    let new_target = if new_target.is_empty() {
        if fs::symlink_metadata(file("new-target"))?.len() != 0 {
            return Ok(Loaded::Invalid);
        }
        None
    } else {
        if !managed_target(&new_target, datadir)
            || fs::symlink_metadata(file("new-target"))?.len() != new_target.len() as u64 + 1
        {
            return Ok(Loaded::Invalid);
        }
        Some(new_target)
    };
    if !attempt_valid(&attempt)
        || (bootstrap && !host_mutation)
        || (!host_mutation && new_target.is_some())
    {
        return Ok(Loaded::Invalid);
    }
    let expected = 4 + usize::from(!bootstrap) + usize::from(selection_present);
    if selection_present != files::safe_private_regular(&file("previous-selection"))
        || (!selection_present && !files::absent(&file("previous-selection")))
    {
        return Ok(Loaded::Invalid);
    }
    let old_target = if bootstrap {
        if !files::absent(&file("old-target")) {
            return Ok(Loaded::Invalid);
        }
        None
    } else {
        if !files::safe_private_regular(&file("old-target")) {
            return Ok(Loaded::Invalid);
        }
        let old = files::line(&file("old-target"), 4096)?;
        if old.contains('\n') || !managed_target(&old, datadir) {
            return Ok(Loaded::Invalid);
        }
        Some(old)
    };
    let count = fs::read_dir(directory)?.count();
    if count != expected {
        return Ok(Loaded::Invalid);
    }
    Ok(Loaded::Valid(Journal {
        bootstrap,
        selection_present,
        host_mutation,
        attempt,
        old_target,
        new_target,
    }))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    files::create(path, bytes, 0o600)
}

/// Removes a journal stage this process created, only by its known names.
fn discard_stage(stage: &Path) {
    if files::safe_private_directory(stage) {
        for name in ENTRIES {
            let _ = fs::remove_file(stage.join(name));
        }
        let _ = fs::remove_dir(stage);
    }
}

/// What a new transaction records before it changes anything.
pub(super) struct Plan<'a> {
    pub(super) bootstrap: bool,
    pub(super) host_mutation: bool,
    /// The active target (required unless `bootstrap`).
    pub(super) old_target: Option<&'a str>,
    pub(super) new_selection: &'a [u8],
}

/// Publishes a complete journal atomically (stage, flush, exclusive rename)
/// and returns it as loaded back. Records a recovery root in the previous
/// generation first, so collection keeps it while the journal exists.
pub(super) fn prepare(paths: &Paths, plan: &Plan) -> Result<Journal> {
    require(
        files::absent(&paths.journal),
        "another update transaction is already active",
    )?;
    let stage = files::temp_directory(&paths.cache, &format!("{NAME}."))?;
    let result = (|| -> Result<Journal> {
        require(
            files::safe_private_directory(&stage),
            "unsafe journal stage",
        )?;
        let attempt = files::temp_suffix(&stage)?;
        let selection_present = !files::absent(&paths.selection);
        if selection_present {
            require(
                files::safe_regular(&paths.selection),
                "unsafe guest image selection",
            )?;
            write_private(
                &stage.join("previous-selection"),
                &fs::read(&paths.selection)?,
            )?;
        }
        if !plan.bootstrap {
            let old = plan.old_target.ok_or("missing previous target")?;
            require(
                managed_target(old, &paths.datadir),
                "unsafe previous target",
            )?;
            // Remember every recovery root using this generation, including
            // callers with other HOME directories, before the journal exists.
            files::recovery_root(Path::new(old), &paths.cache)?;
            write_private(&stage.join("old-target"), format!("{old}\n").as_bytes())?;
        }
        write_private(&stage.join("new-selection"), plan.new_selection)?;
        let state = format!(
            "version=3\nbootstrap={}\nselection={}\nhostMutation={}\n",
            u8::from(plan.bootstrap),
            if selection_present {
                "present"
            } else {
                "absent"
            },
            u8::from(plan.host_mutation)
        );
        write_private(&stage.join("state"), state.as_bytes())?;
        write_private(&stage.join("new-target"), b"")?;
        write_private(&stage.join("attempt"), format!("{attempt}\n").as_bytes())?;
        files::sync();
        files::rename_exclusive(&stage, &paths.journal)?;
        match load(&paths.journal, &paths.datadir) {
            Loaded::Valid(journal) if journal.attempt == attempt => {
                files::sync();
                Ok(journal)
            }
            _ => Err("published journal does not validate".into()),
        }
    })();
    if result.is_err() && stage.exists() {
        discard_stage(&stage);
    }
    result
}

/// Durably records the attempted generation target (before its link is
/// published) and remembers this cache as a recovery root of it.
pub(super) fn record_target(paths: &Paths, target: &str) -> Result<()> {
    files::recovery_root(Path::new(target), &paths.cache)?;
    let (stage, mut file) = files::temp_file(&paths.cache, ".hamn-target.")?;
    let result = (|| -> Result<()> {
        file.write_all(format!("{target}\n").as_bytes())?;
        file.sync_all()?;
        files::sync();
        fs::rename(&stage, paths.journal.join("new-target"))?;
        files::sync();
        let recorded = paths.journal.join("new-target");
        require(
            files::safe_private_regular(&recorded)
                && fs::read(&recorded)? == format!("{target}\n").as_bytes(),
            "attempted target was not recorded",
        )
    })();
    if result.is_err() {
        let _ = fs::remove_file(&stage);
    }
    result
}

/// Replaces the selection with a verified private copy of `source`.
fn install_selection(paths: &Paths, source: &Path, prefix: &str) -> Result<()> {
    let bytes = fs::read(source)?;
    let (stage, mut file) = files::temp_file(&paths.cache, prefix)?;
    let result = (|| -> Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        require(fs::read(&stage)? == bytes, "staged selection differs")?;
        fs::rename(&stage, &paths.selection)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&stage);
        return result;
    }
    files::sync();
    require(
        files::safe_regular(&paths.selection) && fs::read(&paths.selection)? == bytes,
        "selection was not committed",
    )
}

/// Makes the journal's new selection the active guest image selection.
pub(super) fn commit_selection(paths: &Paths) -> Result<()> {
    require(
        matches!(load(&paths.journal, &paths.datadir), Loaded::Valid(_)),
        "unsafe journal",
    )?;
    install_selection(
        paths,
        &paths.journal.join("new-selection"),
        ".guest-image.json.update.",
    )
}

fn restore_selection(paths: &Paths, journal: &Journal) -> Result<()> {
    if journal.selection_present {
        return install_selection(
            paths,
            &paths.journal.join("previous-selection"),
            ".guest-image.json.rollback.",
        );
    }
    if !files::absent(&paths.selection) {
        require(
            files::safe_regular(&paths.selection),
            "unsafe guest image selection",
        )?;
        fs::remove_file(&paths.selection)?;
        files::sync();
    }
    require(files::absent(&paths.selection), "selection was not removed")
}

fn read_link(link: &Path) -> Option<String> {
    let target = fs::read_link(link).ok()?;
    target.to_str().map(str::to_owned)
}

fn restore_link(paths: &Paths, journal: &Journal) -> Result<()> {
    if journal.bootstrap {
        return Ok(());
    }
    let old = journal
        .old_target
        .as_deref()
        .ok_or("missing previous target")?;
    require(
        managed_target(old, &paths.datadir),
        "unsafe previous target",
    )?;
    require(
        fs::symlink_metadata(&paths.link).is_ok_and(|m| m.file_type().is_symlink()),
        "managed hamn link is missing",
    )?;
    let current = read_link(&paths.link).ok_or("cannot read managed hamn link")?;
    if current == old {
        return Ok(());
    }
    require(
        managed_target(&current, &paths.datadir),
        "unsafe active target",
    )?;
    let stage = files::temp_directory(&paths.bindir, ".hamn-update-rollback.")?;
    let staged = stage.join("hamn");
    let result = (|| -> Result<()> {
        std::os::unix::fs::symlink(old, &staged)?;
        interrupt::fault("rollback-link")?;
        fs::rename(&staged, &paths.link)?;
        Ok(())
    })();
    let _ = fs::remove_file(&staged);
    let _ = fs::remove_dir(&stage);
    result?;
    files::sync();
    require(
        read_link(&paths.link).as_deref() == Some(old),
        "rollback link was not restored",
    )
}

/// Whether the active link proves this journal's ownership.
fn owns_active(paths: &Paths, journal: &Journal) -> bool {
    if files::absent(&paths.link) {
        // A first install may have recorded its target but not exposed it.
        return journal.bootstrap;
    }
    if !fs::symlink_metadata(&paths.link).is_ok_and(|m| m.file_type().is_symlink()) {
        return false;
    }
    let Some(current) = read_link(&paths.link) else {
        return false;
    };
    if !managed_target(&current, &paths.datadir) {
        return false;
    }
    if !journal.bootstrap && journal.old_target.as_deref() == Some(current.as_str()) {
        return true;
    }
    // Only the recorded attempted target identifies this transaction's own
    // install; an unrelated later install (another HOME) is preserved.
    journal.host_mutation && journal.new_target.as_deref() == Some(current.as_str())
}

/// Why a rollback did not happen.
pub(super) enum RollbackError {
    Legacy(u32),
    /// Diagnostics for the caller to print, one per line.
    Failed(Vec<String>),
}

/// Restores the recorded selection and (for host mutations) command link,
/// then retires the journal as recovered. Returns the human summary.
pub(super) fn rollback(paths: &Paths) -> std::result::Result<&'static str, RollbackError> {
    let journal = match load(&paths.journal, &paths.datadir) {
        Loaded::Valid(journal) => journal,
        Loaded::Legacy(version) => return Err(RollbackError::Legacy(version)),
        Loaded::Invalid => return Err(RollbackError::Failed(Vec::new())),
    };
    // Root locks serialize current writers but do not make another HOME's
    // older journal authoritative. Check identity before changing anything.
    if !owns_active(paths, &journal) {
        return Err(RollbackError::Failed(vec![
            "pending transaction does not own the active generation; preserving its journal and both selections".into(),
            "the active generation is neither the journal's previous nor its attempted generation; automatic rollback cannot prove ownership".into(),
            "manual review of the retained journal and generation history is required; retrying alone will not resolve this ambiguity".into(),
        ]));
    }
    let restored = (|| -> Result<()> {
        restore_selection(paths, &journal)?;
        if journal.host_mutation {
            restore_link(paths, &journal)?;
        }
        retire(paths, "recovered")
    })();
    match restored {
        Ok(()) if journal.bootstrap => Ok(
            "guest image selection was restored; no previous binary was recorded, so a published command may remain",
        ),
        Ok(()) => Ok("prior binary and guest image selection were restored"),
        Err(_) => Err(RollbackError::Failed(Vec::new())),
    }
}

/// Renames the valid active journal to `.hamn-update-<outcome>.<attempt>`.
pub(super) fn retire(paths: &Paths, outcome: &str) -> Result<()> {
    require(
        matches!(outcome, "completed" | "recovered"),
        "unknown journal outcome",
    )?;
    let Loaded::Valid(journal) = load(&paths.journal, &paths.datadir) else {
        return Err("unsafe journal".into());
    };
    let retired = paths
        .cache
        .join(format!(".hamn-update-{outcome}.{}", journal.attempt));
    require(files::absent(&retired), "retired journal already exists")?;
    interrupt::fault("retire-journal")?;
    files::rename_exclusive(&paths.journal, &retired)?;
    require(files::absent(&paths.journal), "journal was not retired")?;
    files::sync();
    Ok(())
}

/// Removes one deferred cleanup directory, accepting only journal files.
fn remove_deferred(deferred: &Path) -> Result<()> {
    require(
        files::safe_private_directory(deferred),
        "unsafe deferred journal",
    )?;
    for entry in fs::read_dir(deferred)? {
        let entry = entry?.path();
        let name = entry.file_name().and_then(|n| n.to_str()).unwrap_or("");
        require(
            ENTRIES.contains(&name) && files::safe_private_regular(&entry),
            "unsafe deferred journal entry",
        )?;
    }
    for name in ENTRIES {
        match fs::remove_file(deferred.join(name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    fs::remove_dir(deferred)?;
    Ok(())
}

fn named(cache: &Path, prefix: &str) -> Result<Vec<(PathBuf, String)>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(cache)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_owned();
        if name.starts_with(prefix) {
            entries.push((path, name));
        }
    }
    entries.sort();
    Ok(entries)
}

/// `<outcome>.<attempt>` after `prefix`, with a known outcome.
fn outcome_attempt<'a>(name: &'a str, prefix: &str) -> Option<(&'a str, &'a str)> {
    let (outcome, attempt) = name.strip_prefix(prefix)?.split_once('.')?;
    (matches!(outcome, "completed" | "recovered") && attempt_valid(attempt))
        .then_some((outcome, attempt))
}

/// Removes every `.hamn-update-cleanup.<outcome>.<attempt>` directory.
pub(super) fn cleanup_deferred(paths: &Paths) -> Result<()> {
    for (path, name) in named(&paths.cache, ".hamn-update-cleanup.")? {
        require(
            outcome_attempt(&name, ".hamn-update-cleanup.").is_some(),
            "unsafe deferred journal name",
        )?;
        remove_deferred(&path)?;
    }
    Ok(())
}

/// Why a retired journal could not be removed.
pub(super) struct CleanupError {
    /// A retired journal of version 1 or 2, and where it is.
    pub(super) legacy: Option<(u32, PathBuf)>,
}

/// Moves each valid retired journal to its cleanup name and removes it.
pub(super) fn cleanup_retired(paths: &Paths) -> std::result::Result<(), CleanupError> {
    let fail = |legacy| CleanupError { legacy };
    let mut retired = named(&paths.cache, ".hamn-update-completed.").map_err(|_| fail(None))?;
    retired.extend(named(&paths.cache, ".hamn-update-recovered.").map_err(|_| fail(None))?);
    for (path, name) in retired {
        let Some((outcome, attempt)) = outcome_attempt(&name, ".hamn-update-") else {
            return Err(fail(None));
        };
        match load(&path, &paths.datadir) {
            Loaded::Valid(_) => {}
            Loaded::Legacy(version) => return Err(fail(Some((version, path)))),
            Loaded::Invalid => return Err(fail(None)),
        }
        let deferred = paths
            .cache
            .join(format!(".hamn-update-cleanup.{outcome}.{attempt}"));
        if !files::absent(&deferred) || files::rename_exclusive(&path, &deferred).is_err() {
            return Err(fail(None));
        }
        files::sync();
        remove_deferred(&deferred).map_err(|_| fail(None))?;
    }
    Ok(())
}

/// The owner and mode of `path`, for diagnostics in tests.
#[cfg(test)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(path).unwrap().mode() & 0o7777
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_support::test_support::Temp;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    struct Fixture {
        _temp: Temp,
        paths: Paths,
        targets: [String; 3],
    }

    /// A data root with three generations and a cache with a selection.
    fn fixture() -> Fixture {
        let temp = Temp::new();
        let root = fs::canonicalize(&temp.0).unwrap();
        let (bindir, datadir, cache) = (root.join("bin"), root.join("data"), root.join("cache"));
        for (path, mode) in [(&bindir, 0o755), (&datadir, 0o755), (&cache, 0o755)] {
            fs::DirBuilder::new().mode(mode).create(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        }
        let targets = ["a", "b", "c"].map(|seed| {
            let generation =
                datadir.join(format!(".hamn-generations/{}-Abc12{seed}", seed.repeat(64)));
            fs::create_dir_all(generation.join("bin")).unwrap();
            fs::write(generation.join("bin/hamn"), seed).unwrap();
            generation.join("bin/hamn").to_str().unwrap().to_owned()
        });
        let paths = Paths::new(&cache, &bindir, &datadir);
        fs::write(&paths.selection, "previous").unwrap();
        std::os::unix::fs::symlink(&targets[0], &paths.link).unwrap();
        Fixture {
            _temp: temp,
            paths,
            targets,
        }
    }

    fn plan<'a>(old: &'a str) -> Plan<'a> {
        Plan {
            bootstrap: false,
            host_mutation: true,
            old_target: Some(old),
            new_selection: b"next",
        }
    }

    #[test]
    fn prepared_journal_round_trips_and_every_entry_is_private() {
        let f = fixture();
        let journal = prepare(&f.paths, &plan(&f.targets[0])).unwrap();
        assert_eq!(journal.old_target.as_deref(), Some(f.targets[0].as_str()));
        assert_eq!(
            (journal.new_target, journal.selection_present),
            (None, true)
        );
        assert_eq!(mode(&f.paths.journal), 0o700);
        for entry in fs::read_dir(&f.paths.journal).unwrap() {
            assert_eq!(mode(&entry.unwrap().path()), 0o600);
        }
        assert!(
            prepare(&f.paths, &plan(&f.targets[0])).is_err(),
            "a second transaction started"
        );
        record_target(&f.paths, &f.targets[1]).unwrap();
        let Loaded::Valid(loaded) = load(&f.paths.journal, &f.paths.datadir) else {
            panic!()
        };
        assert_eq!(loaded.new_target.as_deref(), Some(f.targets[1].as_str()));
    }

    #[test]
    fn malformed_journals_are_invalid_and_legacy_versions_are_named() {
        let f = fixture();
        prepare(&f.paths, &plan(&f.targets[0])).unwrap();
        let state = f.paths.journal.join("state");
        let valid = fs::read(&state).unwrap();
        for (bad, expected) in [
            (
                &b"version=1\nbootstrap=0\nselection=present\n"[..],
                Loaded::Legacy(1),
            ),
            (
                b"version=2\nbootstrap=0\nselection=present\nhostMutation=1\n",
                Loaded::Legacy(2),
            ),
            (
                b"version=4\nbootstrap=0\nselection=present\nhostMutation=1\n",
                Loaded::Invalid,
            ),
            (
                b"version=3\nbootstrap=0\nselection=present\nhostMutation=1\nextra=1\n",
                Loaded::Invalid,
            ),
            (
                b"version=3\nbootstrap=1\nselection=present\nhostMutation=0\n",
                Loaded::Invalid,
            ),
            (
                b"version=3\nbootstrap=1\nselection=present\nhostMutation=1\n",
                Loaded::Invalid,
            ),
        ] {
            fs::write(&state, bad).unwrap();
            assert_eq!(
                load(&f.paths.journal, &f.paths.datadir),
                expected,
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        fs::write(&state, &valid).unwrap();
        for (name, contents) in [
            ("attempt", &b"short\n"[..]),
            ("new-target", b"/elsewhere/bin/hamn\n"),
            ("old-target", b"/elsewhere/bin/hamn\n"),
        ] {
            let path = f.paths.journal.join(name);
            let saved = fs::read(&path).unwrap();
            fs::write(&path, contents).unwrap();
            assert_eq!(
                load(&f.paths.journal, &f.paths.datadir),
                Loaded::Invalid,
                "{name}"
            );
            fs::write(&path, saved).unwrap();
        }
        fs::write(f.paths.journal.join("unexpected"), "").unwrap();
        fs::set_permissions(
            f.paths.journal.join("unexpected"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert_eq!(
            load(&f.paths.journal, &f.paths.datadir),
            Loaded::Invalid,
            "extra entry"
        );
    }

    #[test]
    fn rollback_restores_the_recorded_target_and_selection_only_when_owned() {
        let f = fixture();
        prepare(&f.paths, &plan(&f.targets[0])).unwrap();
        record_target(&f.paths, &f.targets[1]).unwrap();
        fs::remove_file(&f.paths.link).unwrap();
        std::os::unix::fs::symlink(&f.targets[2], &f.paths.link).unwrap();
        fs::write(&f.paths.selection, "other home").unwrap();
        // The link names neither recorded generation: nothing changes.
        let Err(RollbackError::Failed(lines)) = rollback(&f.paths) else {
            panic!("unowned rollback succeeded")
        };
        assert_eq!(lines.len(), 3);
        assert_eq!(
            fs::read_link(&f.paths.link).unwrap().to_str(),
            Some(f.targets[2].as_str())
        );
        assert_eq!(fs::read(&f.paths.selection).unwrap(), b"other home");
        assert!(f.paths.journal.is_dir());
        // The attempted target is ours: the previous link and selection return.
        fs::remove_file(&f.paths.link).unwrap();
        std::os::unix::fs::symlink(&f.targets[1], &f.paths.link).unwrap();
        assert_eq!(
            rollback(&f.paths).ok(),
            Some("prior binary and guest image selection were restored")
        );
        assert_eq!(
            fs::read_link(&f.paths.link).unwrap().to_str(),
            Some(f.targets[0].as_str())
        );
        assert_eq!(fs::read(&f.paths.selection).unwrap(), b"previous");
        assert!(files::absent(&f.paths.journal));
        cleanup_retired(&f.paths).ok().unwrap();
        cleanup_deferred(&f.paths).unwrap();
        assert!(fs::read_dir(&f.paths.cache).unwrap().all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".hamn-update")
        }));
    }

    #[test]
    fn retired_and_deferred_cleanup_refuse_unknown_names_and_entries() {
        let f = fixture();
        let deferred = f.paths.cache.join(".hamn-update-cleanup.completed.Abc123");
        fs::DirBuilder::new().mode(0o700).create(&deferred).unwrap();
        fs::write(deferred.join("foreign"), "keep").unwrap();
        assert!(cleanup_deferred(&f.paths).is_err());
        assert_eq!(fs::read(deferred.join("foreign")).unwrap(), b"keep");
        fs::remove_dir_all(&deferred).unwrap();
        let unknown = f.paths.cache.join(".hamn-update-cleanup.other.Abc123");
        fs::DirBuilder::new().mode(0o700).create(&unknown).unwrap();
        assert!(cleanup_deferred(&f.paths).is_err());
        fs::remove_dir(&unknown).unwrap();
        let legacy = f.paths.cache.join(".hamn-update-completed.Abc123");
        fs::DirBuilder::new().mode(0o700).create(&legacy).unwrap();
        fs::write(
            legacy.join("state"),
            "version=1\nbootstrap=0\nselection=present\n",
        )
        .unwrap();
        fs::set_permissions(legacy.join("state"), fs::Permissions::from_mode(0o600)).unwrap();
        let Err(CleanupError {
            legacy: Some((1, path)),
        }) = cleanup_retired(&f.paths)
        else {
            panic!()
        };
        assert_eq!(path, legacy);
        assert!(
            legacy.join("state").is_file(),
            "a legacy journal was changed"
        );
    }
}
