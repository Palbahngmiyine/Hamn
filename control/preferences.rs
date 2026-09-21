use std::{fs::{self, OpenOptions}, io::{self, Read, Write}, path::{Path, PathBuf}};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use serde::{Deserialize, Serialize};
use std::os::fd::AsRawFd;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Workspace { Containers, Kubernetes }
impl Workspace {
    pub fn index(self) -> usize { if self == Self::Containers { 0 } else { 1 } }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Preferences {
    version: u32, default_workspace: Workspace,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] recent_targets: Vec<Target>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")] favorites: Vec<Target>,
}
/// Persist identifiers and configuration paths only, never CLI arguments,
/// endpoint credentials, tokens, or kubeconfig contents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Target {
    Hamn { name: String },
    Docker { name: String, config: Option<String> },
    Kubernetes { name: String, namespace: Option<String>, config: Option<String> },
}
impl Target {
    pub fn label(&self) -> String { match self {
        Self::Hamn { name } => format!("Hamn profile: {name}"),
        Self::Docker { name, config } => format!("Docker context: {name} | config: {}", config.as_deref().unwrap_or("CLI default")),
        Self::Kubernetes { name, namespace, config } => format!("Kubernetes: {name} / {} | kubeconfig: {}", namespace.as_deref().unwrap_or("context default"), config.as_deref().unwrap_or("CLI default")),
    } }
    fn valid(&self) -> bool {
        let fields: Vec<&str> = match self {
            Self::Hamn { name } => vec![name],
            Self::Docker { name, config } => vec![name, config.as_deref().unwrap_or("")],
            Self::Kubernetes { name, namespace, config } => vec![name, namespace.as_deref().unwrap_or(""), config.as_deref().unwrap_or("")],
        };
        !fields[0].is_empty() && fields.iter().all(|s| s.len() <= 1024 && !s.chars().any(char::is_control))
    }
}
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
/// All TUI writers use this stable, private inode; the JSON itself is renamed.
/// Contention is reported rather than blocking terminal input. Readers observe
/// an old or new complete document and need no lock. Never unlink the lock file.
struct UpdateLock { _file: fs::File }
fn update_lock(path: &Path) -> io::Result<UpdateLock> {
    let parent = path.parent().ok_or_else(invalid)?;
    match fs::DirBuilder::new().mode(0o700).create(parent) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {},
        result => result?,
    }
    directory(parent)?;
    let file = OpenOptions::new().read(true).write(true).create(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK).open(parent.join("tui.lock"))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } || metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 || metadata.len() != 0 {
        return Err(invalid());
    }
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 { break; }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted { continue; }
        if error.kind() == io::ErrorKind::WouldBlock {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "TUI preferences are busy in another instance; retry after it finishes"));
        }
        return Err(error);
    }
    Ok(UpdateLock { _file: file })
}
fn load_document(path: &Path) -> io::Result<Option<Preferences>> {
    let parent = path.parent().ok_or_else(invalid)?;
    match directory(parent) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    }
    let mut file = match OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK).open(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        result => result?,
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } ||
        metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 || metadata.len() > 65536 { return Err(invalid()); }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file).take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 { return Err(invalid()); }
    let value: Preferences = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if value.version != 1 || value.recent_targets.len() > 32 || value.favorites.len() > 32 || !value.recent_targets.iter().chain(&value.favorites).all(Target::valid) { return Err(invalid()); }
    Ok(Some(value))
}
fn save_document(path: &Path, preferences: &Preferences) -> io::Result<()> {
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
        let bytes = serde_json::to_vec(preferences)?;
        output.write_all(&bytes)?;
        output.sync_all()?;
        fs::rename(&temp, path)?;
        dir.sync_all()
    })();
    if result.is_err() { let _ = fs::remove_file(temp); }
    result
}

pub fn load(path: &Path) -> io::Result<Option<Workspace>> { Ok(load_document(path)?.map(|value| value.default_workspace)) }
pub fn save(path: &Path, workspace: Workspace) -> io::Result<()> {
    let _lock = update_lock(path)?;
    let mut value = load_document(path).ok().flatten().unwrap_or(Preferences { version: 1, default_workspace: workspace, recent_targets: Vec::new(), favorites: Vec::new() });
    value.default_workspace = workspace;
    save_document(path, &value)
}
pub fn targets(path: &Path, favorites: bool) -> io::Result<Vec<Target>> {
    Ok(load_document(path)?.map_or(Vec::new(), |value| if favorites { value.favorites } else { value.recent_targets }))
}
pub fn remember_target(path: &Path, target: Target, favorite: bool) -> io::Result<()> {
    if !target.valid() { return Err(invalid()); }
    let lock = update_lock(path)?;
    remember_target_locked(path, target, favorite, &lock)
}
fn remember_target_locked(path: &Path, target: Target, favorite: bool, _lock: &UpdateLock) -> io::Result<()> {
    let mut value = load_document(path)?.ok_or_else(invalid)?;
    let values = if favorite { &mut value.favorites } else { &mut value.recent_targets };
    let existing = values.iter().position(|saved| saved == &target);
    if let Some(index) = existing { values.remove(index); }
    if !favorite || existing.is_none() { values.insert(0, target); }
    values.truncate(32);
    let bytes = serde_json::to_vec(&value)?;
    if bytes.len() > 65536 { return Err(invalid()); }
    save_document(path, &value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn target_labels_distinguish_same_context_in_different_configuration_files() {
        let first = Target::Docker { name: "dev".into(), config: Some("/first/config".into()) };
        let second = Target::Docker { name: "dev".into(), config: Some("/second/config".into()) };
        assert_ne!(first.label(), second.label());
        let first = Target::Kubernetes { name: "dev".into(), namespace: None, config: Some("/first/kubeconfig".into()) };
        let second = Target::Kubernetes { name: "dev".into(), namespace: None, config: Some("/second/kubeconfig".into()) };
        assert_ne!(first.label(), second.label());
        assert!(first.label().contains("/first/kubeconfig"));
    }
    #[test]
    fn preference_lock_rejects_links_unsafe_permissions_and_non_files() {
        let root = std::env::temp_dir().join(format!("hamn-preference-lock-{}", std::process::id()));
        fs::create_dir(&root).unwrap(); let path = root.join(".hamn/tui.json");
        save(&path, Workspace::Containers).unwrap(); let lock = root.join(".hamn/tui.lock");
        assert_eq!(fs::metadata(&lock).unwrap().mode() & 0o777, 0o600);
        let before = fs::read(&path).unwrap();
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(save(&path, Workspace::Kubernetes).is_err());
        fs::remove_file(&lock).unwrap();
        let external = root.join("outside"); fs::write(&external, "keep").unwrap();
        symlink(&external, &lock).unwrap(); assert!(save(&path, Workspace::Kubernetes).is_err());
        assert_eq!(fs::read_to_string(&external).unwrap(), "keep");
        fs::remove_file(&lock).unwrap(); fs::create_dir(&lock).unwrap();
        assert!(save(&path, Workspace::Kubernetes).is_err()); fs::remove_dir(&lock).unwrap();
        fs::write(&lock, "").unwrap(); fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&lock, root.join("hardlink")).unwrap();
        assert!(save(&path, Workspace::Kubernetes).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "spawned by concurrent_target_writers_preserve_changes_after_contention"]
    fn target_writer_fixture() {
        let path = PathBuf::from(std::env::var_os("HAMN_PREFERENCES_TEST_PATH").unwrap());
        let first = std::env::var("HAMN_PREFERENCES_TEST_WRITER").unwrap() == "first";
        if first {
            let lock = update_lock(&path).unwrap();
            println!("WRITER_LOCKED"); io::stdout().flush().unwrap();
            io::stdin().read_exact(&mut [0u8]).unwrap();
            remember_target_locked(&path, Target::Hamn { name: "first".into() }, false, &lock).unwrap();
        } else {
            let target = Target::Hamn { name: "second".into() };
            assert_eq!(remember_target(&path, target.clone(), true).unwrap_err().kind(), io::ErrorKind::WouldBlock);
            println!("WRITER_BUSY"); io::stdout().flush().unwrap();
            io::stdin().read_exact(&mut [0u8]).unwrap();
            remember_target(&path, target, true).unwrap();
        }
        println!("WRITER_COMMITTED"); io::stdout().flush().unwrap();
    }
    #[test]
    fn concurrent_target_writers_preserve_changes_after_contention() {
        struct Writer { child: std::process::Child, lines: std::sync::mpsc::Receiver<String> }
        impl Drop for Writer {
            fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
        }
        impl Writer {
            fn start(path: &Path, which: &str) -> Self {
                use std::io::BufRead;
                let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "preferences::tests::target_writer_fixture", "--ignored", "--nocapture"])
                    .env("HAMN_PREFERENCES_TEST_PATH", path).env("HAMN_PREFERENCES_TEST_WRITER", which)
                    .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
                let stdout = child.stdout.take().unwrap(); let (send, lines) = std::sync::mpsc::channel();
                std::thread::spawn(move || { for line in io::BufReader::new(stdout).lines() { if send.send(line.unwrap()).is_err() { break; } } });
                Self { child, lines }
            }
            fn until(&self, marker: &str) {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop { let line = self.lines.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())).unwrap(); if line.contains(marker) { return; } }
            }
            fn release(&mut self) { self.child.stdin.as_mut().unwrap().write_all(b"1").unwrap(); }
        }
        let root = std::env::temp_dir().join(format!("hamn-concurrent-preferences-{}", std::process::id()));
        fs::create_dir(&root).unwrap(); let path = root.join(".hamn/tui.json");
        save(&path, Workspace::Containers).unwrap();
        let mut first = Writer::start(&path, "first"); first.until("WRITER_LOCKED");
        let mut second = Writer::start(&path, "second"); second.until("WRITER_BUSY");
        assert!(targets(&path, false).unwrap().is_empty()); assert!(targets(&path, true).unwrap().is_empty());
        first.release(); first.until("WRITER_COMMITTED"); assert!(first.child.wait().unwrap().success());
        second.release(); second.until("WRITER_COMMITTED"); assert!(second.child.wait().unwrap().success());
        assert_eq!(targets(&path, false).unwrap(), vec![Target::Hamn { name: "first".into() }]);
        assert_eq!(targets(&path, true).unwrap(), vec![Target::Hamn { name: "second".into() }]);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn targets_are_bounded_atomic_private_and_preserved_by_workspace_changes() {
        let root = std::env::temp_dir().join(format!("hamn-target-preferences-{}", std::process::id()));
        fs::create_dir(&root).unwrap(); let path = root.join(".hamn/tui.json");
        save(&path, Workspace::Containers).unwrap();
        let target = Target::Kubernetes { name: "dev".into(), namespace: Some("work".into()), config: Some("/fixture/config".into()) };
        remember_target(&path, target.clone(), false).unwrap();
        remember_target(&path, target.clone(), true).unwrap();
        save(&path, Workspace::Kubernetes).unwrap();
        assert_eq!(targets(&path, true).unwrap(), vec![target.clone()]);
        assert_eq!(targets(&path, false).unwrap(), vec![target.clone()]);
        remember_target(&path, target, true).unwrap(); assert!(targets(&path, true).unwrap().is_empty());
        for i in 0..40 { remember_target(&path, Target::Hamn { name: format!("profile-{i}") }, false).unwrap(); }
        assert_eq!(targets(&path, false).unwrap().len(), 32);
        let before = fs::read(&path).unwrap();
        assert!(remember_target(&path, Target::Hamn { name: "bad\nname".into() }, false).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert!(serde_json::from_str::<Target>(r#"{"kind":"hamn","name":"dev","token":"secret"}"#).is_err());
        fs::remove_dir_all(root).unwrap();
    }
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
