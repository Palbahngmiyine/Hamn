use std::{fs::{self, OpenOptions}, io::{self, Read, Write}, path::{Path, PathBuf}};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Workspace { Containers, Kubernetes }
impl Workspace {
    pub fn index(self) -> usize { if self == Self::Containers { 0 } else { 1 } }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Preferences { version: u32, default_workspace: Workspace }
fn invalid() -> io::Error { io::Error::other("Invalid or unsafe ~/.hamn/tui.json; choose a default workspace to replace it") }
pub fn path() -> io::Result<PathBuf> {
    Ok(PathBuf::from(std::env::var_os("HOME").ok_or_else(invalid)?).join(".hamn/tui.json"))
}
fn directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
        return Err(invalid());
    }
    Ok(())
}
pub fn load(path: &Path) -> io::Result<Option<Workspace>> {
    let parent = path.parent().ok_or_else(invalid)?;
    match directory(parent) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    }
    let mut file = match OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } ||
        metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 || metadata.len() > 4096 { return Err(invalid()); }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file).take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 { return Err(invalid()); }
    let value: Preferences = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if value.version != 1 { return Err(invalid()); }
    Ok(Some(value.default_workspace))
}
pub fn save(path: &Path, workspace: Workspace) -> io::Result<()> {
    let parent = path.parent().ok_or_else(invalid)?;
    match fs::DirBuilder::new().mode(0o700).create(parent) {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {},
        result => result?,
    }
    directory(parent)?;
    // Opening the directory first keeps the durability check tied to this directory.
    let dir = OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW).open(parent)?;
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temp = parent.join(format!(".tui-{}-{}.tmp", std::process::id(), SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let mut output = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temp)?;
    let result = (|| {
        let bytes = serde_json::to_vec(&Preferences { version: 1, default_workspace: workspace })?;
        output.write_all(&bytes)?;
        output.sync_all()?;
        fs::rename(&temp, path)?;
        dir.sync_all()
    })();
    if result.is_err() { let _ = fs::remove_file(temp); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn preferences_roundtrip_reject_corruption_and_never_follow_links() {
        let root = std::env::temp_dir().join(format!("hamn-preferences-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join(".hamn/tui.json");
        assert_eq!(load(&path).unwrap(), None);
        for workspace in [Workspace::Containers, Workspace::Kubernetes] {
            save(&path, workspace).unwrap();
            assert_eq!(load(&path).unwrap(), Some(workspace));
            assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        }
        for text in ["", "{}", "null", "{\"version\":2,\"defaultWorkspace\":\"containers\"}",
                     "{\"version\":1,\"defaultWorkspace\":\"vm\"}", "bad", "{\"version\":1,\"version\":1}"] {
            fs::write(&path, text).unwrap(); assert!(load(&path).is_err());
        }
        save(&path, Workspace::Containers).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&path).is_err());
        let outside = root.join("outside"); fs::write(&outside, "preserved").unwrap();
        fs::remove_file(&path).unwrap(); symlink(&outside, &path).unwrap();
        assert!(load(&path).is_err());
        save(&path, Workspace::Kubernetes).unwrap();
        assert_eq!(fs::read_to_string(outside).unwrap(), "preserved");
        fs::remove_dir_all(root).unwrap();
    }
}
