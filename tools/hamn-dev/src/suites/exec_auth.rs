//! Real kubeconfig exec plugins: credentials reach the API server and never
//! the output, plugins run noninteractively without stdin, interactive-only
//! plugins are refused, and a plugin is reaped at the deadline and on
//! cancellation. The plugin is this executable (fixture `exec-auth`); each
//! step selects its behavior, as the script rewrote its shell plugin.
use crate::runner::{self, case};
use crate::support::api_fixtures::{self, Completed, MkdTemp, py_json, respond, truthy, utf8};
use crate::support::http::{Options, Reply, Server};
use crate::support::tui::{install_fixture, select_fixture};
use crate::support::{hamn, pty};
use serde_json::{Value, json};
use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "exec-auth",
        "Exec authentication credentials, noninteractive mode, timeout and cleanup",
        vec![case("exec_plugins_honor_deadlines_protect_credentials_and_avoid_stdin", exec_plugins)],
        filters,
    )
}

/// What the plugin does, one per shell plugin of the script.
#[derive(Clone, Copy)]
enum Plugin {
    /// Requires a non-terminal stdin and the kubeconfig's environment,
    /// saves `KUBERNETES_EXEC_INFO` to `info` and prints `credential`.
    Credential,
    /// Prints the secret to stderr and fails.
    Failing,
    /// Prints `credential`.
    Prints,
    /// Creates `pid`; it must never run.
    Touches,
    /// Writes its PID to `pid`, then becomes `sleep 60`.
    Sleeps,
    /// Writes its PID to the `ready` FIFO, then becomes `sleep 60`.
    SleepsAfterFifo,
}

impl Plugin {
    fn name(self) -> &'static str {
        match self {
            Plugin::Credential => "credential",
            Plugin::Failing => "failing",
            Plugin::Prints => "prints",
            Plugin::Touches => "touches",
            Plugin::Sleeps => "sleeps",
            Plugin::SleepsAfterFifo => "sleeps-after-fifo",
        }
    }
}

struct Fixture {
    root: PathBuf,
    config: PathBuf,
    kubeconfig: Value,
    binary: PathBuf,
}

impl Fixture {
    fn setup(&mut self, plugin: Plugin, credential: Option<&Value>, interactive: &str) {
        fs::write(self.root.join("plugin-behavior"), plugin.name()).unwrap();
        if let Some(credential) = credential {
            fs::write(self.root.join("credential"), py_json(credential)).unwrap();
        }
        self.kubeconfig["users"][0]["user"]["exec"]["interactiveMode"] = json!(interactive);
        fs::write(&self.config, py_json(&self.kubeconfig)).unwrap();
    }

    fn command(&self, timeout: &str) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .args(["--headless", "k8s", "pods", "list", "--context", "test", "--kubeconfig", utf8(&self.config), "--timeout", timeout])
            .env("HOME", &self.root)
            .env("FIXTURE_ROOT", &self.root);
        command
    }

    fn invoke(&self) -> (Completed, Value) {
        let before = fs::read(&self.config).unwrap();
        let result = api_fixtures::run(&mut self.command("2"), None, Duration::from_secs(7));
        assert_eq!(fs::read(&self.config).unwrap(), before);
        assert!(!format!("{}{}", result.stdout, result.stderr).contains("fixture-secret"), "{result:?}");
        let value = result.json();
        (result, value)
    }
}

fn exec_plugins() {
    let directory = MkdTemp::new_in(Path::new("/tmp"), "hamn-exec-auth-");
    let root = directory.path().to_path_buf();
    let (plugin, info, pid) = (root.join("plugin"), root.join("info"), root.join("pid"));
    install_fixture(&root, "plugin");
    select_fixture(&root, "plugin", "exec-auth");
    let headers = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
    let server = {
        let headers = Arc::clone(&headers);
        Server::tcp(Options::default(), move |request| {
            let (headers, method, authorization) =
                (Arc::clone(&headers), request.method.clone(), request.header("Authorization").map(str::to_owned));
            Reply::Raw(Box::new(move |stream| {
                // Python's http.server at HTTP/1.0: GET only, one request
                // per connection.
                if method != "GET" {
                    respond(stream, "HTTP/1.0 501 Not Implemented", &[("Connection", "close".into())], b"");
                    return;
                }
                headers.lock().unwrap().push(authorization);
                let body = br#"{"apiVersion":"v1","kind":"PodList","items":[]}"#;
                respond(stream, "HTTP/1.0 200 OK", &[("Content-Length", body.len().to_string())], body);
            }))
        })
    };
    let kubeconfig = json!({"apiVersion": "v1", "kind": "Config",
        "clusters": [{"name": "test", "cluster": {"server": format!("http://127.0.0.1:{}", server.port())}}],
        "contexts": [{"name": "test", "context": {"cluster": "test", "user": "test"}}],
        "users": [{"name": "test", "user": {"exec": {"apiVersion": "client.authentication.k8s.io/v1",
            "command": utf8(&plugin), "interactiveMode": "IfAvailable", "provideClusterInfo": true,
            "env": [{"name": "HAMN_AUTH_FIXTURE", "value": "present"}]}}}]});
    let mut fixture = Fixture { root: root.clone(), config: root.join("kubeconfig"), kubeconfig, binary: hamn() };
    let credential = json!({"apiVersion": "client.authentication.k8s.io/v1", "kind": "ExecCredential",
        "status": {"token": "fixture-secret"}});
    let bearer = || vec![Some("Bearer fixture-secret".to_owned())];

    fixture.setup(Plugin::Credential, Some(&credential), "IfAvailable");
    let (result, envelope) = fixture.invoke();
    assert!(result.success() && truthy(envelope.get("ok")), "{envelope}");
    assert_eq!(*headers.lock().unwrap(), bearer());
    let supplied = api_fixtures::parse(&fs::read_to_string(&info).unwrap());
    assert_eq!(supplied["spec"]["interactive"], json!(false), "{supplied}");
    assert!(supplied["spec"]["cluster"]["server"].as_str().is_some_and(|server| server.starts_with("http://127.0.0.1:")), "{supplied}");
    fixture.setup(Plugin::Failing, None, "IfAvailable");
    assert_eq!(fixture.invoke().1["error"]["code"], "authenticationFailed");
    for status in [
        json!({"token": "fixture-secret", "expirationTimestamp": "2000-01-01T00:00:00Z"}),
        json!({"clientCertificateData": "fixture-secret"}),
        json!({}),
    ] {
        let mut credential = credential.clone();
        credential["status"] = status;
        fixture.setup(Plugin::Prints, Some(&credential), "IfAvailable");
        assert_eq!(fixture.invoke().1["error"]["code"], "authenticationFailed");
    }
    fixture.setup(Plugin::Touches, None, "Always");
    assert_eq!(fixture.invoke().1["error"]["code"], "authenticationRequired");
    assert!(!pid.exists());

    fixture.setup(Plugin::Sleeps, None, "IfAvailable");
    let started = Instant::now();
    assert_eq!(fixture.invoke().1["error"]["code"], "timeout");
    assert!(started.elapsed() < Duration::from_secs(6), "{:?}", started.elapsed());
    let child_pid: i32 = fs::read_to_string(&pid).unwrap().trim().parse().unwrap();
    if alive(child_pid) {
        pty::kill_group(child_pid as u32, libc::SIGKILL);
        panic!("timed out plugin was not reaped");
    }

    let fifo = root.join("ready");
    make_fifo(&fifo);
    fixture.setup(Plugin::SleepsAfterFifo, None, "IfAvailable");
    let ready = open_nonblocking(&fifo);
    let mut command = fixture.command("30");
    command.stdin(Stdio::inherit()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut operation = Reaped(command.spawn().expect("spawn the cancelled operation"));
    assert!(!pty::readable(&[ready.as_raw_fd()], Duration::from_secs(5)).is_empty(), "plugin did not start");
    let data = pty::read_some(ready.as_raw_fd());
    let child_pid: i32 = String::from_utf8(data).unwrap().trim().parse().unwrap();
    pty::kill(operation.0.id(), libc::SIGTERM);
    let output = api_fixtures::communicate(&mut operation.0, Duration::from_secs(5)).expect("the operation ends within 5 seconds");
    let (output, error) = (api_fixtures::text(output.stdout), api_fixtures::text(output.stderr));
    assert_eq!(api_fixtures::parse(&output)["error"]["code"], "cancelled", "{output} {error}");
    assert!(!alive(child_pid), "cancelled plugin was not reaped");
    drop(ready);
    drop(operation);
    assert_eq!(*headers.lock().unwrap(), bearer());
    drop(server);
}

/// `os.kill(pid, 0)`: `false` only when no such process exists. Any other
/// error fails the case, as the script's uncaught `PermissionError` did.
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    let error = io::Error::last_os_error();
    assert_eq!(error.raw_os_error(), Some(libc::ESRCH), "kill({pid}, 0): {error}");
    false
}

fn make_fifo(path: &Path) {
    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is a valid C string.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo: {}", io::Error::last_os_error());
}

/// The FIFO's read end, opened `O_RDONLY | O_NONBLOCK` as the script did, so
/// the plugin's open for writing does not block.
fn open_nonblocking(path: &Path) -> OwnedFd {
    let name = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is a valid C string; open returns a new descriptor or -1.
    let fd = unsafe { libc::open(name.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
    assert!(fd >= 0, "open {}: {}", path.display(), io::Error::last_os_error());
    // SAFETY: fd is a new descriptor owned here.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

/// Kills and reaps a child still running when dropped (the script's
/// `finally`: kill, then wait).
struct Reaped(std::process::Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = api_fixtures::wait_until(&mut self.0, Instant::now() + Duration::from_secs(5));
    }
}

/// The exec plugin, with the behavior `$FIXTURE_ROOT/plugin-behavior` names.
/// A failed requirement exits 1, as `set -eu` shell plugins did.
pub fn fixture(_program: &str, _args: &[String]) -> ExitCode {
    let root = PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"));
    let behavior = fs::read_to_string(root.join("plugin-behavior")).expect("plugin behavior");
    let print_credential = || {
        let credential = fs::read(root.join("credential")).expect("credential");
        io::stdout().write_all(&credential).unwrap();
    };
    match behavior.as_str() {
        "credential" => {
            // SAFETY: isatty only inspects descriptor 0.
            if unsafe { libc::isatty(0) } == 1 {
                return ExitCode::FAILURE;
            }
            if std::env::var_os("HAMN_AUTH_FIXTURE").is_none_or(|value| value != "present") {
                return ExitCode::FAILURE;
            }
            let Some(info) = std::env::var_os("KUBERNETES_EXEC_INFO") else { return ExitCode::FAILURE };
            fs::write(root.join("info"), info.as_bytes()).unwrap();
            print_credential();
        }
        "failing" => {
            eprintln!("fixture-secret");
            return ExitCode::FAILURE;
        }
        "prints" => print_credential(),
        "touches" => {
            OpenOptions::new().create(true).append(true).open(root.join("pid")).unwrap();
        }
        "sleeps" | "sleeps-after-fifo" => {
            let target = root.join(if behavior == "sleeps" { "pid" } else { "ready" });
            let mut output = OpenOptions::new().write(true).create(true).truncate(true).open(target).unwrap();
            writeln!(output, "{}", std::process::id()).unwrap();
            drop(output);
            let error = Command::new("/bin/sleep").arg("60").exec();
            panic!("exec /bin/sleep: {error}");
        }
        other => panic!("unknown plugin behavior {other:?}"),
    }
    ExitCode::SUCCESS
}
