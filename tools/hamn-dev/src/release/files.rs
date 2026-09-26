//! File digests, safe input checks and create-only output for release
//! artifacts and evidence.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Lowercase hex SHA-256 of a file's bytes, read as a stream.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => hasher.update(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The path, if it names a regular file (not a link) that the effective
/// user owns and that has exactly one link: an input no other user or path
/// can change behind the validator's back.
pub fn owned_regular(path: &Path) -> Result<&Path, String> {
    let unsafe_input = || format!("unsafe physical validation input: {}", path.display());
    let info = fs::symlink_metadata(path).map_err(|_| unsafe_input())?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let user = unsafe { libc::geteuid() };
    if !info.file_type().is_file() || info.uid() != user || info.nlink() != 1 {
        return Err(unsafe_input());
    }
    Ok(path)
}

/// Reads a JSON evidence or metadata file that is a regular file, not a
/// link, and at most 2 MiB.
pub fn read_json_limited(path: &Path) -> Result<Value, String> {
    let link = fs::symlink_metadata(path).map(|info| info.file_type().is_symlink()).unwrap_or(false);
    let info = fs::metadata(path).ok().filter(|info| info.is_file());
    if link || info.is_none_or(|info| info.len() > 2 * 1024 * 1024) {
        return Err(format!("unsafe or excessive evidence file: {}", path.display()));
    }
    let data = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_slice(&data).map_err(|error| format!("{}: invalid JSON: {error}", path.display()))
}

/// Reads a JSON file without size or link checks, for inputs the caller has
/// already bound by digest or produced itself.
pub fn read_json(path: &Path) -> Result<Value, String> {
    let data = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_slice(&data).map_err(|error| format!("{}: invalid JSON: {error}", path.display()))
}

/// Writes `data` to a new file; an existing path is never replaced.
pub fn write_new(path: &Path, data: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    file.write_all(data).and_then(|()| file.sync_all()).map_err(|error| format!("{}: {error}", path.display()))
}

/// Creates or truncates `path` and writes `data`.
pub fn write(path: &Path, data: &[u8]) -> Result<(), String> {
    fs::write(path, data).map_err(|error| format!("{}: {error}", path.display()))
}

/// Sets exactly `mode`, whatever the umask made of it (`chmod MODE`).
pub fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| format!("{}: {error}", path.display()))
}

/// Copies the bytes of `source` to a new file `destination` with exactly
/// `mode`; no extended attributes or other metadata are copied, and an
/// existing destination is never replaced.
pub fn copy_new(source: &Path, destination: &Path, mode: u32) -> Result<(), String> {
    let mut input = File::open(source).map_err(|error| format!("{}: {error}", source.display()))?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(destination)
        .map_err(|error| format!("{}: {error}", destination.display()))?;
    io::copy(&mut input, &mut output)
        .and_then(|_| output.sync_all())
        .map_err(|error| format!("{} -> {}: {error}", source.display(), destination.display()))?;
    set_mode(destination, mode)
}

/// A private (0700) directory `PARENT/PREFIX<unique>`, removed with its
/// contents when dropped, like `mktemp -d` with an EXIT trap.
pub struct Workspace {
    path: Option<PathBuf>,
}

impl Workspace {
    pub fn create(parent: &Path, prefix: &str) -> Result<Self, String> {
        for attempt in 0..1000u32 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.subsec_nanos());
            let path = parent.join(format!("{prefix}{}-{nanos:08x}{attempt:03x}", std::process::id()));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self { path: Some(path) }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("{}: {error}", path.display())),
            }
        }
        Err(format!("cannot create a unique workspace in {}", parent.display()))
    }

    pub fn path(&self) -> &Path {
        self.path.as_deref().expect("a workspace has a path until it is removed")
    }

    /// Removes the workspace now, reporting a failure that a drop would
    /// have to ignore.
    pub fn remove(mut self) -> Result<(), String> {
        let path = self.path.take().expect("a workspace is removed once");
        fs::remove_dir_all(&path).map_err(|error| format!("{}: {error}", path.display()))
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

/// The canonical form of release JSON: object keys sorted at every level,
/// no insignificant whitespace, one trailing newline. Hashes of these
/// files are stable for equal content.
pub fn canonical_json(value: &Value) -> String {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                Value::Object(keys.into_iter().map(|key| (key.clone(), sorted(&map[key]))).collect())
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    let mut text = serde_json::to_string(&sorted(value)).expect("JSON values serialize");
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::tmp::TempDir;
    use serde_json::json;

    #[test]
    fn digests_and_canonical_json_are_stable() {
        let directory = TempDir::new("hamn-release-files-");
        let path = directory.path().join("data");
        fs::write(&path, b"abc").unwrap();
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256_file(&path).unwrap(), expected);
        assert_eq!(
            canonical_json(&json!({"b": [{"d": 1, "c": true}], "a": "x"})),
            "{\"a\":\"x\",\"b\":[{\"c\":true,\"d\":1}]}\n"
        );
    }

    #[test]
    fn inputs_must_be_owned_single_link_regular_files() {
        let directory = TempDir::new("hamn-release-files-");
        let file = directory.path().join("file");
        fs::write(&file, b"x").unwrap();
        assert!(owned_regular(&file).is_ok());
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(owned_regular(&link).is_err());
        assert!(owned_regular(directory.path()).is_err());
        assert!(owned_regular(&directory.path().join("missing")).is_err());
        fs::hard_link(&file, directory.path().join("second-name")).unwrap();
        assert!(owned_regular(&file).is_err());
    }

    #[test]
    fn copies_are_new_files_with_exact_modes() {
        let directory = TempDir::new("hamn-release-files-");
        let source = directory.path().join("source");
        fs::write(&source, b"bytes").unwrap();
        let copy = directory.path().join("copy");
        copy_new(&source, &copy, 0o755).unwrap();
        assert_eq!(fs::read(&copy).unwrap(), b"bytes");
        assert_eq!(fs::metadata(&copy).unwrap().permissions().mode() & 0o7777, 0o755);
        fs::write(&source, b"changed").unwrap();
        assert!(copy_new(&source, &copy, 0o644).is_err(), "an existing destination is replaced");
        assert_eq!(fs::read(&copy).unwrap(), b"bytes");
        assert!(copy_new(&directory.path().join("missing"), &directory.path().join("other"), 0o644).is_err());
        assert!(!directory.path().join("other").exists());
    }

    #[test]
    fn workspaces_are_private_unique_and_always_removed() {
        let directory = TempDir::new("hamn-release-files-");
        let first = Workspace::create(directory.path(), ".work.").unwrap();
        let second = Workspace::create(directory.path(), ".work.").unwrap();
        assert_ne!(first.path(), second.path());
        assert_eq!(fs::metadata(first.path()).unwrap().permissions().mode() & 0o777, 0o700);
        fs::write(first.path().join("content"), "x").unwrap();
        let (first_path, second_path) = (first.path().to_owned(), second.path().to_owned());
        first.remove().unwrap();
        drop(second);
        assert!(!first_path.exists() && !second_path.exists());
        assert!(Workspace::create(&directory.path().join("missing"), ".work.").is_err());
    }

    #[test]
    fn evidence_reads_reject_links_directories_and_excess() {
        let directory = TempDir::new("hamn-release-files-");
        let path = directory.path().join("evidence.json");
        fs::write(&path, b"{\"ok\":true}").unwrap();
        assert_eq!(read_json_limited(&path).unwrap(), json!({"ok": true}));
        let link = directory.path().join("link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_json_limited(&link).is_err());
        assert!(read_json_limited(directory.path()).is_err());
        let large = directory.path().join("large.json");
        fs::write(&large, vec![b' '; 2 * 1024 * 1024 + 1]).unwrap();
        assert!(read_json_limited(&large).unwrap_err().contains("excessive"));
        fs::write(&path, b"{").unwrap();
        assert!(read_json_limited(&path).is_err());
        assert!(write_new(&path, b"replaced").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{");
    }
}
