//! A real Hamn TUI on a PTY, with recorded `docker` and `kubectl` fixtures
//! (links to this executable) in a disposable HOME, shared by the TUI and
//! workspace suites.
use super::pty::{self, Pty};
use super::screen::RatatuiScreen;
use super::tmp::TempDir;
use serde_json::{Value, json};
use std::fs;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Installs `root/bin/<name>` as a link to this executable, which then acts
/// as the fixture that `HAMN_DEV_FIXTURE` (or `root/fixture-<name>`) selects.
pub fn install_fixture(bin: &Path, name: &str) {
    let path = bin.join(name);
    let _ = fs::remove_file(&path);
    std::os::unix::fs::symlink(std::env::current_exe().expect("current executable"), &path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// Selects the fixture behavior of `root/bin/<cli>` for this harness only.
pub fn select_fixture(root: &Path, cli: &str, fixture: &str) {
    fs::write(root.join(format!("fixture-{cli}")), fixture).unwrap();
}

pub struct Options<'a> {
    pub workspace: &'a str,
    pub namespace: Option<&'a str>,
    pub prepare_preferences: Option<&'a dyn Fn(&Path)>,
    /// The fixture behavior of `docker` and `kubectl`.
    pub fixture: &'a str,
}

impl<'a> Options<'a> {
    pub fn new(workspace: &'a str) -> Self {
        Self { workspace, namespace: None, prepare_preferences: None, fixture: "native-regressions" }
    }
}

pub struct Harness {
    pub root: PathBuf,
    pub pty: Pty,
    pub notice: OwnedFd,
    pub gate: OwnedFd,
    pub child: Child,
    pub screen: RatatuiScreen,
    pub output: Vec<u8>,
    pub notices: Vec<u8>,
    // Dropped last: the directory outlives the child and descriptors.
    _directory: TempDir,
}

impl Harness {
    pub fn new(workspace: &str) -> Self {
        Self::with(Options::new(workspace))
    }

    pub fn with(options: Options) -> Self {
        let directory = TempDir::new("hamn-native-regression-");
        let root = directory.path().to_path_buf();
        fs::create_dir(root.join("bin")).unwrap();
        fs::create_dir(root.join(".hamn")).unwrap();
        fs::set_permissions(root.join(".hamn"), fs::Permissions::from_mode(0o700)).unwrap();
        let preferences = root.join(".hamn/tui.json");
        fs::write(&preferences, json!({"version": 1, "defaultWorkspace": options.workspace}).to_string()).unwrap();
        fs::set_permissions(&preferences, fs::Permissions::from_mode(0o600)).unwrap();
        if let Some(prepare) = options.prepare_preferences {
            prepare(&preferences);
        }
        for name in ["docker", "kubectl"] {
            install_fixture(&root.join("bin"), name);
        }
        let config = root.join("kubeconfig");
        fs::write(
            &config,
            json!({"apiVersion": "v1", "kind": "Config", "current-context": "old-cluster",
                "contexts": [{"name": "old-cluster", "context": {"cluster": "fixture", "namespace": "test"}}],
                "clusters": [{"name": "fixture", "cluster": {"server": "http://127.0.0.1:1"}}]})
            .to_string(),
        )
        .unwrap();
        let notice = pty::fifo(&root.join("notice"));
        let gate = pty::fifo(&root.join("gate"));
        let pty = Pty::open(32, 160);
        let mut command = Command::new(super::hamn());
        if let Some(namespace) = options.namespace {
            command.args(["--namespace", namespace]);
        }
        command
            .env("HOME", &root)
            .env("FIXTURE_ROOT", &root)
            .env("HAMN_DEV_FIXTURE", options.fixture)
            .env("PATH", format!("{}/bin:/usr/bin:/bin", root.display()))
            .env("TERM", "xterm-256color")
            .env("KUBECONFIG", &config);
        let child = pty.spawn(&mut command);
        Self {
            root,
            pty,
            notice,
            gate,
            child,
            screen: RatatuiScreen::new(32, 160),
            output: Vec::new(),
            notices: Vec::new(),
            _directory: directory,
        }
    }

    pub fn master(&self) -> i32 {
        self.pty.master.as_raw_fd()
    }

    /// Reads the PTY and the notice FIFO until `predicate` holds, failing
    /// after 15 seconds with the screen in the message.
    pub fn wait(&mut self, predicate: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !predicate(self) {
            let remaining = deadline.checked_duration_since(Instant::now());
            let remaining = remaining.unwrap_or_else(|| panic!("timed out; screen:\n{}", self.screen.text()));
            let ready = pty::readable(&[self.master(), self.notice.as_raw_fd()], remaining);
            assert!(!ready.is_empty(), "timed out; screen:\n{}", self.screen.text());
            for fd in ready {
                let data = pty::read_some(fd);
                assert!(!data.is_empty(), "PTY closed before the expected result");
                if fd == self.master() {
                    self.output.extend_from_slice(&data);
                    self.screen.feed(&data);
                } else {
                    self.notices.extend_from_slice(&data);
                }
            }
        }
    }

    pub fn until(&mut self, text: &str) {
        self.wait(|harness| harness.screen.text().contains(text));
    }

    pub fn noticed(&mut self, text: &str) {
        let text = text.as_bytes().to_vec();
        self.wait(move |harness| harness.notices.windows(text.len()).any(|window| window == text));
    }

    pub fn write(&self, keys: &[u8]) {
        pty::write_all(self.master(), keys);
    }

    pub fn send(&mut self, keys: &[u8], text: &str) {
        self.write(keys);
        self.until(text);
    }

    pub fn release_gate(&self) {
        pty::write_all(self.gate.as_raw_fd(), b"1");
    }

    pub fn text(&self) -> String {
        self.screen.text()
    }

    /// The fixture invocations so far, as (program name, arguments).
    pub fn calls(&self) -> Vec<(String, Vec<String>)> {
        let text = fs::read_to_string(self.root.join("calls")).unwrap_or_default();
        text.lines()
            .map(|line| {
                let value: Value = serde_json::from_str(line).unwrap();
                let program = value[0].as_str().unwrap().to_owned();
                let args = value[1].as_array().unwrap().iter().map(|arg| arg.as_str().unwrap().to_owned()).collect();
                (program, args)
            })
            .collect()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            pty::kill(self.child.id(), libc::SIGTERM);
            if pty::wait_for_exit(&mut self.child, self.pty.master.as_raw_fd(), Duration::from_secs(5)).is_none() {
                pty::kill_group(self.child.id(), libc::SIGKILL);
                let _ = self.child.wait();
            }
        }
    }
}
