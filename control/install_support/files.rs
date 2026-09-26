//! Filesystem primitives shared by the installer and updater: ownership
//! checks against the effective user, digests, private temporary names
//! (`mkdtemp`/`mkstemp` semantics), exclusive renames and bounded child runs.
//! Nothing here holds locks or decides transaction order.
use super::{Result, require};
use sha2::{Digest, Sha256};
use std::{
    ffi::{CString, OsString},
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub(super) fn uid() -> u32 {
    // geteuid has no pointer or lifetime obligations.
    unsafe { libc::geteuid() }
}

/// An entry owned by the effective user that is not a symbolic link: a
/// directory, or a regular file with one link; `mode` (permission bits)
/// must match exactly when given.
pub(super) fn owned(path: &Path, directory: bool, mode: Option<u32>) -> Result<Metadata> {
    let m = fs::symlink_metadata(path)?;
    require(
        m.uid() == uid()
            && if directory {
                m.is_dir()
            } else {
                m.is_file() && m.nlink() == 1
            },
        "unsafe ownership",
    )?;
    if let Some(mode) = mode {
        require(m.mode() & 0o7777 == mode, "unsafe mode")?;
    }
    Ok(m)
}

/// The former shell `safe_directory`: an owned 0755 directory.
pub(super) fn safe_directory(path: &Path) -> bool {
    owned(path, true, Some(0o755)).is_ok()
}

/// An owned 0700 directory.
pub(super) fn safe_private_directory(path: &Path) -> bool {
    owned(path, true, Some(0o700)).is_ok()
}

/// An owned regular file with one link (any mode).
pub(super) fn safe_regular(path: &Path) -> bool {
    owned(path, false, None).is_ok()
}

/// An owned 0600 regular file with one link.
pub(super) fn safe_private_regular(path: &Path) -> bool {
    owned(path, false, Some(0o600)).is_ok()
}

/// No entry at all, not even a dangling symbolic link. An inspection error
/// other than absence counts as present, so callers fail closed.
pub(super) fn absent(path: &Path) -> bool {
    matches!(fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

pub(super) fn digest(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
/// Identity of a canonical install root, recorded in generation markers.
pub(super) fn path_hash(path: &str) -> String {
    hash(format!("{path}\0").as_bytes())
}
pub(super) fn text(path: &Path) -> Result<String> {
    Ok(fs::read_to_string(path)?)
}
/// Up to `limit` bytes of a regular file as UTF-8, with trailing newlines
/// removed (the former shell `$(cat FILE)`).
pub(super) fn line(path: &Path, limit: u64) -> Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    require(bytes.len() as u64 <= limit, "file exceeds size limit")?;
    Ok(String::from_utf8(bytes)?.trim_end_matches('\n').to_owned())
}
pub(super) fn parent(path: &Path) -> Result<&Path> {
    path.parent().ok_or_else(|| "missing parent".into())
}
/// A path as UTF-8 text; installation paths are recorded as text.
pub(super) fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| "installation path is not UTF-8".into())
}

pub(super) fn same_file(a: &Path, b: &Path) -> Result<bool> {
    let (a, b) = (fs::metadata(a)?, fs::metadata(b)?);
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}

/// Flushes every filesystem (the former `/bin/sync` ordering points).
pub(super) fn sync() {
    // sync has no arguments and cannot fail.
    unsafe { libc::sync() };
}

fn c_path(path: &Path) -> Result<CString> {
    Ok(CString::new(path.as_os_str().as_bytes())?)
}

/// `mkdtemp PARENT/PREFIXXXXXXX`: a new 0700 directory whose last six
/// characters are random `[A-Za-z0-9]`.
pub(super) fn temp_directory(parent: &Path, prefix: &str) -> Result<PathBuf> {
    let mut template = c_path(&parent.join(format!("{prefix}XXXXXX")))?.into_bytes_with_nul();
    // mkdtemp rewrites the NUL-terminated template in place; the buffer
    // outlives the call and no pointer escapes.
    if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    template.pop();
    Ok(PathBuf::from(OsString::from_vec(template)))
}

/// `mkstemp PARENT/PREFIXXXXXXX`: a new 0600 file, open for writing.
pub(super) fn temp_file(parent: &Path, prefix: &str) -> Result<(PathBuf, File)> {
    let mut template = c_path(&parent.join(format!("{prefix}XXXXXX")))?.into_bytes_with_nul();
    // As above; mkstemp returns a new descriptor owned by the File below.
    let fd = unsafe { libc::mkstemp(template.as_mut_ptr().cast()) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let file = unsafe { <File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    template.pop();
    Ok((PathBuf::from(OsString::from_vec(template)), file))
}

/// The random suffix of a `temp_directory`/`temp_file` name.
pub(super) fn temp_suffix(path: &Path) -> Result<String> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let suffix = &name[name.len().saturating_sub(6)..];
    require(
        suffix.len() == 6 && suffix.bytes().all(|c| c.is_ascii_alphanumeric()),
        "invalid temporary name",
    )?;
    Ok(suffix.to_owned())
}

/// Atomic rename that fails instead of replacing an existing entry
/// (including a dangling symbolic link) at `to`.
pub(super) fn rename_exclusive(from: &Path, to: &Path) -> Result<()> {
    let (from, to) = (c_path(from)?, c_path(to)?);
    // Both C strings remain alive for this synchronous Darwin call.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Writes `bytes` to a new file `path` (never an existing entry) with
/// `mode`, then flushes it.
pub(super) fn create(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(mode))?;
    file.sync_all()?;
    Ok(())
}

/// Copies the bytes (not metadata or extended attributes) of the regular
/// file `source` into the new file `destination` with `mode`.
pub(super) fn copy_new(source: &Path, destination: &Path, mode: u32) -> Result<()> {
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source)?;
    require(
        input.metadata()?.is_file(),
        "copy source is not a regular file",
    )?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(mode))?;
    output.sync_all()?;
    Ok(())
}

/// Atomic metadata publication; staging is on the destination filesystem.
/// A durable complete payload precedes rename. No partial public file is exposed.
pub(super) fn publish(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = parent(path)?;
    let mut temporary = None;
    for suffix in 0..1000 {
        let candidate = parent.join(format!(".hamn-metadata.{}.{suffix}", std::process::id()));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let (temporary, mut file) = temporary.ok_or("cannot create metadata stage")?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        // RENAME_EXCL atomically refuses an existing file or symlink without
        // introducing a hardlink window on interruption.
        rename_exclusive(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if let Err(error) = fs::remove_file(&temporary) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error.into());
        }
    }
    result
}

/// Remembers, inside the generation of `target`, that the update journal
/// of `cache` may still name it; collection keeps it until that journal is
/// gone. Idempotent; an existing record must name the same cache.
pub(super) fn recovery_root(target: &Path, cache: &Path) -> Result<()> {
    let generation = parent(parent(target)?)?;
    let cache = fs::canonicalize(cache)?;
    let cache = cache.to_str().ok_or("recovery root is not UTF-8")?;
    let path: PathBuf = generation.join(format!(".hamn-recovery-root-{}", hash(cache.as_bytes())));
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            owned(&path, false, Some(0o600))?;
            require(text(&path)? == cache, "unsafe generation recovery root")
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => publish(&path, cache.as_bytes()),
        Err(e) => Err(e.into()),
    }
}

/// Runs `command` (in its own process group, standard input from
/// /dev/null) and returns its exit status and standard output. A command
/// that has not exited within `timeout` is killed with its group and fails.
pub(super) fn bounded_output(command: &mut Command, timeout: Duration) -> Result<(bool, Vec<u8>)> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .process_group(0)
        .spawn()?;
    let mut stdout = child.stdout.take().ok_or("missing child output")?;
    let reader = std::thread::spawn(move || {
        let mut data = Vec::new();
        stdout.read_to_end(&mut data).map(|_| data)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            // The leader is unreaped, so its PID still names this group.
            unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = reader.join().map_err(|_| "child output reader failed")??;
    let status = status.ok_or("command timed out")?;
    Ok((status.success(), output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_support::test_support::Temp;
    #[test]
    fn metadata_publication_preserves_existing_file_and_cleans_failed_stage() {
        let t = Temp::new();
        let file = t.0.join("metadata");
        publish(&file, b"complete").unwrap();
        assert!(publish(&file, b"replacement").is_err());
        assert_eq!(fs::read(&file).unwrap(), b"complete");
        assert_eq!(fs::read_dir(&t.0).unwrap().count(), 1);
        let alias = t.0.join("alias");
        std::os::unix::fs::symlink(&file, &alias).unwrap();
        assert!(publish(&alias, b"bad").is_err());
        assert_eq!(fs::read(&file).unwrap(), b"complete");
    }
    #[test]
    fn temporary_names_are_private_unique_and_carry_a_six_character_suffix() {
        let t = Temp::new();
        let first = temp_directory(&t.0, ".staging.").unwrap();
        let second = temp_directory(&t.0, ".staging.").unwrap();
        assert_ne!(first, second);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(temp_suffix(&first).unwrap().len(), 6);
        let (file, _) = temp_file(&t.0, ".hamn-target.").unwrap();
        assert!(safe_private_regular(&file));
        assert!(temp_suffix(Path::new("/x/short")).is_err());
    }
    #[test]
    fn exclusive_rename_never_replaces_an_entry_or_dangling_link() {
        let t = Temp::new();
        let (from, to) = (t.0.join("from"), t.0.join("to"));
        fs::write(&from, "new").unwrap();
        std::os::unix::fs::symlink(t.0.join("missing"), &to).unwrap();
        assert!(rename_exclusive(&from, &to).is_err());
        assert_eq!(fs::read(&from).unwrap(), b"new");
        fs::remove_file(&to).unwrap();
        rename_exclusive(&from, &to).unwrap();
        assert!(absent(&from) && fs::read(&to).unwrap() == b"new");
    }
    #[test]
    fn bounded_output_kills_a_command_past_its_deadline() {
        let started = Instant::now();
        let result = bounded_output(
            Command::new("/bin/sh").args(["-c", "sleep 30"]),
            Duration::from_millis(100),
        );
        assert!(result.is_err() && started.elapsed() < Duration::from_secs(5));
        let (ok, output) = bounded_output(
            Command::new("/bin/sh").args(["-c", "echo hamn 1.2.3; exit 3"]),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(!ok && output == b"hamn 1.2.3\n");
    }
}
