//! Private temporary directories removed when dropped.
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

pub struct TempDir(PathBuf);

impl TempDir {
    /// Creates `<parent>/<prefix><unique>` with mode 0700.
    pub fn new_in(parent: &Path, prefix: &str) -> Self {
        for attempt in 0..1000u32 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.subsec_nanos());
            let path = parent.join(format!("{prefix}{}-{nanos:08x}{attempt:03x}", std::process::id()));
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("{}: {error}", path.display()),
            }
        }
        panic!("cannot create a unique directory in {}", parent.display());
    }

    /// Creates a directory under /tmp, whose short path keeps Unix socket
    /// paths below their length limit.
    pub fn new(prefix: &str) -> Self {
        Self::new_in(Path::new("/tmp"), prefix)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
