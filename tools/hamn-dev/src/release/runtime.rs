//! A Hamn executable driven in an isolated HOME, for physical and live
//! validation.
//!
//! Contract:
//! - Every child gets exactly [`Runtime::environment`]: the isolated HOME, a
//!   system PATH (`/usr/bin:/bin:/usr/sbin:/sbin`) and `LC_ALL=C`, so no
//!   user Docker, Kubernetes or Hamn target can leak in. Callers may add
//!   variables before use.
//! - Headless calls always name their profile (`--profile`) and must return
//!   `schemaVersion: 1, ok: true`; anything else, including
//!   `outcomeUnknown`, is an error. Log calls read NDJSON records, which
//!   must all be `ok`, and return the last record's data.
//! - Every child is bounded by [`Runtime::timeout`] (660 s by default) and
//!   killed and reaped when it expires (see [`super::process`]).
//! - [`Runtime::stop`] stops only the named, owned profiles and reports
//!   every profile it could not prove stopped; callers then keep the
//!   workspace instead of deleting disks under a running VM.
use super::process::{self, Spec};
use super::syntax::shell_join;
use crate::support::pty::{self, Pty};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub struct Runtime {
    pub binary: PathBuf,
    pub home: PathBuf,
    /// The external Docker CLI used for engine calls on a profile's socket.
    pub docker: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub timeout: Duration,
}

impl Runtime {
    pub fn new(binary: impl Into<PathBuf>, home: impl Into<PathBuf>, docker: impl Into<PathBuf>) -> Self {
        let home = home.into();
        let environment = BTreeMap::from([
            ("HOME".to_owned(), home.to_string_lossy().into_owned()),
            ("PATH".to_owned(), "/usr/bin:/bin:/usr/sbin:/sbin".to_owned()),
            ("LC_ALL".to_owned(), "C".to_owned()),
        ]);
        Self { binary: binary.into(), home, docker: docker.into(), environment, timeout: Duration::from_secs(660) }
    }

    /// The profile's directory in the isolated HOME.
    pub fn profile_dir(&self, profile: &str) -> PathBuf {
        self.home.join(".hamn").join(profile)
    }

    /// Runs `program args` with this runtime's environment and deadline and
    /// returns its standard output; a nonzero exit is an error.
    pub fn run<S: AsRef<OsStr>>(&self, program: &OsStr, args: &[S]) -> Result<String, String> {
        process::run(program, args, &Spec { environment: Some(&self.environment), input: None }, self.timeout)
    }

    /// `hamn --headless WORDS... --profile PROFILE FLAGS...`, returning the
    /// result's `data`.
    pub fn call(&self, words: &[&str], profile: &str, flags: &[&str]) -> Result<Value, String> {
        let mut args: Vec<&str> = vec!["--headless"];
        args.extend(words);
        args.extend(["--profile", profile]);
        args.extend(flags);
        let output = self.run(self.binary.as_os_str(), &args)?;
        let logs = words.last() == Some(&"logs") || words.get(2) == Some(&"logs");
        let result = if logs {
            let records = output
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).map_err(|error| format!("invalid log record: {error}")))
                .collect::<Result<Vec<Value>, String>>()?;
            if records.is_empty() || records.iter().any(|record| record.get("ok") != Some(&Value::Bool(true))) {
                return Err("invalid log stream result".into());
            }
            records.into_iter().last().expect("nonempty records")
        } else {
            serde_json::from_str(&output).map_err(|error| format!("invalid headless result: {error}"))?
        };
        if result.get("schemaVersion") != Some(&Value::from(1)) || result.get("ok") != Some(&Value::Bool(true)) {
            return Err(format!("invalid headless result: {result}"));
        }
        result.get("data").cloned().ok_or_else(|| format!("headless result has no data: {result}"))
    }

    /// The external Docker CLI against the profile's forwarded engine socket.
    pub fn engine(&self, words: &[&str], profile: &str) -> Result<String, String> {
        let socket = self.profile_dir(profile).join("docker.sock");
        let mut args: Vec<OsString> = vec!["--host".into(), format!("unix://{}", socket.display()).into()];
        args.extend(words.iter().map(OsString::from));
        self.run(self.docker.as_os_str(), &args)
    }

    /// Runs `script` as root in the profile's guest with `bash -euc` over the
    /// profile's own SSH key, never the user's SSH configuration.
    // The physical gate needs no guest shell; live validation suites do.
    #[allow(dead_code)]
    pub fn ssh(&self, script: &str, profile: &str) -> Result<String, String> {
        let status = self.call(&["vm", "status"], profile, &[])?;
        let address = status.get("ip").and_then(Value::as_str).ok_or("VM status has no IP address")?;
        let key = self.profile_dir(profile).join("id_ed25519");
        let mut args: Vec<OsString> = vec!["-F".into(), "none".into(), "-i".into(), key.into()];
        for option in [
            "BatchMode=yes",
            "IdentitiesOnly=yes",
            "UserKnownHostsFile=/dev/null",
            "StrictHostKeyChecking=no",
            "ConnectTimeout=10",
        ] {
            args.extend(["-o".into(), option.into()]);
        }
        args.push(format!("hamn@{address}").into());
        args.push(shell_join(&["sudo", "bash", "-euc", script]).into());
        self.run(OsStr::new("/usr/bin/ssh"), &args)
    }

    /// Opens the TUI on a 24x100 PTY, waits for it to render, quits with `q`
    /// and requires a zero exit with the terminal settings restored.
    pub fn terminal(&self) -> Result<(), String> {
        self.terminal_within(Duration::from_secs(15), Duration::from_secs(10))
    }

    pub fn terminal_within(&self, render: Duration, quit: Duration) -> Result<(), String> {
        let pty = Pty::open(24, 100);
        let master = pty.master.as_raw_fd();
        // Settings are read through the master, which reports the slave's
        // termios: macOS revokes the parent's slave descriptor once the
        // child's session ends (observed on macOS 27), so it cannot be read
        // afterwards.
        let before = TerminalSettings::read(master)?;
        let mut command = Command::new(&self.binary);
        command.env_clear().envs(&self.environment).env("TERM", "xterm-256color");
        let mut session = Session(pty.spawn(&mut command));
        let mut output = Vec::new();
        let deadline = Instant::now() + render;
        while !output.windows(4).any(|window| window == b"Hamn") {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || pty::readable(&[master], remaining).is_empty() {
                return Err("TUI did not render before the deadline".into());
            }
            output.extend(pty::read_some(master));
        }
        pty::write_all(master, b"q");
        // Keep consuming redraw and restore bytes while quitting: a full PTY
        // output queue could otherwise block the application's writer.
        let status = pty::wait_for_exit(&mut session.0, master, quit + Duration::from_secs(1))
            .ok_or("TUI did not exit after q before the deadline")?;
        if !status.success() || TerminalSettings::read(master)? != before {
            return Err("TUI did not restore the terminal".into());
        }
        Ok(())
    }

    /// Stops each owned profile and proves it stopped. All profiles are
    /// attempted; any failure is reported and the workspace must be kept.
    pub fn stop(&self, profiles: &[String]) -> Result<(), String> {
        let mut errors = Vec::new();
        for profile in profiles {
            let stopped = self
                .call(&["vm", "stop"], profile, &["--yes"])
                .and_then(|_| self.call(&["vm", "status"], profile, &[]))
                .map(|status| status.get("state") == Some(&Value::from("stopped")));
            match stopped {
                Ok(true) => {}
                Ok(false) => errors.push(profile.clone()),
                Err(error) => errors.push(error),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("physical test cleanup failed; workspace retained: {}", errors.join("; ")))
        }
    }
}

/// Kills the TUI's session if it is still running when the check ends.
struct Session(Child);

impl Drop for Session {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            pty::kill_group(self.0.id(), libc::SIGKILL);
            let _ = self.0.wait();
        }
    }
}

/// The terminal attributes the TUI must restore. `PENDIN` (pending input
/// reprint) is excluded because the line discipline itself toggles it.
#[derive(Debug, PartialEq, Eq)]
struct TerminalSettings {
    flags: [u64; 4],
    speeds: [u64; 2],
    control: Vec<u8>,
}

impl TerminalSettings {
    fn read(fd: RawFd) -> Result<Self, String> {
        // SAFETY: termios is plain data, fully written by tcgetattr on success.
        let mut settings: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: fd is an open terminal descriptor; settings is writable.
        if unsafe { libc::tcgetattr(fd, &mut settings) } != 0 {
            return Err(format!("tcgetattr: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: both read the initialized settings.
        let speeds = unsafe { [libc::cfgetispeed(&settings) as u64, libc::cfgetospeed(&settings) as u64] };
        Ok(Self {
            flags: [
                settings.c_iflag as u64,
                settings.c_oflag as u64,
                settings.c_cflag as u64,
                (settings.c_lflag & !libc::PENDIN) as u64,
            ],
            speeds,
            control: settings.c_cc.to_vec(),
        })
    }
}

/// `PATH` lookup of an executable, like `shutil.which`.
pub fn which(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path?).map(|directory| directory.join(name)).find(|candidate| {
        std::fs::metadata(candidate).is_ok_and(|info| info.is_file() && info.permissions().mode() & 0o111 != 0)
    })
}
