//! `vm up` and `vm down` through the headless contract of the real binary,
//! with an isolated HOME: every spelling of a workspace names one profile;
//! a vm up that admission refuses, or whose start fails, leaves no profile
//! it made; an ephemeral profile is reused and kept, then stopped or purged
//! by vm down; a profile without the workspace's record is left alone; and
//! a vm up waits for another operation on its profile, and gives up when
//! cancelled meanwhile. Never starts a VM: each start is refused by
//! admission or before it; an unreachable proxy keeps a start that got
//! further offline. Expected names and records follow from the contract in
//! host/core/workspace.h, and git directories from git itself;
//! tests/host/test_workspace.c covers their edge cases.
use crate::runner::{self, case};
use crate::support::api_fixtures;
use crate::support::hamn;
use crate::support::tmp::TempDir;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "vm-up",
        "a workspace's VM is created, reused, stopped and purged by vm up and vm down alone",
        vec![
            case("a_refused_vm_up_leaves_no_profile_it_made", a_refused_vm_up_leaves_no_profile_it_made),
            case("a_new_profile_that_cannot_start_is_deleted_again", a_new_profile_that_cannot_start_is_deleted_again),
            case("a_new_profile_unconfirmed_or_unrecorded_is_deleted_again", a_new_profile_unconfirmed_or_unrecorded_is_deleted_again),
            case("the_workspace_must_be_an_existing_directory_of_this_user", the_workspace_must_be_an_existing_directory_of_this_user),
            case("an_ephemeral_profile_is_reused_kept_stopped_and_purged", an_ephemeral_profile_is_reused_kept_stopped_and_purged),
            case("a_profile_without_this_workspaces_record_is_left_alone", a_profile_without_this_workspaces_record_is_left_alone),
            case("vm_up_waits_while_another_operation_holds_the_profile", vm_up_waits_while_another_operation_holds_the_profile),
            case("a_vm_up_cancelled_while_it_waits_creates_nothing", a_vm_up_cancelled_while_it_waits_creates_nothing),
            case("a_new_profile_is_created_with_the_requested_resources", a_new_profile_is_created_with_the_requested_resources),
        ],
        filters,
    )
}

/// No VM fits in a reserve of all the memory there is.
const NO_ROOM: (&str, &str) = ("HAMN_ADMISSION_MEMORY_RESERVE_MIB", "4294967295");

/// The binary under test with a private HOME.
struct Home {
    directory: TempDir,
}

impl Home {
    fn new() -> Self {
        Self { directory: TempDir::new("hamn-up-") }
    }

    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn hamn(&self) -> PathBuf {
        self.path().join(".hamn")
    }

    /// `hamn --headless ARGUMENTS` with `environment`, offline: a start
    /// that admission lets through fails at the guest image instead of
    /// downloading one.
    fn command(&self, arguments: &[&str], environment: &[(&str, &str)]) -> Command {
        let mut command = Command::new(hamn());
        command
            .arg("--headless")
            .args(arguments)
            .env("HOME", self.path())
            .env("https_proxy", "http://127.0.0.1:9")
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .current_dir(self.path());
        for (key, value) in environment {
            command.env(key, value);
        }
        command
    }

    /// The envelope and standard error of a call; the exit status matches `ok`.
    fn call(&self, arguments: &[&str], environment: &[(&str, &str)]) -> (Value, String) {
        let result = api_fixtures::run(&mut self.command(arguments, environment), None, Duration::from_secs(60));
        let value = result.json();
        assert_eq!(result.status.code(), Some(if value["ok"] == true { 0 } else { 1 }), "{result:?}");
        (value, result.stderr)
    }

    fn run(&self, arguments: &[&str]) -> Value {
        self.call(arguments, &[]).0
    }

    fn up(&self, workspace: &Path, arguments: &[&str], environment: &[(&str, &str)]) -> (Value, String) {
        let path = workspace.to_str().unwrap();
        self.call(&[&["vm", "up", "--workspace", path, "--yes"][..], arguments].concat(), environment)
    }

    fn down(&self, workspace: &Path, arguments: &[&str]) -> (Value, String) {
        let path = workspace.to_str().unwrap();
        self.call(&[&["vm", "down", "--workspace", path, "--yes"][..], arguments].concat(), &[])
    }

    fn listed(&self) -> Vec<String> {
        let value = self.run(&["vm", "list"]);
        assert_eq!(value["ok"], true, "{value}");
        value["data"].as_array().unwrap().iter().map(|row| row["name"].as_str().unwrap().to_owned()).collect()
    }

    /// A directory of HOME.
    fn directory(&self, name: &str) -> PathBuf {
        let path = self.path().join(name);
        fs::create_dir(&path).unwrap();
        path
    }

    /// A repository HOME/main with one commit and its linked worktree
    /// HOME/<name>, both made by git; the worktree and git's own answer for
    /// its common git directory, as a real path.
    fn git_worktree(&self, name: &str) -> (PathBuf, PathBuf) {
        let git = |arguments: &[&str]| {
            let output = Command::new("git")
                .args(arguments)
                .env("HOME", self.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .current_dir(self.path())
                .output()
                .unwrap();
            assert!(output.status.success(), "git {arguments:?}: {output:?}");
            String::from_utf8(output.stdout).unwrap()
        };
        git(&["init", "-q", "main"]);
        git(&["-C", "main", "-c", "user.name=Hamn", "-c", "user.email=hamn@example.invalid", "commit", "-q", "--allow-empty", "-m", "init"]);
        git(&["-C", "main", "worktree", "add", "-q", "--detach", &format!("../{name}")]);
        let common = git(&["-C", name, "rev-parse", "--path-format=absolute", "--git-common-dir"]);
        (self.path().join(name), fs::canonicalize(common.trim()).unwrap())
    }

    /// The ephemeral profile of `workspace` as the contract describes it,
    /// made without vm up: `vm apply` of its definition, then its record.
    fn make_ephemeral(&self, workspace: &Path, git: Option<&Path>) -> PathBuf {
        let name = profile_of(workspace);
        let mut mounts = mount(&real(workspace), true);
        if let Some(git) = git {
            mounts.push_str(&mount(git, false));
        }
        let file = self.path().join(format!("{name}.yaml"));
        fs::write(
            &file,
            format!("apiVersion: hamn/v1\nkind: Profile\nmetadata:\n  name: {name}\nspec:\n  mountHome: false\n  mounts:\n{mounts}"),
        )
        .unwrap();
        let applied = self.run(&["vm", "apply", "--profile", &name, "--file", file.to_str().unwrap(), "--yes"]);
        assert_eq!(applied["data"]["action"], "create", "{applied}");
        let profile = self.hamn().join(&name);
        private_file(&profile.join("ephemeral"), &record_of(workspace));
        profile
    }

    /// Every path under HOME with its kind and size, sorted.
    fn tree(&self) -> Vec<String> {
        let mut paths = Vec::new();
        walk(self.path(), self.path(), &mut paths);
        paths.sort();
        paths
    }
}

fn walk(root: &Path, directory: &Path, paths: &mut Vec<String>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let metadata = fs::symlink_metadata(&path).unwrap();
        let relative = path.strip_prefix(root).unwrap().display().to_string();
        paths.push(format!("{relative} {:?} {}", metadata.file_type(), metadata.len()));
        if metadata.is_dir() {
            walk(root, &path, paths);
        }
    }
}

fn real(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap()
}

/// The name the contract gives a workspace's profile: ws- and the first 12
/// hexadecimal digits of the SHA-256 of its real path.
fn profile_of(workspace: &Path) -> String {
    let digest = Sha256::digest(real(workspace).as_os_str().as_bytes());
    format!("ws-{}", digest[..6].iter().map(|byte| format!("{byte:02x}")).collect::<String>())
}

/// The record of the contract: "<st_dev> <st_ino> <real path>\n".
fn record_of(workspace: &Path) -> String {
    let metadata = fs::metadata(workspace).unwrap();
    format!("{} {} {}\n", metadata.dev(), metadata.ino(), real(workspace).display())
}

/// One entry of a definition's mounts, at the directory's own path.
fn mount(path: &Path, writable: bool) -> String {
    let quoted = serde_json::to_string(path.to_str().unwrap()).unwrap();
    format!("    - location: {quoted}\n      mountPoint: {quoted}\n      writable: {writable}\n")
}

fn private_file(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn failure(value: &Value, code: &str, message: &str) {
    assert!(value["ok"] == false && value["error"]["code"] == code, "{value}");
    assert!(value["error"]["message"].as_str().is_some_and(|text| text.contains(message)), "{value}");
}

/// The names in ~/.hamn.
fn entries(directory: &Path) -> Vec<String> {
    let mut names: Vec<_> =
        fs::read_dir(directory).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

/// Admission refuses every spelling of one git worktree, and each refusal
/// names the same profile, which the call created, with the worktree and
/// git's common directory shared at their own paths, and deleted again:
/// only its lock files stay, as after a purge.
fn a_refused_vm_up_leaves_no_profile_it_made() {
    let home = Home::new();
    let (tree, common) = home.git_worktree("Tree");
    let name = profile_of(&tree);
    let link = home.path().join("link");
    std::os::unix::fs::symlink(&tree, &link).unwrap();
    // The volume of /tmp is case-insensitive, as macOS installs it.
    assert!(home.path().join("tree").is_dir());
    let shares = format!(
        "profile {name} shares the workspace {} writable and the git directory {} read-only, each at its own path",
        real(&tree).display(),
        common.display()
    );
    for spelling in [tree.clone(), home.path().join("tree"), home.path().join("TREE"), link, tree.join(".")] {
        let (value, stderr) = home.up(&spelling, &[], &[NO_ROOM]);
        failure(&value, "admissionRefused", &format!("refused to start the VM of profile {name}: "));
        assert_eq!(value["target"]["workspace"], spelling.to_str().unwrap(), "{value}");
        assert!(value["target"]["profile"].is_null(), "{value}");
        let details = &value["error"]["details"];
        assert_eq!(details["profile"], name.as_str(), "{spelling:?}: {value}");
        assert_eq!(details["request"], json!({"cpus": 4, "memoryMiB": 4096}), "{value}");
        let message = value["error"]["message"].as_str().unwrap();
        assert!(message.ends_with(&format!("; vm up deleted the profile {name} it had created")), "{message}");
        assert!(stderr.contains(&format!("profile {name}: created")) && stderr.contains(&shares), "{stderr}");
        assert!(!home.hamn().join(&name).exists(), "{spelling:?}");
        assert!(home.listed().is_empty());
    }
    assert_eq!(entries(&home.hamn()), [".locks".to_owned(), format!(".{name}-mutation.lock")]);
    assert!(tree.join(".git").is_file() && common.join("HEAD").is_file());
}

/// A start that fails after the profile was created, here because a
/// writable share must lie beneath HOME, deletes it again; the workspace,
/// which is no git repository, stays as it was.
fn a_new_profile_that_cannot_start_is_deleted_again() {
    let home = Home::new();
    let outside = TempDir::new("hamn-up-outside-");
    fs::write(outside.path().join("keep"), "keep\n").unwrap();
    let name = profile_of(outside.path());
    let (value, stderr) = home.up(outside.path(), &[], &[]);
    failure(&value, "operationFailed", "must be an owned non-symlink directory beneath HOME");
    let message = value["error"]["message"].as_str().unwrap();
    assert!(message.ends_with(&format!("; vm up deleted the profile {name} it had created")), "{message}");
    let shared = real(outside.path());
    for line in [
        format!("warning: the VM does not share a git directory: {}/.git: there is none", shared.display()),
        format!("profile {name} shares the workspace {} writable at its own path", shared.display()),
        format!("deleted profile {name} with its disk and Docker data"),
    ] {
        assert!(stderr.contains(&line), "{line:?} missing from {stderr}");
    }
    assert!(!home.hamn().join(&name).exists() && home.listed().is_empty());
    assert_eq!(entries(outside.path()), ["keep"]);
}

/// A new profile whose config.yaml was written but not confirmed durable, or
/// whose workspace record cannot be written, is deleted again. The failures
/// come from the atomic-write fault hook of host/util/fs.c, at the path the
/// C core builds from HOME.
fn a_new_profile_unconfirmed_or_unrecorded_is_deleted_again() {
    let home = Home::new();
    let tree = home.directory("tree");
    let name = profile_of(&tree);
    let unrecorded = format!("cannot record workspace {} in profile {name}: Input/output error", real(&tree).display());
    for (file, stage, reason) in [
        ("config.yaml", "fsync", format!("the settings of profile {name} were written but not confirmed durable")),
        ("ephemeral", "open", unrecorded),
    ] {
        let fault = format!("{}/.hamn/{name}/{file}", home.path().display());
        let (value, stderr) = home.up(
            &tree,
            &[],
            &[("HAMN_TEST_FAIL_ATOMIC_PARENT_PATH", &fault), ("HAMN_TEST_FAIL_ATOMIC_PARENT_STAGE", stage)],
        );
        failure(&value, "operationFailed", &reason);
        let message = value["error"]["message"].as_str().unwrap();
        assert!(message.ends_with(&format!("; vm up deleted the profile {name} it had created")), "{message}");
        assert!(stderr.contains(&format!("deleted profile {name} with its disk and Docker data")), "{stderr}");
        assert!(!home.hamn().join(&name).exists() && home.listed().is_empty(), "{file}");
    }
}

/// A missing path, a file, another user's directory and a relative path are
/// refused by vm up and vm down before anything under HOME changes.
fn the_workspace_must_be_an_existing_directory_of_this_user() {
    let home = Home::new();
    fs::write(home.path().join("file"), "file\n").unwrap();
    let unstorable = home.directory("line\nbreak");
    let before = home.tree();
    let mut cases = vec![
        (home.path().join("missing"), "does not exist"),
        (home.path().join("file"), "is not a directory"),
        (home.path().join("file/below"), "is not a directory"),
    ];
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        cases.push((PathBuf::from("/usr"), "workspace /usr is not owned by this user"));
    }
    for (path, message) in &cases {
        let (value, _) = home.up(path, &[], &[]);
        failure(&value, "invalidRequest", message);
        let (value, _) = home.down(path, &[]);
        failure(&value, "invalidRequest", message);
        let (value, _) = home.down(path, &["--purge"]);
        failure(&value, "invalidRequest", message);
    }
    let (value, _) = home.up(&unstorable, &[], &[]);
    failure(&value, "invalidRequest", "cannot be shared: a profile cannot store its path");
    let (value, _) = home.up(Path::new("relative/tree"), &[], &[]);
    failure(&value, "invalidRequest", "--workspace must be an absolute path");
    assert_eq!(home.tree(), before);
    assert!(!home.hamn().exists());
}

/// A profile with the record of this workspace is the one vm up starts
/// again, by any spelling. A refused or failed start keeps it as it was,
/// asking to share less than it shares is refused, vm down stops it, and
/// vm down --purge deletes it once, keeping the worktree and its repository.
fn an_ephemeral_profile_is_reused_kept_stopped_and_purged() {
    let home = Home::new();
    let (tree, common) = home.git_worktree("tree");
    let name = profile_of(&tree);
    let profile = home.make_ephemeral(&tree, Some(&common));
    let kept = |profile: &Path| {
        (fs::read(profile.join("config.yaml")).unwrap(), fs::read(profile.join("ephemeral")).unwrap())
    };
    let before = kept(&profile);

    for spelling in [tree.clone(), home.path().join("TREE")] {
        let (value, stderr) = home.up(&spelling, &[], &[NO_ROOM]);
        failure(&value, "admissionRefused", &format!("refused to start the VM of profile {name}: "));
        assert_eq!(value["error"]["details"]["profile"], name.as_str(), "{value}");
        assert!(!value["error"]["message"].as_str().unwrap().contains("deleted the profile"), "{value}");
        assert!(!stderr.contains("created"), "{stderr}");
        assert_eq!(kept(&profile), before);
        assert_eq!(home.listed(), [name.clone()]);
    }
    let (value, _) = home.up(&tree, &["--no-git-mount"], &[]);
    failure(&value, "conflict", &format!("profile {name} shares the git directory of {}", real(&tree).display()));
    assert_eq!(kept(&profile), before);

    let (value, _) = home.up(&tree, &["--disk", "10"], &[]);
    failure(&value, "operationFailed", "disk size cannot shrink (current: 60 GiB)");
    assert_eq!(kept(&profile), before);

    let workspace = real(&tree).to_str().unwrap().to_owned();
    let (value, _) = home.down(&tree, &[]);
    assert_eq!(value["ok"], true, "{value}");
    let data = &value["data"];
    assert!(data["profile"] == name.as_str() && data["name"] == name.as_str() && data["workspace"] == workspace.as_str(), "{value}");
    assert_eq!(data["state"], "stopped", "{value}");
    assert_eq!(kept(&profile), before);

    let (value, stderr) = home.down(&home.path().join("TREE"), &["--purge"]);
    assert_eq!(
        value["data"],
        json!({"profile": name, "workspace": workspace, "deleted": true, "purged": true, "alreadyAbsent": false})
    );
    assert!(stderr.contains(&format!("deleted profile {name} with its disk and Docker data")), "{stderr}");
    assert!(!profile.exists() && home.listed().is_empty());
    assert!(tree.join(".git").is_file() && common.join("HEAD").is_file());

    let before = home.tree();
    let (value, stderr) = home.down(&tree, &["--purge"]);
    assert_eq!(value["data"]["alreadyAbsent"], true, "{value}");
    assert!(stderr.contains(&format!("workspace {workspace} has no VM; nothing to delete")), "{stderr}");
    let (value, _) = home.down(&tree, &[]);
    failure(&value, "operationFailed", &format!("workspace {workspace} has no VM: profile {name} does not exist"));
    assert_eq!(home.tree(), before);
}

/// A profile of the workspace's name that vm up did not make, or whose
/// record names another directory, is neither started, stopped nor purged.
fn a_profile_without_this_workspaces_record_is_left_alone() {
    let home = Home::new();
    let tree = home.directory("tree");
    let name = profile_of(&tree);
    let created = home.run(&["vm", "create", "--profile", &name, "--yes"]);
    assert_eq!(created["ok"], true, "{created}");
    let record = home.hamn().join(&name).join("ephemeral");
    let metadata = fs::metadata(&tree).unwrap();
    let workspace = real(&tree);
    for (text, code, message) in [
        (None, "conflict", format!("profile {name} exists and is not the VM of a workspace: it has no ephemeral record")),
        (
            Some("1 2 /elsewhere\n".to_owned()),
            "conflict",
            format!("profile {name} belongs to workspace /elsewhere, not to {}", workspace.display()),
        ),
        (
            Some(format!("{} {} {}\n", metadata.dev(), metadata.ino() + 1, workspace.display())),
            "conflict",
            format!("profile {name} belongs to an earlier directory at {}", workspace.display()),
        ),
        (Some("not a record\n".to_owned()), "operationFailed", "is not \"<device> <inode> <path>\" on one line".to_owned()),
    ] {
        if let Some(text) = text {
            private_file(&record, &text);
        }
        let before = home.tree();
        for value in [home.up(&tree, &[], &[]).0, home.down(&tree, &[]).0, home.down(&tree, &["--purge"]).0] {
            failure(&value, code, &message);
        }
        assert_eq!(home.tree(), before);
        assert_eq!(home.listed(), [name.clone()]);
    }
}

/// Holds the lifecycle lock of profile `name`, as another operation on it does.
fn hold_lifecycle_lock(home: &Home, name: &str) -> fs::File {
    let locks = home.hamn().join(".locks");
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&locks).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(locks.join(format!("{name}.lock")))
        .unwrap();
    // SAFETY: the descriptor belongs to `lock`, which outlives the call.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
    lock
}

/// `vm up` of `workspace`, once its worker waits for the lifecycle lock of
/// `name` that this test holds: `lsof` shows the worker with the lock file
/// open. The child and the worker's process ID.
fn up_behind_lock(home: &Home, workspace: &Path, name: &str) -> (Child, i32) {
    let mut child = home
        .command(&["vm", "up", "--workspace", workspace.to_str().unwrap(), "--yes"], &[NO_ROOM])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let lock = home.hamn().join(".locks").join(format!("{name}.lock"));
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let output = Command::new("/usr/sbin/lsof").arg("-t").arg("--").arg(&lock).output().unwrap();
        let worker = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<i32>().ok())
            .find(|pid| *pid != std::process::id() as i32);
        if let Some(worker) = worker {
            return (child, worker);
        }
        if child.try_wait().unwrap().is_some() || Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("vm up ended, or ran for 30 s, without opening {}", lock.display());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The envelope and standard error of `child` once it ends, within 30 s.
fn finished(mut child: Child) -> (Value, String) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("vm up did not end within 30 s of the lock's release");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let (mut output, mut stderr) = (String::new(), String::new());
    child.stdout.take().unwrap().read_to_string(&mut output).unwrap();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    (serde_json::from_str(&output).unwrap(), stderr)
}

/// While another operation holds the profile's lifecycle lock, vm up waits
/// for it, then goes on: two vm up of one workspace run one after another.
fn vm_up_waits_while_another_operation_holds_the_profile() {
    let home = Home::new();
    let tree = home.directory("tree");
    let name = profile_of(&tree);
    home.make_ephemeral(&tree, None);
    let lock = hold_lifecycle_lock(&home, &name);
    let (mut child, _) = up_behind_lock(&home, &tree, &name);
    assert!(child.try_wait().unwrap().is_none(), "vm up ran while another operation held profile {name}");
    drop(lock);
    let (value, stderr) = finished(child);
    failure(&value, "admissionRefused", &format!("refused to start the VM of profile {name}: "));
    assert!(
        !stderr.contains("does not share a git directory") && !stderr.contains("created"),
        "a profile started again keeps its mounts and warns of none: {stderr}"
    );
}

/// A vm up cancelled while it waits for the lifecycle lock gives up once it
/// holds the lock, before it creates anything.
fn a_vm_up_cancelled_while_it_waits_creates_nothing() {
    let home = Home::new();
    let tree = home.directory("tree");
    let name = profile_of(&tree);
    let lock = hold_lifecycle_lock(&home, &name);
    let (child, worker) = up_behind_lock(&home, &tree, &name);
    // SAFETY: kill reads no memory; worker is the core worker of the vm up
    // this test started, which still runs while the lock is held.
    assert_eq!(unsafe { libc::kill(worker, libc::SIGTERM) }, 0);
    drop(lock);
    let (value, stderr) = finished(child);
    failure(&value, "cancelled", "cancelled before any change");
    assert!(!stderr.contains("created"), "{stderr}");
    assert!(!home.hamn().join(&name).exists());
}

/// A new profile is made with the requested resources, so a disk below the
/// default 60 GiB is no shrink for its first start.
fn a_new_profile_is_created_with_the_requested_resources() {
    let home = Home::new();
    let tree = home.directory("tree");
    let name = profile_of(&tree);
    let (value, _) = home.up(&tree, &["--cpu", "2", "--memory", "2", "--disk", "30"], &[NO_ROOM]);
    failure(&value, "admissionRefused", &format!("refused to start the VM of profile {name}: "));
    assert_eq!(value["error"]["details"]["request"], json!({"cpus": 2, "memoryMiB": 2048}), "{value}");
    assert!(!home.hamn().join(&name).exists() && home.listed().is_empty());
}
