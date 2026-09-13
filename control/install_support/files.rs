use super::{Result, require};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    mem::MaybeUninit,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(super) fn owned(path: &Path, directory: bool, mode: Option<u32>) -> Result<Metadata> {
    let m = fs::symlink_metadata(path)?;
    // geteuid has no pointer or lifetime obligations.
    require(
        m.uid() == unsafe { libc::geteuid() }
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
pub(super) fn path_hash(path: &str) -> String {
    hash(format!("{path}\0").as_bytes())
}
pub(super) fn text(path: &Path) -> Result<String> {
    Ok(fs::read_to_string(path)?)
}
pub(super) fn parent(path: &Path) -> Result<&Path> {
    path.parent().ok_or_else(|| "missing parent".into())
}

pub(super) fn same_file(a: &Path, b: &Path) -> Result<bool> {
    let (a, b) = (fs::metadata(a)?, fs::metadata(b)?);
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}

fn fd_stat(fd: i32) -> Result<libc::stat> {
    let mut value = MaybeUninit::<libc::stat>::uninit();
    // fstat initializes this local buffer on success; fd is borrowed, never closed.
    if unsafe { libc::fstat(fd, value.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { value.assume_init() })
}

pub(super) fn fd_identity(fd: i32) -> Result<String> {
    let m = fd_stat(fd)?;
    Ok(format!(
        "{}:{}:{}:{:o}:{}",
        m.st_dev,
        m.st_ino,
        m.st_uid,
        m.st_mode & 0o7777,
        m.st_nlink
    ))
}

pub(super) fn flock(fd: i32, nonblocking: bool) -> Result<()> {
    loop {
        // flock borrows a descriptor inherited from the shell; its open-file
        // description remains owned by that shell after this helper exits.
        if unsafe {
            libc::flock(
                fd,
                libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 },
            )
        } == 0
        {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
}

pub(super) fn lock_prepare(path: &Path) -> Result<()> {
    let parent = owned(parent(path)?, true, None)?;
    require(
        matches!(parent.mode() & 0o7777, 0o700 | 0o755),
        "unsafe transaction lock parent",
    )?;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    }
    owned(path, false, Some(0o600))?;
    Ok(())
}

pub(super) fn lock_same(path: &Path, fd: i32) -> Result<()> {
    let a = owned(path, false, Some(0o600))?;
    let b = fd_stat(fd)?;
    require(
        a.dev() == b.st_dev as u64 && a.ino() == b.st_ino,
        "transaction descriptor differs",
    )
}

pub(super) fn lock_acquire(path: &Path, fd: i32) -> Result<()> {
    lock_same(path, fd)?;
    flock(fd, false)?;
    lock_same(path, fd)
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
        // introducing a hardlink window on interruption. Both C strings remain
        // alive for this synchronous Darwin call; no pointers escape.
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let from = CString::new(temporary.as_os_str().as_bytes())?;
        let to = CString::new(path.as_os_str().as_bytes())?;
        if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_support::test_support::Temp;
    use std::os::fd::AsRawFd;
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
    fn descriptor_lock_checks_inode_mode_and_link_count() {
        let t = Temp::new();
        let path = t.0.join("lock");
        lock_prepare(&path).unwrap();
        let file = OpenOptions::new().append(true).open(&path).unwrap();
        lock_acquire(&path, file.as_raw_fd()).unwrap();
        let other = t.0.join("other");
        lock_prepare(&other).unwrap();
        assert!(lock_same(&other, file.as_raw_fd()).is_err());
        fs::hard_link(&path, t.0.join("hardlink")).unwrap();
        assert!(lock_same(&path, file.as_raw_fd()).is_err());
    }
}
