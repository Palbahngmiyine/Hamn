//! The TUI under test on a real 38x160 PTY. Output is appended to
//! `live-tui.ansi` in the root and the last matched screen is kept in
//! `live-tui-screen.txt`, as independent evidence of what a user saw.
use super::{Live, PROFILE, check_interrupt, finally, readable};
use crate::support::exec::wait_timeout;
use crate::support::pty::{self, Pty};
use crate::support::screen::RatatuiScreen;
use crate::support::termios;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// A screen the live helpers can drive: the live [`Terminal`], or the
/// recorded-peer TUI harness in the helpers' own checks.
pub(crate) trait Driver {
    fn text(&self) -> String;
    /// Reads output until `predicate` holds for the screen text.
    fn wait_for(&mut self, predicate: &dyn Fn(&str) -> bool);
    fn write(&mut self, keys: &[u8]);

    fn until(&mut self, marker: &str) {
        self.wait_for(&|text| text.contains(marker));
    }

    fn send(&mut self, keys: &[u8], marker: Option<&str>) {
        self.write(keys);
        if let Some(marker) = marker {
            self.until(marker);
        }
    }
}

pub(crate) struct Terminal {
    root: PathBuf,
    pty: Pty,
    /// Read through the master, which reports the slave's settings: macOS
    /// revokes the parent's slave descriptor once the child's session ends.
    before: termios::Settings,
    pub child: Child,
    pub screen: RatatuiScreen,
    record: File,
    closed: bool,
}

impl Terminal {
    /// Starts `binary --profile verify` with exactly `environment`.
    pub(crate) fn new(binary: &Path, environment: &BTreeMap<String, String>, root: &Path) -> Self {
        let pty = Pty::open(38, 160);
        let before = termios::settings(pty.master.as_raw_fd());
        let path = root.join("live-tui.ansi");
        let record = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let mut command = Command::new(binary);
        command.args(["--profile", PROFILE]).env_clear().envs(environment);
        let child = pty.spawn(&mut command);
        Self {
            root: root.to_path_buf(),
            pty,
            before,
            child,
            screen: RatatuiScreen::new(38, 160),
            record,
            closed: false,
        }
    }

    pub(crate) fn master(&self) -> RawFd {
        self.pty.master.as_raw_fd()
    }

    pub(crate) fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Reads until `predicate` holds for the screen text, failing with the
    /// screen after `timeout`.
    pub(crate) fn wait_within(&mut self, predicate: &dyn Fn(&str) -> bool, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while !predicate(&self.screen.text()) {
            check_interrupt();
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "{}", self.screen.text());
            if readable(&[self.master()], left).is_empty() {
                continue;
            }
            assert!(self.pump(), "PTY closed before the expected result");
        }
        fs::write(self.root.join("live-tui-screen.txt"), self.screen.text()).unwrap();
    }

    /// Records and renders the output that is ready; false once the PTY
    /// reports end of file.
    pub(crate) fn pump(&mut self) -> bool {
        let data = pty::read_some(self.master());
        if data.is_empty() {
            return false;
        }
        self.record.write_all(&data).and_then(|()| self.record.flush()).unwrap();
        self.screen.feed(&data);
        true
    }

    /// Runs `:command`, waits for `marker` and `Exit code CODE`, and returns
    /// to the resource view.
    pub(crate) fn command(&mut self, command: &str, marker: &str, code: i32) {
        self.send(format!(":{command}\r").as_bytes(), Some(marker));
        self.until(&format!("Exit code {code}"));
        self.send(b"\r", None);
    }

    /// Quits with `q` and requires a zero exit with the terminal settings
    /// restored; the TUI is killed if it outlives the check.
    pub(crate) fn close(&mut self) {
        if self.closed {
            return;
        }
        let master = self.master();
        finally(
            self,
            |terminal| {
                // An exited TUI no longer reads its input.
                if terminal.running() {
                    pty::write_all(master, b"q");
                }
                let deadline = Instant::now() + Duration::from_secs(10);
                while terminal.running() && Instant::now() < deadline {
                    if !readable(&[master], Duration::from_millis(100)).is_empty() {
                        let data = pty::read_some(master);
                        terminal.record.write_all(&data).unwrap();
                    }
                }
                let status =
                    wait_timeout(&mut terminal.child, Duration::from_secs(1)).expect("the TUI did not exit after q");
                assert!(status.success(), "the TUI exited with {status}");
                assert_eq!(termios::settings(master), terminal.before, "the TUI did not restore the terminal");
            },
            Terminal::end,
        );
    }

    /// Kills a still running TUI's session and reaps it.
    fn end(&mut self) {
        self.closed = true;
        if self.running() {
            pty::kill_group(self.child.id(), libc::SIGKILL);
        }
        let _ = wait_timeout(&mut self.child, Duration::from_secs(5));
        let _ = self.record.flush();
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if !self.closed {
            self.end();
        }
    }
}

impl Driver for Terminal {
    fn text(&self) -> String {
        self.screen.text()
    }

    fn wait_for(&mut self, predicate: &dyn Fn(&str) -> bool) {
        self.wait_within(predicate, Duration::from_secs(90));
    }

    fn write(&mut self, keys: &[u8]) {
        pty::write_all(self.master(), keys);
    }
}

/// Real TUI command output, interactive exec and detach, exit statuses and
/// terminal restoration; with `kubeconfig`, also the Kubernetes workspace's
/// query, exec, apply and port-forward against the disposable cluster.
pub(crate) fn exercise(live: &Live, kubeconfig: Option<&Path>) {
    let mut environment = live.runtime.environment.clone();
    if let Some(kubeconfig) = kubeconfig {
        environment.insert("KUBECONFIG".into(), super::path_str(kubeconfig).to_owned());
    }
    let mut terminal = Terminal::new(&live.runtime.binary, &environment, &live.root);
    finally(
        &mut terminal,
        |terminal| {
            if !live.runtime.home.join(".hamn/tui.json").exists() {
                terminal.until("Choose your default workspace");
                terminal.send(b"1\r", None);
            }
            terminal.until("hamn-workspace-sentinel");
            let direct = live.docker(&["ps", "--filter", "name=hamn-workspace-sentinel", "--format", "{{.Names}}"]);
            terminal.command("ps --filter name=hamn-workspace-sentinel --format '{{.Names}}'", direct.trim(), 0);
            terminal.until("hamn-workspace-sentinel");
            terminal.send(b":exec -it hamn-workspace-sentinel sh\r", Some("/ #"));
            terminal.send(b"printf 'PTY_INPUT_PROOF\\n'\r", Some("PTY_INPUT_PROOF"));
            // Docker 29 exec reports its escape-sequence exit as 1.
            terminal.send(b"\x10\x11", Some("Exit code 1"));
            terminal.send(b"\r", Some("hamn-workspace-sentinel"));
            terminal.command("exec hamn-workspace-sentinel sh -c 'exit 7'", "Exit code 7", 7);
            terminal.until("hamn-workspace-sentinel");
            if kubeconfig.is_some() {
                terminal.send(b"\t", Some("workspace-http"));
                assert!(!terminal.text().contains("Hamn profile"), "{}", terminal.text());
                terminal.command("get pods -n workspace-proof -o name", "pod/workspace-http", 0);
                terminal.until("workspace-http");
                terminal.send(b":exec -it -n workspace-proof workspace-http -- sh\r", Some("/ #"));
                terminal.send(b"printf 'KUBE_PTY_PROOF\\n'\r", Some("KUBE_PTY_PROOF"));
                terminal.send(b"exit\r", Some("Exit code 0"));
                terminal.send(b"\r", Some("workspace-http"));
                let configmap = live.root.join("configmap.json");
                terminal.command(&format!("apply -f {}", configmap.display()), "configmap/tui-proof created", 0);
                terminal.until("workspace-http");
                terminal.send(
                    b":port-forward -n workspace-proof pod/workspace-http 18089:8080\r",
                    Some("Forwarding from 127.0.0.1:18089"),
                );
                assert_eq!(http_get(18089).trim_ascii(), b"kube-http-proof");
                terminal.send(b"\x03", Some("Exit code"));
                terminal.send(b"\r", Some("workspace-http"));
            }
        },
        Terminal::close,
    );
    println!("PASS: real TUI command output, interactive exec/detach, exit status and restoration");
}

/// `GET /` from 127.0.0.1:`port` without any proxy, within 10 seconds per
/// step; returns the body of a 200 response.
pub(crate) fn http_get(port: u16) -> Vec<u8> {
    use std::io::Read;
    use std::net::{SocketAddr, TcpStream};
    let timeout = Duration::from_secs(10);
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream =
        TcpStream::connect_timeout(&address, timeout).unwrap_or_else(|error| panic!("connect {address}: {error}"));
    stream.set_read_timeout(Some(timeout)).unwrap();
    stream.set_write_timeout(Some(timeout)).unwrap();
    // HTTP/1.0: the server closes the connection and never chunks the body.
    stream.write_all(format!("GET / HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes()).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap_or_else(|error| panic!("read {address}: {error}"));
    let split = response.windows(4).position(|window| window == b"\r\n\r\n").expect("an HTTP response head");
    let status = String::from_utf8_lossy(&response[..split]).lines().next().unwrap_or_default().to_owned();
    assert!(status.split(' ').nth(1) == Some("200"), "{address}: {status}");
    response[split + 4..].to_vec()
}
