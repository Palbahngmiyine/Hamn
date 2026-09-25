//! Release fixtures for the installer and updater suites: digests, schema v3
//! manifests, `release/` host archives made with the system tar, copies of
//! the checkout's release support files, and bounded updater processes in an
//! owned HOME. Nothing here touches the real `~/.hamn` or `~/.local`.
use super::pty;
use super::real_cli;
pub use super::real_cli::Output;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The checkout under test (the working directory, as the Makefile runs
/// suites from the repository root). Panics unless it holds the updater.
pub fn checkout() -> PathBuf {
    let root = std::env::current_dir().expect("working directory");
    assert!(root.join("scripts/update-host.sh").is_file(), "run from the Hamn checkout: {}", root.display());
    root
}

/// Lowercase hexadecimal SHA-256 of `bytes`.
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Lowercase hexadecimal SHA-256 of the file at `path`.
pub fn file_digest(path: &Path) -> String {
    digest(&fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display())))
}

/// One manifest artifact: where it is, its digest and its byte size.
#[derive(Clone, Debug)]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

impl Artifact {
    /// A local `file://` artifact (accepted only with
    /// `HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS=1`).
    pub fn local(path: &Path) -> Self {
        let bytes = fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        Self { url: format!("file://{}", path.display()), sha256: digest(&bytes), size: bytes.len() as u64 }
    }
}

/// A valid stable schema v3 release manifest (`docs/INSTALLATION.md`).
pub fn manifest(version: &str, host: &Artifact, guest: &Artifact) -> Value {
    json!({
        "schemaVersion": 3, "channel": "stable", "version": version, "commit": "a".repeat(40),
        "validationMode": "github-hosted-no-vm",
        "compatibility": {"os": "darwin", "architecture": "arm64", "minimumMacOS": "13.0"},
        "artifacts": {
            "host": {"url": host.url, "sha256": host.sha256, "size": host.size},
            "guestImage": {"url": guest.url, "sha256": guest.sha256, "size": guest.size,
                "format": "qcow2", "compression": "zlib", "virtualSize": 8_u64 * 1024 * 1024 * 1024},
        },
    })
}

/// Writes `value` as one JSON line.
pub fn write_json(path: &Path, value: &Value) {
    fs::write(path, format!("{value}\n")).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// Writes `value` as one JSON line with mode 0600.
pub fn write_private_json(path: &Path, value: &Value) {
    write_json(path, value);
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

/// Writes an executable (0755) file.
pub fn write_executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Copies the checkout's `scripts` and `packaging` into `release` (as a
/// host archive and an installed generation carry them).
pub fn copy_release_support(release: &Path) {
    let root = checkout();
    fs::create_dir_all(release).unwrap();
    for name in ["scripts", "packaging"] {
        let status = Command::new("/bin/cp").arg("-R").arg(root.join(name)).arg(release).status().expect("cp");
        assert!(status.success(), "copy {name} into {}", release.display());
    }
}

/// Packs the directory `source` as `release/` into the gzip tar `archive`,
/// without macOS metadata, as release candidates are packed.
pub fn pack_release(source: &Path, archive: &Path) {
    let parent = source.parent().expect("release parent");
    let name = source.file_name().and_then(|name| name.to_str()).expect("release name");
    let output = Command::new("/usr/bin/tar")
        .env("COPYFILE_DISABLE", "1")
        .args(["--no-mac-metadata", "-czf"])
        .arg(archive)
        .arg("-C")
        .arg(parent)
        .args(["-s", &format!("|^{name}|release|")])
        .arg(name)
        .output()
        .expect("tar");
    assert!(output.status.success(), "tar: {}", String::from_utf8_lossy(&output.stderr));
}

/// A FIFO named `name` in `root`, opened for a bounded ready wait.
pub fn ready_fifo(root: &Path, name: &str) -> (PathBuf, OwnedFd) {
    let path = root.join(name);
    let fd = pty::fifo(&path);
    (path, fd)
}

/// Waits up to `timeout` for a `ready\n` line on the FIFO descriptor `fd`.
pub fn await_ready(fd: &OwnedFd, timeout: Duration, what: &str) {
    assert!(!pty::readable(&[fd.as_raw_fd()], timeout).is_empty(), "{what} did not reach its boundary within {timeout:?}");
    assert_eq!(pty::read_some(fd.as_raw_fd()), b"ready\n", "{what}");
}

/// Runs `command` to completion within `timeout` with captured output.
pub fn run(command: &mut Command, timeout: Duration) -> Output {
    real_cli::run(command, timeout)
}

/// Creates a FIFO at `path` without opening it: an updater barrier opens it
/// for reading after announcing readiness; `release` then writes to it.
pub fn mkfifo(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: name is a valid C string.
    let result = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
    assert_eq!(result, 0, "mkfifo {}: {}", path.display(), std::io::Error::last_os_error());
}

/// Writes one line to the barrier FIFO at `path` once its reader has opened
/// it, failing after `timeout`. A non-blocking open reports ENXIO until the
/// reader exists, so a reader that died cannot hang the test.
pub fn release(path: &Path, timeout: Duration) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let deadline = Instant::now() + timeout;
    loop {
        match fs::OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(path) {
            Ok(mut writer) => {
                writer.write_all(b"continue\n").expect("release barrier");
                return;
            }
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                assert!(Instant::now() < deadline, "no reader opened {} within {timeout:?}", path.display());
                // The reader opens right after its ready line; wait briefly.
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("{}: {error}", path.display()),
        }
    }
}

/// A child process group leader (Python's `start_new_session=True`). When
/// dropped with the child still running, the whole group is killed with
/// SIGKILL and the leader reaped, so no updater or installer survives a test.
pub struct Group(Option<Child>);

impl Group {
    /// Starts `command` in a new process group with piped stdout/stderr.
    pub fn spawn(command: &mut Command) -> Self {
        command.stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
        Self(Some(command.spawn().unwrap_or_else(|error| panic!("spawn {command:?}: {error}"))))
    }

    /// Like `spawn`, with standard error sent to `stderr` (for example a
    /// terminal) and standard input from /dev/null.
    pub fn spawn_with_stderr(command: &mut Command, stderr: Stdio) -> Self {
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(stderr).process_group(0);
        Self(Some(command.spawn().unwrap_or_else(|error| panic!("spawn {command:?}: {error}"))))
    }

    /// The piped standard error, for bounded reads before `finish` collects
    /// the rest.
    pub fn stderr_fd(&self) -> std::os::fd::RawFd {
        self.0.as_ref().expect("child").stderr.as_ref().expect("piped standard error").as_raw_fd()
    }

    pub fn id(&self) -> u32 {
        self.0.as_ref().expect("running child").id()
    }

    /// Whether the leader is still running.
    pub fn running(&mut self) -> bool {
        matches!(self.0.as_mut().expect("child").try_wait(), Ok(None))
    }

    /// Sends `signal` to the leader only (the updater's own traps run).
    pub fn signal(&self, signal: i32) {
        pty::kill(self.id(), signal);
    }

    /// Sends `signal` to the whole group.
    pub fn signal_group(&self, signal: i32) {
        pty::kill_group(self.id(), signal);
    }

    /// Reads all output and waits within `timeout` (`communicate`).
    pub fn finish(mut self, timeout: Duration) -> Output {
        real_cli::communicate(self.0.take().expect("child"), timeout)
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            if child.try_wait().ok().flatten().is_none() {
                pty::kill_group(child.id(), libc::SIGKILL);
            }
            let _ = child.wait();
        }
    }
}

/// A `bin/hamn` release stand-in: `--version` prints `hamn VERSION`, every
/// private `__install-support` operation runs the frozen executable `native`
/// (the Hamn under test), and anything else exits 64.
pub fn version_wrapper(version: &str, native: &Path) -> String {
    let native = native.to_str().expect("UTF-8 path");
    assert!(!native.contains('\''), "unquotable path {native}");
    format!(
        "#!/bin/sh\nif [ \"$#\" = 1 ] && [ \"$1\" = --version ]; then\n  printf \"%s\\n\" \"hamn {version}\"\n\
         elif [ \"${{1:-}}\" = __install-support ]; then\n  exec '{native}' \"$@\"\nelse exit 64; fi\n"
    )
}

/// An owned HOME (`root/home`, install roots `home/bin` and `home/source`)
/// plus version-wrapped releases of the checkout's updater, all under one
/// temporary root. Private operations run one frozen copy of the Hamn under
/// test, so no build output is read after setup. Each release updater gets
/// one fixture-only observation point, right before it takes the install
/// root locks: `HAMN_TEST_UPDATE_BEFORE_LOCK_READY_FIFO` receives `ready`.
pub struct Releases {
    pub root: PathBuf,
    pub home: PathBuf,
    pub bindir: PathBuf,
    pub datadir: PathBuf,
    /// The managed `hamn` command link.
    pub command: PathBuf,
    /// The selected guest image record.
    pub selection: PathBuf,
    native: PathBuf,
    _directory: super::tmp::TempDir,
}

/// The updater the releases carry: `$HAMN_TEST_UPDATER_SOURCE`, or the
/// checkout's `scripts/update-host.sh`.
fn updater_source() -> PathBuf {
    std::env::var_os("HAMN_TEST_UPDATER_SOURCE").map_or_else(|| checkout().join("scripts/update-host.sh"), PathBuf::from)
}

impl Releases {
    pub fn new(prefix: &str) -> Self {
        let directory = super::tmp::TempDir::new(prefix);
        let root = fs::canonicalize(directory.path()).unwrap();
        let home = root.join("home");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let native = root.join("native-hamn");
        fs::copy(super::hamn(), &native).unwrap();
        let checked = run(Command::new(&native).args(["__install-support", "upgrade", "version", "1.0.1"]), Duration::from_secs(10));
        assert_eq!(checked.returncode, 0, "frozen native support: {checked:?}");
        Self {
            bindir: home.join("bin"),
            datadir: home.join("source"),
            command: home.join("bin/hamn"),
            selection: home.join(".hamn/cache/guest-image.json"),
            root,
            home,
            native,
            _directory: directory,
        }
    }

    /// Builds release `version` (`root/release-VERSION`, its host archive, a
    /// guest image and a v3 manifest) and returns its updater and manifest.
    pub fn release(&self, version: &str) -> (PathBuf, PathBuf) {
        let release = self.root.join(format!("release-{version}"));
        fs::create_dir_all(release.join("bin")).unwrap();
        write_executable(&release.join("bin/hamn"), &version_wrapper(version, &self.native));
        copy_release_support(&release);
        let updater = release.join("scripts/update-host.sh");
        let source = fs::read_to_string(updater_source()).unwrap();
        let boundary = "source \"$script_dir/install-transaction.sh\"";
        assert_eq!(source.matches(boundary).count(), 1, "missing root-lock fixture boundary");
        let observation = "if [ -n \"${HAMN_TEST_UPDATE_BEFORE_LOCK_READY_FIFO:-}\" ]; then\n  \
                           printf \"ready\\n\" >\"$HAMN_TEST_UPDATE_BEFORE_LOCK_READY_FIFO\"\nfi\n";
        write_executable(&updater, &source.replace(boundary, &format!("{observation}{boundary}")));
        fs::write(release.join("packaging/release/update-manifest-url"), "https://fixture.test/manifest-v3.json\n").unwrap();
        let archive = self.root.join(format!("{version}.tar.gz"));
        pack_release(&release, &archive);
        let guest = self.root.join(format!("{version}.img"));
        fs::write(&guest, format!("guest-{version}")).unwrap();
        let manifest_path = self.root.join(format!("{version}.json"));
        write_json(&manifest_path, &manifest(&format!("v{version}"), &Artifact::local(&archive), &Artifact::local(&guest)));
        (updater, manifest_path)
    }

    /// The updater of the currently installed generation.
    pub fn installed_updater(&self) -> PathBuf {
        let target = fs::canonicalize(&self.command).expect("installed command");
        target.parent().and_then(Path::parent).expect("generation").join("share/hamn/src/scripts/update-host.sh")
    }

    /// `bash UPDATER --bindir ... --datadir ... --manifest MANIFEST
    /// --output-json OPTIONS...` in this HOME, with local artifacts allowed.
    pub fn updater(&self, script: &Path, manifest: &Path, options: &[&str]) -> Command {
        let mut command = Command::new("bash");
        command
            .arg(script)
            .arg("--bindir")
            .arg(&self.bindir)
            .arg("--datadir")
            .arg(&self.datadir)
            .arg("--manifest")
            .arg(manifest)
            .arg("--output-json")
            .args(options)
            .env("HOME", &self.home)
            .env("TMPDIR", &self.root)
            .env("HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS", "1");
        command
    }

    /// Runs an updater to success and returns its JSON result.
    pub fn run_update(&self, script: &Path, manifest: &Path, options: &[&str]) -> Value {
        let result = run(&mut self.updater(script, manifest, options), Duration::from_secs(30));
        assert_eq!(result.returncode, 0, "{}", result.stderr());
        serde_json::from_slice(&result.stdout).expect("updater JSON result")
    }

    /// Starts an updater in its own process group with `extra` environment.
    pub fn spawn(&self, script: &Path, manifest: &Path, options: &[&str], extra: &[(&str, &Path)]) -> Group {
        let mut command = self.updater(script, manifest, options);
        for (name, value) in extra {
            command.env(name, value);
        }
        Group::spawn(&mut command)
    }

    /// The active command target.
    pub fn active(&self) -> PathBuf {
        fs::read_link(&self.command).expect("managed command link")
    }

    /// The pending update journal of this HOME.
    pub fn journal(&self) -> PathBuf {
        self.home.join(".hamn/cache/.hamn-update-transaction")
    }
}
