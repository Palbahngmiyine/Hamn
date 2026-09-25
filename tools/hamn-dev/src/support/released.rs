//! A genuine Hamn 0.1.2 installation to migrate from.
//!
//! The installer, its sourced helpers and the packaging tree come from the
//! v0.1.2 release commit in this checkout's Git history (`git archive`, so a
//! shallow clone fails loudly instead of testing a hand-made tree). They are
//! laid out as the 0.1.2 release archive was (`build-candidate.sh` of that
//! commit: `bin/hamn`, every tracked file of `scripts` and `packaging`, and
//! `packaging/release/update-manifest-url`) and the real 0.1.2
//! `install-host.sh` installs them with the system tools it was written for.
//!
//! One part is a stand-in: the 0.1.2 executable itself cannot be built from
//! this checkout (its build needs Python and that commit's toolchain). The
//! archive's `bin/hamn` is a script that answers `--version` with
//! `hamn 0.1.2` and runs `__install-support` operations in this executable's
//! `released-install-support` fixture, a port of the 0.1.2 private
//! operations the installer calls. The generation layout, modes, markers and
//! links are therefore the 0.1.2 installer's own output.
use super::upgrade::{self, Output};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

/// The v0.1.2 release commit (tag `v0.1.2`).
pub const RELEASE_COMMIT: &str = "3b7ab8ee7ce13b220c8eea0e0cbfe713634b613e";
pub const VERSION: &str = "0.1.2";
const SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// Lays out the 0.1.2 release archive root in `root` and returns it.
pub fn payload(root: &Path) -> PathBuf {
    let checkout = upgrade::checkout();
    let payload = root.join(format!("hamn-v{VERSION}-darwin-arm64"));
    fs::create_dir_all(payload.join("bin")).unwrap();
    let archive = root.join("hamn-v0.1.2-sources.tar");
    let exported = upgrade::run(
        Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["archive", "--format=tar", "-o"])
            .arg(&archive)
            .args([RELEASE_COMMIT, "scripts", "packaging"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null()),
        Duration::from_secs(60),
    );
    assert_eq!(
        exported.returncode,
        0,
        "the v0.1.2 release commit {RELEASE_COMMIT} is not in this checkout's history (a shallow clone?): {}",
        exported.stderr()
    );
    let unpacked = upgrade::run(
        Command::new("/usr/bin/tar").arg("-xf").arg(&archive).arg("-C").arg(&payload),
        Duration::from_secs(60),
    );
    assert_eq!(unpacked.returncode, 0, "{}", unpacked.stderr());
    fs::remove_file(&archive).unwrap();
    let pointer = payload.join("packaging/release/update-manifest-url");
    fs::write(&pointer, "https://github.com/Palbahngmiyine/Hamn/releases/latest/download/hamn-update-manifest.json\n")
        .unwrap();
    fs::set_permissions(&pointer, fs::Permissions::from_mode(0o644)).unwrap();
    let support = root.join("released-fixture-bin/hamn-0.1.2-support");
    fs::create_dir_all(support.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), &support).unwrap();
    let support = support.to_str().expect("UTF-8 path");
    assert!(!support.contains('\''), "unquotable path {support}");
    upgrade::write_executable(
        &payload.join("bin/hamn"),
        &format!(
            "#!/bin/sh\n# Stand-in for the Hamn {VERSION} executable.\n\
             if [ \"$#\" = 1 ] && [ \"$1\" = --version ]; then\n  printf '%s\\n' 'hamn {VERSION}'\n\
             elif [ \"${{1:-}}\" = __install-support ]; then\n  HAMN_DEV_FIXTURE=released-install-support exec '{support}' \"$@\"\n\
             else exit 64; fi\n"
        ),
    );
    payload
}

/// Runs the 0.1.2 installer of `payload` as its release did
/// (`install-host.sh BINARY BINDIR DATADIR`) with an owned HOME and the
/// system tools only.
pub fn install_command(payload: &Path, bindir: &Path, datadir: &Path, home: &Path) -> Command {
    let mut command = Command::new("/bin/bash");
    command
        .arg(payload.join("scripts/install-host.sh"))
        .arg(payload.join("bin/hamn"))
        .arg(bindir)
        .arg(datadir)
        .env_clear()
        .env("HOME", home)
        .env("PATH", SYSTEM_PATH)
        .stdin(Stdio::null());
    command
}

/// Installs Hamn 0.1.2 from `payload`, which must succeed; returns the
/// active command target and the installer's output.
pub fn install(payload: &Path, bindir: &Path, datadir: &Path, home: &Path) -> (PathBuf, Output) {
    let result = upgrade::run(&mut install_command(payload, bindir, datadir, home), Duration::from_secs(120));
    assert_eq!(result.returncode, 0, "the 0.1.2 installer failed: {} {}", result.stdout(), result.stderr());
    let target = fs::read_link(bindir.join("hamn")).expect("0.1.2 command link");
    (target, result)
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn fd_stat(fd: i32) -> Result<libc::stat, String> {
    // fstat fills this zeroed local structure for a descriptor the shell
    // passed down; it is only read after success.
    let mut status: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut status) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(status)
}

/// An owned regular single-link file with `mode`, as 0.1.2 `files::owned`.
fn owned_file(path: &Path, mode: u32) -> Result<fs::Metadata, String> {
    let m = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    let uid = unsafe { libc::geteuid() };
    if m.uid() != uid || !m.is_file() || m.nlink() != 1 || m.mode() & 0o7777 != mode {
        return Err("unsafe ownership".into());
    }
    Ok(m)
}

fn lock_same(path: &Path, fd: i32) -> Result<(), String> {
    let (a, b) = (owned_file(path, 0o600)?, fd_stat(fd)?);
    if a.dev() != b.st_dev as u64 || a.ino() != b.st_ino {
        return Err("transaction descriptor differs".into());
    }
    Ok(())
}

fn flock(fd: i32) -> Result<(), String> {
    loop {
        // The descriptor is the invoking shell's; its open file description
        // (and so the lock) outlives this process, as in 0.1.2.
        if unsafe { libc::flock(fd, libc::LOCK_EX) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.to_string());
        }
    }
}

fn operation(args: &[&str]) -> Result<(), String> {
    let fd = |text: &str| text.parse::<i32>().map_err(|error| error.to_string());
    match args {
        ["hash", path] => println!("{}", digest(&fs::read(path).map_err(|error| error.to_string())?)),
        ["path-hash", path] => println!("{}", digest(format!("{path}\0").as_bytes())),
        ["lock-prepare", path] => {
            let path = Path::new(path);
            let parent = fs::symlink_metadata(path.parent().ok_or("no parent")?).map_err(|error| error.to_string())?;
            if !parent.is_dir()
                || parent.uid() != unsafe { libc::geteuid() }
                || !matches!(parent.mode() & 0o7777, 0o700 | 0o755)
            {
                return Err("unsafe transaction lock parent".into());
            }
            match OpenOptions::new().write(true).create_new(true).mode(0o600).open(path) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.to_string()),
            }
            owned_file(path, 0o600)?;
        }
        ["lock-same", path, descriptor] => lock_same(Path::new(path), fd(descriptor)?)?,
        ["lock-acquire", path, descriptor] => {
            lock_same(Path::new(path), fd(descriptor)?)?;
            flock(fd(descriptor)?)?;
            lock_same(Path::new(path), fd(descriptor)?)?;
        }
        ["fd-identity", descriptor] => {
            let m = fd_stat(fd(descriptor)?)?;
            println!("{}:{}:{}:{:o}:{}", m.st_dev, m.st_ino, m.st_uid, m.st_mode & 0o7777, m.st_nlink);
        }
        ["fd-lock", descriptor] => flock(fd(descriptor)?)?,
        // 0.1.2 collected obsolete generations here; a first installation
        // has none, so the port does nothing.
        ["prune", _bin, _data, _previous, _source] => {}
        _ => return Err("invalid private installer operation or argument count".into()),
    }
    Ok(())
}

/// Fixture `released-install-support`: `__install-support OPERATION ...`
/// for the operations Hamn 0.1.2's installer calls (`hash`, `path-hash`,
/// `lock-prepare`, `lock-same`, `lock-acquire`, `fd-identity`, `fd-lock`
/// and `prune`), ported from that release's `control/install_support`.
pub fn install_support(_program: &str, args: &[String]) -> ExitCode {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(("__install-support", rest)) = args.split_first().map(|(first, rest)| (*first, rest)) else {
        eprintln!("hamn: expected __install-support");
        return ExitCode::from(2);
    };
    match operation(rest) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("hamn: {error}");
            ExitCode::FAILURE
        }
    }
}
