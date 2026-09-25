//! Safe extraction of a candidate host archive (`.tar.gz`, or an
//! uncompressed tar) for the physical harness.
//!
//! The whole archive is validated before anything is written: only regular
//! files and directories, relative paths without `..`, no duplicate paths,
//! at most [`LIMITS`] entries and bytes. Extracted modes keep only the
//! owner/group/other read and execute bits plus owner write, as Python's
//! `tarfile` "data" filter does. The archive must hold exactly one root
//! directory, which is returned.
use super::files::owned_regular;
use flate2::read::GzDecoder;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use tar::EntryType;

pub struct Limits {
    pub entries: usize,
    pub bytes: u64,
}

pub const LIMITS: Limits = Limits { entries: 10_000, bytes: 512 * 1024 * 1024 };

/// Extracts `archive` into the empty directory `destination` and returns
/// the archive's single root directory.
pub fn unpack(archive: &Path, destination: &Path) -> Result<PathBuf, String> {
    unpack_with(archive, destination, &LIMITS)
}

pub fn unpack_with(archive: &Path, destination: &Path, limits: &Limits) -> Result<PathBuf, String> {
    owned_regular(archive)?;
    let members = validate(archive, limits)?;
    extract(archive, destination, &members)?;
    let roots: Vec<PathBuf> = fs::read_dir(destination)
        .and_then(|entries| entries.map(|entry| entry.map(|entry| entry.path())).collect())
        .map_err(|error| format!("{}: {error}", destination.display()))?;
    match roots.as_slice() {
        [root] if fs::symlink_metadata(root).is_ok_and(|info| info.is_dir()) => Ok(root.clone()),
        _ => Err("candidate must have exactly one archive root".into()),
    }
}

fn open(archive: &Path) -> Result<tar::Archive<Box<dyn Read>>, String> {
    let describe = |error: io::Error| format!("{}: {error}", archive.display());
    let mut magic = [0u8; 2];
    let mut file = File::open(archive).map_err(describe)?;
    let gzip = file.read(&mut magic).map_err(describe)? == 2 && magic == [0x1f, 0x8b];
    let file = File::open(archive).map_err(describe)?;
    let reader: Box<dyn Read> = if gzip { Box::new(GzDecoder::new(file)) } else { Box::new(file) };
    Ok(tar::Archive::new(reader))
}

/// A validated member: its relative path and whether it is a directory.
struct Member {
    path: PathBuf,
    directory: bool,
}

/// The path below the destination, or `None` for an absolute path or one
/// that leaves it.
fn relative(path: &Path) -> Option<PathBuf> {
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(relative)
}

fn validate(archive: &Path, limits: &Limits) -> Result<Vec<Member>, String> {
    let unsafe_entry = || "unsafe candidate archive entry".to_owned();
    let mut bundle = open(archive)?;
    let (mut members, mut names, mut bytes) = (Vec::new(), BTreeSet::new(), 0u64);
    for entry in bundle.entries().map_err(|error| format!("{}: {error}", archive.display()))? {
        let entry = entry.map_err(|error| format!("{}: {error}", archive.display()))?;
        let directory = match entry.header().entry_type() {
            EntryType::Regular | EntryType::Continuous => false,
            EntryType::Directory => true,
            _ => return Err(unsafe_entry()),
        };
        let path = entry.path().map_err(|_| unsafe_entry())?;
        let path = relative(&path).ok_or_else(unsafe_entry)?;
        if (path.as_os_str().is_empty() && !directory) || !names.insert(path.clone()) {
            return Err(unsafe_entry());
        }
        bytes = bytes.saturating_add(entry.header().size().map_err(|_| unsafe_entry())?);
        members.push(Member { path, directory });
        if members.len() > limits.entries || bytes > limits.bytes {
            return Err("excessive candidate archive".into());
        }
    }
    if members.is_empty() {
        return Err("excessive candidate archive".into());
    }
    Ok(members)
}

fn extract(archive: &Path, destination: &Path, members: &[Member]) -> Result<(), String> {
    let changed = || format!("{} changed during extraction", archive.display());
    let mut bundle = open(archive)?;
    let mut entries = bundle.entries().map_err(|error| format!("{}: {error}", archive.display()))?;
    for member in members {
        let mut entry =
            entries.next().ok_or_else(changed)?.map_err(|error| format!("{}: {error}", archive.display()))?;
        if entry.path().ok().and_then(|path| relative(&path)).as_ref() != Some(&member.path) {
            return Err(changed());
        }
        let target = destination.join(&member.path);
        let describe = |error: io::Error| format!("{}: {error}", target.display());
        if member.directory {
            fs::DirBuilder::new().recursive(true).mode(0o755).create(&target).map_err(describe)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::DirBuilder::new().recursive(true).mode(0o755).create(parent).map_err(describe)?;
        }
        let mode = data_filter_mode(entry.header().mode().map_err(|error| format!("{}: {error}", archive.display()))?);
        let mut output = OpenOptions::new().write(true).create_new(true).mode(mode).open(&target).map_err(describe)?;
        io::copy(&mut entry, &mut output).map_err(describe)?;
    }
    Ok(())
}

/// Clears special, group-write and other-write bits, adds owner read and
/// write, and drops group/other execute when the owner cannot execute.
fn data_filter_mode(mode: u32) -> u32 {
    let mut mode = (mode & 0o755) | 0o600;
    if mode & 0o100 == 0 {
        mode &= !0o011;
    }
    mode
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_filter_keeps_only_safe_permission_bits() {
        assert_eq!(data_filter_mode(0o4777), 0o755);
        assert_eq!(data_filter_mode(0o644), 0o644);
        assert_eq!(data_filter_mode(0o011), 0o600);
        assert_eq!(data_filter_mode(0o000), 0o600);
    }

    #[test]
    fn relative_paths_stay_below_the_destination() {
        assert_eq!(relative(Path::new("root/./bin/hamn")), Some(PathBuf::from("root/bin/hamn")));
        assert_eq!(relative(Path::new("../outside")), None);
        assert_eq!(relative(Path::new("root/../../outside")), None);
        assert_eq!(relative(Path::new("/outside")), None);
    }
}
