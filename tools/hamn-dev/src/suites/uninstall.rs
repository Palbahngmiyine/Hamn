//! Managed uninstall requires an exact confirmation and preserves every
//! foreign or unsafe path. Installs use the native installer in owned
//! homes; no VM is started.
use crate::runner::{self, case};
use crate::support::real_cli;
use crate::support::tmp::TempDir;
use crate::support::upgrade::{self, Output};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "uninstall",
        "uninstall confirmation and managed-path safety",
        vec![
            case(
                "confirmation_is_exact_and_yes_removes_only_the_managed_install",
                confirmation_is_exact_and_yes_removes_only_the_managed_install,
            ),
            case("symlinked_runtime_root_is_never_followed", symlinked_runtime_root_is_never_followed),
            case("empty_data_marker_is_not_ownership_evidence", empty_data_marker_is_not_ownership_evidence),
        ],
        filters,
    )
}

const RUN: Duration = Duration::from_secs(60);

/// One owned install with runtime data: a profile disk and a cached image.
struct Install {
    home: PathBuf,
    bindir: PathBuf,
    datadir: PathBuf,
    _directory: TempDir,
}

impl Install {
    fn new(name: &str) -> Self {
        let directory = TempDir::new("hamn-uninstall-");
        let root = fs::canonicalize(directory.path()).unwrap();
        let (home, bindir, datadir) = (
            root.join(format!("{name}-home")),
            root.join(format!("{name}-bin")),
            root.join(format!("{name}-share/hamn/src")),
        );
        fs::create_dir_all(home.join(".hamn/default")).unwrap();
        fs::create_dir_all(home.join(".hamn/cache")).unwrap();
        fs::File::create(home.join(".hamn/default/disk.img")).unwrap().set_len(4096).unwrap();
        fs::write(home.join(".hamn/cache/image"), "cached-image\n").unwrap();
        let hamn = crate::support::hamn();
        upgrade::install(&hamn, &hamn, &bindir, &datadir, &home);
        Self { home, bindir, datadir, _directory: directory }
    }

    /// `hamn --headless system uninstall ARGS...` through the managed link,
    /// with `answer` on standard input (`None`: an empty standard input).
    fn uninstall(&self, args: &[&str], answer: Option<&str>) -> Output {
        let mut command = Command::new(self.bindir.join("hamn"));
        command
            .args(["--headless", "system", "uninstall"])
            .args(args)
            .env("HOME", &self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("uninstall");
        let mut input = child.stdin.take().unwrap();
        if let Some(answer) = answer {
            // A command that exits before reading closes its standard input.
            let _ = input.write_all(format!("{answer}\n").as_bytes());
        }
        drop(input);
        real_cli::communicate(child, RUN)
    }

    fn intact(&self) -> bool {
        self.home.join(".hamn").is_dir()
            && fs::symlink_metadata(self.bindir.join("hamn")).is_ok_and(|m| m.file_type().is_symlink())
            && self.datadir.is_dir()
    }
}

fn data_lock(datadir: &Path) -> PathBuf {
    datadir.parent().unwrap().join(format!(".{}.hamn-install.lock", datadir.file_name().unwrap().to_str().unwrap()))
}

fn confirmation_is_exact_and_yes_removes_only_the_managed_install() {
    let install = Install::new("managed");
    // n, uppercase Y, and end of input preserve runtime data and install files.
    for answer in [Some("n"), Some("Y"), None] {
        let result = install.uninstall(&[], answer);
        assert_ne!(result.returncode, 0, "uninstall accepted {answer:?}");
        assert!(install.intact(), "uninstall changed data after {answer:?}");
        if answer == Some("n") {
            assert!(result.stdout().contains("\"code\":\"invalidRequest\""), "{}", result.stdout());
        }
    }
    // --yes removes only the proven installation and the Hamn runtime.
    let result = install.uninstall(&["--yes"], None);
    assert_eq!(result.returncode, 0, "{} {}", result.stdout(), result.stderr());
    assert!(result.stdout().contains("\"completed\":true"), "{}", result.stdout());
    assert!(!install.home.join(".hamn").exists());
    assert!(fs::symlink_metadata(install.bindir.join("hamn")).is_err());
    assert!(!install.datadir.exists(), "confirmed uninstall left managed files behind");
    assert!(
        !install.bindir.join(".hamn-install.lock").exists() && !data_lock(&install.datadir).exists(),
        "confirmed uninstall left managed install locks behind"
    );
}

fn symlinked_runtime_root_is_never_followed() {
    let install = Install::new("unsafe");
    let victim = install.home.parent().unwrap().join("victim");
    fs::create_dir(&victim).unwrap();
    fs::write(victim.join("keep"), "keep\n").unwrap();
    fs::remove_dir_all(install.home.join(".hamn")).unwrap();
    std::os::unix::fs::symlink(&victim, install.home.join(".hamn")).unwrap();
    let result = install.uninstall(&["--yes"], Some("y"));
    assert_ne!(result.returncode, 0, "uninstall accepted a symlinked runtime root");
    assert!(result.stdout().contains("refusing unsafe Hamn runtime path"), "{}", result.stdout());
    assert_eq!(fs::read_to_string(victim.join("keep")).unwrap(), "keep\n");
    assert!(fs::symlink_metadata(install.home.join(".hamn")).unwrap().file_type().is_symlink());
    assert!(
        fs::symlink_metadata(install.bindir.join("hamn")).unwrap().file_type().is_symlink() && install.datadir.is_dir()
    );
}

/// An empty (pre-release) data marker is not ownership evidence: uninstall
/// refuses the root and removes nothing.
fn empty_data_marker_is_not_ownership_evidence() {
    let install = Install::new("empty-marker");
    fs::write(install.datadir.join(".hamn-managed"), "").unwrap();
    let result = install.uninstall(&["--yes"], None);
    assert_ne!(result.returncode, 0, "uninstall accepted an empty data marker");
    assert!(result.stdout().contains("refusing unmanaged or unsafe installation root"), "{}", result.stdout());
    assert!(install.intact(), "a refused uninstall changed the installation");
    assert_eq!(fs::metadata(install.datadir.join(".hamn-managed")).unwrap().len(), 0);
}
