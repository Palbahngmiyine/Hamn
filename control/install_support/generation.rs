//! Managed host generations: the native host installer.
//!
//! Layout. `DATADIR/.hamn-generations/<sha256>-<suffix>/` is one immutable
//! generation: `bin/hamn` (0755, the SHA-256 in the name), for a release also
//! `share/hamn/update-manifest-url` (0644, the release's manifest pointer),
//! `.hamn-previous-target` (0600, the link target it replaced) and, last,
//! the ownership marker `.hamn-generation` (0600):
//! `version=2`, `binary_sha256=`, `bindir_id=` and `datadir_id=` (SHA-256 of
//! each canonical root path and a NUL). `BINDIR/hamn` is a symbolic link to
//! the active generation's `bin/hamn`; `DATADIR/.hamn-managed` (`version=1`)
//! marks the data root.
//!
//! Earlier layouts are refused, never adopted: pre-generation installs (a
//! standalone `hamn`, `.hamn-binary.sha256`, an empty data marker) and
//! generations of the version 1 marker (Hamn 0.1.2 and earlier, which carried
//! `share/hamn/src` scripts). The refusal names what to move aside.
//!
//! Transaction. The caller holds the transaction locks; `install` holds both
//! install locks throughout. The generation is staged beside its final name,
//! flushed, marked last and renamed into place; with an update journal its
//! target is then recorded durably; only then is the link replaced by one
//! atomic rename. A failure (or SIGKILL) before that rename leaves the
//! previous link; a complete unpublished generation may remain until
//! collection. Nothing is collected here; callers run `retention::collect`.
use super::{
    Result, files, interrupt,
    journal::{self, Loaded},
    locks::{self, Roots},
    require,
};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// Where a release's manifest pointer lives in an archive root and in a
/// generation.
pub(super) const POINTER: &str = "share/hamn/update-manifest-url";
const MARKER: &str = ".hamn-generation";
/// The generation layout this Hamn writes and reads.
const LAYOUT: &str = "2";
const INSTALL_URL: &str = "https://github.com/Palbahngmiyine/Hamn#install";

/// The files a generation is made from.
pub(super) struct Payload {
    pub(super) binary: PathBuf,
    /// The release manifest pointer; source builds have none.
    pub(super) pointer: Option<PathBuf>,
}

/// An update journal that must record the attempted target.
pub(super) struct Journaled<'a> {
    pub(super) paths: &'a journal::Paths,
    pub(super) attempt: &'a str,
}

pub(super) struct Installed {
    pub(super) link: PathBuf,
    pub(super) target: String,
}

/// The ownership marker text of a generation of `binary_sha256` in `roots`.
pub(super) fn marker_text(binary_sha256: &str, roots: &Roots) -> Result<String> {
    Ok(format!(
        "version={LAYOUT}\nbinary_sha256={binary_sha256}\nbindir_id={}\ndatadir_id={}\n",
        files::path_hash(files::utf8(&roots.bindir)?),
        files::path_hash(files::utf8(&roots.datadir)?)
    ))
}

/// The `version=` of an owned 0600 marker, if any.
fn marker_version(generation: &Path) -> Option<String> {
    let marker = generation.join(MARKER);
    files::owned(&marker, false, Some(0o600)).ok()?;
    let text = files::text(&marker).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("version="))
        .map(str::to_owned)
}

fn marker_valid(generation: &Path, expected_hash: &str, roots: &Roots) -> bool {
    let marker = generation.join(MARKER);
    if files::owned(&marker, false, Some(0o600)).is_err() {
        return false;
    }
    let Ok(text) = files::text(&marker) else {
        return false;
    };
    let mut values: [Option<&str>; 4] = [None; 4];
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        let index = match key {
            "version" => 0,
            "binary_sha256" => 1,
            "bindir_id" => 2,
            "datadir_id" => 3,
            _ => return false,
        };
        if values[index].replace(value).is_some() {
            return false;
        }
    }
    let (Ok(bin), Ok(data)) = (files::utf8(&roots.bindir), files::utf8(&roots.datadir)) else {
        return false;
    };
    values
        == [
            Some(LAYOUT),
            Some(expected_hash),
            Some(files::path_hash(bin).as_str()),
            Some(files::path_hash(data).as_str()),
        ]
}

/// A complete generation of `expected_hash` owned by these roots.
fn generation_valid(generation: &Path, expected_hash: &str, roots: &Roots) -> bool {
    let binary = generation.join("bin/hamn");
    files::owned(generation, true, Some(0o755)).is_ok()
        && files::owned(&generation.join("bin"), true, None).is_ok()
        && files::owned(&binary, false, Some(0o755)).is_ok()
        && files::digest(&binary).is_ok_and(|digest| digest == expected_hash)
        && marker_valid(generation, expected_hash, roots)
}

/// How the command link relates to the managed generations.
enum Link {
    Absent,
    Managed {
        target: String,
        identity: (u64, u64, u32, u32, u64),
    },
}

/// The generation directory `root/<name>` named by `target`, if `target` is
/// `root/<name>/bin/hamn` with a valid generation name.
fn named_generation(target: &str, root: &Path) -> Option<(PathBuf, String)> {
    let relative = target.strip_prefix(&format!("{}/", root.to_str()?))?;
    let name = relative.strip_suffix("/bin/hamn")?;
    journal::generation_name(name).then(|| (root.join(name), name[..64].to_owned()))
}

/// A link into a generation of the earlier (version 1 marker) layout.
fn previous_layout(target: &str, roots: &Roots) -> bool {
    named_generation(target, &roots.datadir.join(".hamn-generations"))
        .is_some_and(|(generation, _)| marker_version(&generation).as_deref() == Some("1"))
}

/// The refusal for a command link into an earlier-layout generation.
pub(super) fn previous_layout_message(link: &Path, datadir: &Path) -> String {
    format!(
        "{} points to a Hamn generation of an earlier installation layout (Hamn 0.1.2 or earlier, or a pre-release build), \
         which this Hamn cannot upgrade in place; move {} and {} aside, then reinstall with install.sh: {INSTALL_URL}",
        link.display(),
        link.display(),
        datadir.display()
    )
}

/// Whether the active link of `roots` names an earlier-layout generation.
pub(super) fn active_is_previous_layout(roots: &Roots) -> bool {
    fs::read_link(roots.bindir.join("hamn"))
        .ok()
        .and_then(|target| target.to_str().map(|t| previous_layout(t, roots)))
        .unwrap_or(false)
}

fn managed_link_valid(link: &Path, roots: &Roots) -> Option<String> {
    let target = fs::read_link(link).ok()?;
    let target = target.to_str()?.to_owned();
    let (generation, hash) = named_generation(&target, &roots.datadir.join(".hamn-generations"))?;
    generation_valid(&generation, &hash, roots).then_some(target)
}

fn link_identity(link: &Path) -> Result<(u64, u64, u32, u32, u64)> {
    let m = fs::symlink_metadata(link)?;
    Ok((m.dev(), m.ino(), m.uid(), m.mode() & 0o7777, m.nlink()))
}

fn data_marker_valid(marker: &Path) -> bool {
    fs::symlink_metadata(marker).is_ok_and(|m| {
        m.is_file()
            && m.uid() == files::uid()
            && m.nlink() == 1
            && matches!(m.mode() & 0o7777, 0o600 | 0o644)
    }) && files::line(marker, 4096).is_ok_and(|text| text == "version=1")
}

/// Parent directories of the roots: owned, 0700 or 0755.
fn safe_parent(path: &Path) -> bool {
    files::owned(path, true, None).is_ok_and(|m| matches!(m.mode() & 0o7777, 0o700 | 0o755))
}

fn directory(path: &Path, mode: u32) -> Result<()> {
    fs::DirBuilder::new().mode(mode).create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn path_contains(container: &Path, candidate: &Path) -> bool {
    container == Path::new("/") || candidate.starts_with(container)
}

/// Neither canonical root may contain the other.
pub(super) fn refuse_overlap(roots: &Roots) -> Result<()> {
    if path_contains(&roots.bindir, &roots.datadir) || path_contains(&roots.datadir, &roots.bindir)
    {
        return Err(format!(
            "refusing overlapping binary and data directories: {} and {}",
            roots.bindir.display(),
            roots.datadir.display()
        )
        .into());
    }
    Ok(())
}

/// Checks the data root; returns whether it is already managed.
fn data_state(roots: &Roots) -> Result<bool> {
    let (datadir, marker) = (&roots.datadir, roots.datadir.join(".hamn-managed"));
    let shown = datadir.display();
    let state = if files::absent(datadir) {
        return Ok(false);
    } else if !fs::symlink_metadata(datadir)?.is_dir() {
        return Err(format!("refusing non-directory data path: {shown}").into());
    } else if !files::absent(&marker) {
        if !data_marker_valid(&marker) {
            if fs::symlink_metadata(&marker).is_ok_and(|m| m.is_file() && m.len() == 0) {
                return Err(format!(
                    "{shown} is a pre-release Hamn install (empty data marker), which this installer no longer migrates; move {shown} aside and install again"
                )
                .into());
            }
            return Err(format!(
                "refusing invalid data management marker: {}",
                marker.display()
            )
            .into());
        }
        true
    } else if fs::read_dir(datadir)?.next().is_none() {
        false
    } else {
        return Err(format!("refusing to modify unmanaged data directory: {shown}").into());
    };
    require(
        files::owned(datadir, true, Some(0o755)).is_ok(),
        &format!("refusing writable or foreign data directory: {shown}"),
    )?;
    Ok(state)
}

/// The data marker, created atomically through a private stage.
fn create_data_marker(roots: &Roots) -> Result<()> {
    let stage = files::temp_directory(
        &roots.data_parent,
        &format!(".{}.hamn-marker.", roots.data_base),
    )?;
    let result = (|| -> Result<()> {
        require(
            files::safe_private_directory(&stage),
            "unsafe data marker stage",
        )?;
        files::create(&stage.join("marker"), b"version=1\n", 0o600)?;
        files::sync();
        files::rename_exclusive(&stage.join("marker"), &roots.datadir.join(".hamn-managed"))?;
        require(
            data_marker_valid(&roots.datadir.join(".hamn-managed")),
            "data marker was not created",
        )
    })();
    let _ = fs::remove_file(stage.join("marker"));
    let _ = fs::remove_dir(&stage);
    files::sync();
    result
}

/// Installs `payload` as a new generation and points `BINDIR/hamn` at it
/// (see the module documentation). `transaction` must cover `roots`.
pub(super) fn install(
    payload: &Payload,
    transaction: &locks::Transaction,
    journaled: Option<&Journaled>,
) -> Result<Installed> {
    let roots = transaction.roots();
    interrupt::fault("host-install")?;
    let source = &payload.binary;
    require(
        fs::symlink_metadata(source).is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0),
        &format!(
            "install source is not a regular executable: {}",
            source.display()
        ),
    )?;
    if let Some(pointer) = &payload.pointer {
        require(
            fs::symlink_metadata(pointer).is_ok_and(|m| m.is_file()),
            &format!(
                "release manifest pointer is not a regular file: {}",
                pointer.display()
            ),
        )?;
    }
    refuse_overlap(roots)?;
    require(
        safe_parent(&roots.bindir) && safe_parent(&roots.data_parent),
        "refusing writable or foreign install parent directory",
    )?;
    let _install_locks = locks::Install::acquire(transaction)?;

    let link = roots.bindir.join("hamn");
    let managed = data_state(roots)?;
    let legacy_marker = roots.bindir.join(".hamn-binary.sha256");
    if !files::absent(&legacy_marker) {
        return Err(format!(
            "{} marks a pre-release Hamn install, which this installer no longer migrates; move it and {} aside and install again",
            legacy_marker.display(),
            link.display()
        )
        .into());
    }
    let current = if files::absent(&link) {
        Link::Absent
    } else if fs::symlink_metadata(&link)?.file_type().is_symlink() {
        let target = fs::read_link(&link)?.to_str().map(str::to_owned);
        if target
            .as_deref()
            .is_some_and(|target| previous_layout(target, roots))
        {
            return Err(previous_layout_message(&link, &roots.datadir).into());
        }
        let Some(target) = managed_link_valid(&link, roots) else {
            return Err(format!("refusing foreign hamn symlink: {}", link.display()).into());
        };
        require(managed, "managed hamn symlink has no data ownership marker")?;
        Link::Managed {
            identity: link_identity(&link)?,
            target,
        }
    } else {
        // An older standalone Hamn or another program: never replaced or run.
        return Err(format!(
            "refusing to replace {}: it is not a managed Hamn generation link (an older standalone Hamn or another program); move it aside and install again",
            link.display()
        )
        .into());
    };
    let previous = match &current {
        Link::Absent => None,
        Link::Managed { target, .. } => Some(target.clone()),
    };
    let handoff = |when: &str| -> Result<()> {
        let Some(journaled) = journaled else {
            return Ok(());
        };
        let valid = match journal::load(&journaled.paths.journal, &roots.datadir) {
            Loaded::Valid(journal) => {
                journal.attempt == journaled.attempt
                    && journal.host_mutation
                    && journal.new_target.is_none()
                    && journal.bootstrap == previous.is_none()
                    && (journal.bootstrap || journal.old_target == previous)
            }
            _ => false,
        };
        require(valid, when)
    };
    handoff("unsafe update journal handoff")?;

    if !managed {
        if files::absent(&roots.datadir) {
            directory(&roots.datadir, 0o755)?;
        }
        create_data_marker(roots)?;
    }
    require(
        files::owned(&roots.datadir, true, Some(0o755)).is_ok()
            && data_marker_valid(&roots.datadir.join(".hamn-managed")),
        "data directory ownership changed during install",
    )?;
    let generations = roots.datadir.join(".hamn-generations");
    if files::absent(&generations) {
        directory(&generations, 0o755)?;
    }
    require(
        files::owned(&generations, true, Some(0o755)).is_ok(),
        &format!("refusing unsafe generation root: {}", generations.display()),
    )?;
    interrupt::barrier("STAGING")?;

    let hash = files::digest(source)?;
    let stage = files::temp_directory(&generations, ".staging.")?;
    let generation = generations.join(format!("{hash}-{}", files::temp_suffix(&stage)?));
    let staged = (|| -> Result<()> {
        require(
            files::absent(&generation),
            &format!(
                "generated install identity already exists: {}",
                generation.display()
            ),
        )?;
        directory(&stage.join("bin"), 0o755)?;
        files::copy_new(source, &stage.join("bin/hamn"), 0o755)?;
        require(
            files::digest(&stage.join("bin/hamn"))? == hash,
            "staged binary differs from install source",
        )?;
        if let Some(pointer) = &payload.pointer {
            directory(&stage.join("share"), 0o755)?;
            directory(&stage.join("share/hamn"), 0o755)?;
            files::copy_new(pointer, &stage.join(POINTER), 0o644)?;
            require(
                fs::read(stage.join(POINTER))? == fs::read(pointer)?,
                "staged manifest pointer differs",
            )?;
        }
        fs::set_permissions(&stage, fs::Permissions::from_mode(0o755))?;
        if let Some(previous) = &previous {
            files::create(
                &stage.join(".hamn-previous-target"),
                format!("{previous}\n").as_bytes(),
                0o600,
            )?;
        }
        // Marker last: only a complete, verified generation is publishable.
        files::sync();
        files::create(
            &stage.join(MARKER),
            marker_text(&hash, roots)?.as_bytes(),
            0o600,
        )?;
        files::sync();
        require(
            generation_valid(&stage, &hash, roots),
            "staged generation validation failed",
        )?;
        files::rename_exclusive(&stage, &generation)?;
        Ok(())
    })();
    if let Err(error) = staged {
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }
    files::sync();
    require(
        generation_valid(&generation, &hash, roots),
        "published generation validation failed",
    )?;
    let target = format!("{}/bin/hamn", files::utf8(&generation)?);

    if let Some(journaled) = journaled {
        handoff("update journal changed before generation publication")?;
        // Recording after link publication would leave an unowned rollback
        // window on SIGKILL. Persist the exact target before exposing it.
        journal::record_target(journaled.paths, &target)?;
    }
    interrupt::barrier("BEFORE_LINK_PUBLICATION")?;

    let link_stage = files::temp_directory(&roots.bindir, ".hamn-link.")?;
    let staged_link = link_stage.join("link");
    let published = (|| -> Result<()> {
        require(
            files::safe_private_directory(&link_stage),
            "unsafe link stage",
        )?;
        std::os::unix::fs::symlink(&target, &staged_link)?;
        require(
            fs::read_link(&staged_link)? == Path::new(&target),
            "staged link differs",
        )?;
        match &current {
            Link::Absent => {
                require(files::absent(&link), "hamn path changed before commit")?;
                files::rename_exclusive(&staged_link, &link)
                    .map_err(|_| "hamn path changed before commit")?;
            }
            Link::Managed {
                target: original,
                identity,
            } => {
                require(
                    managed_link_valid(&link, roots).as_ref() == Some(original)
                        && link_identity(&link).ok() == Some(*identity),
                    "managed hamn path changed before commit",
                )?;
                fs::rename(&staged_link, &link)?;
            }
        }
        Ok(())
    })();
    let _ = fs::remove_file(&staged_link);
    let _ = fs::remove_dir(&link_stage);
    published?;
    interrupt::barrier("AFTER_LINK_PUBLICATION")?;
    files::sync();
    require(
        fs::read_link(&link).is_ok_and(|t| t == Path::new(&target))
            && managed_link_valid(&link, roots).is_some(),
        "committed generation validation failed",
    )?;
    Ok(Installed { link, target })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_support::test_support::Temp;

    fn setup(t: &Temp) -> (Roots, PathBuf) {
        let roots = Roots::prepare(&t.0.join("bin"), &t.0.join("share/hamn/src")).unwrap();
        let source = t.0.join("hamn");
        fs::write(&source, "#!/bin/sh\necho hamn 1.2.3\n").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
        (roots, source)
    }

    #[test]
    fn install_publishes_a_marked_generation_and_keeps_its_predecessor() {
        let t = Temp::new();
        let (roots, source) = setup(&t);
        let transaction = locks::Transaction::acquire(&roots).unwrap();
        let payload = Payload {
            binary: source.clone(),
            pointer: None,
        };
        let first = install(&payload, &transaction, None).unwrap();
        let generation = Path::new(&first.target)
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_path_buf();
        assert_eq!(fs::read(&first.target).unwrap(), fs::read(&source).unwrap());
        assert_eq!(
            fs::read_to_string(generation.join(MARKER)).unwrap(),
            marker_text(&files::digest(&source).unwrap(), &roots).unwrap()
        );
        assert!(
            !generation.join("share").exists(),
            "a source build has no manifest pointer"
        );
        assert!(
            !generation.join(".hamn-previous-target").exists(),
            "a first install has no predecessor"
        );
        let second = install(&payload, &transaction, None).unwrap();
        assert!(
            Path::new(&first.target).is_file(),
            "the previous generation was removed"
        );
        let recorded = generation_of(&second.target).join(".hamn-previous-target");
        assert_eq!(
            fs::read_to_string(recorded).unwrap(),
            format!("{}\n", first.target)
        );
    }

    fn generation_of(target: &str) -> PathBuf {
        Path::new(target)
            .parent()
            .and_then(Path::parent)
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn earlier_layout_and_foreign_links_are_refused_without_changes() {
        let t = Temp::new();
        let (roots, source) = setup(&t);
        let transaction = locks::Transaction::acquire(&roots).unwrap();
        let payload = Payload {
            binary: source,
            pointer: None,
        };
        let installed = install(&payload, &transaction, None).unwrap();
        let marker = generation_of(&installed.target).join(MARKER);
        let saved = fs::read_to_string(&marker).unwrap();
        fs::write(&marker, saved.replacen("version=2", "version=1", 1)).unwrap();
        let error = install(&payload, &transaction, None)
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains("earlier installation layout")
                && error.contains("reinstall with install.sh"),
            "{error}"
        );
        assert_eq!(
            fs::read_link(&installed.link).unwrap(),
            Path::new(&installed.target)
        );
        fs::write(&marker, saved.replacen("version=2", "version=3", 1)).unwrap();
        let error = install(&payload, &transaction, None)
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.starts_with("refusing foreign hamn symlink"),
            "{error}"
        );
        fs::write(&marker, &saved).unwrap();
        fs::write(&installed.target, "tampered").unwrap();
        assert!(
            install(&payload, &transaction, None).is_err(),
            "a changed binary was trusted"
        );
    }
}
