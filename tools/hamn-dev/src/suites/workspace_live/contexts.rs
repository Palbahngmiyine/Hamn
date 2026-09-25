//! Real Engine transports through explicit Docker contexts; no VM is
//! started and no user credential is used.
//!
//! TLS terminates at an owned loopback mTLS proxy, which forwards bytes to
//! the existing profile Unix socket. SSH reaches the owned VM's real Docker
//! CLI with a copied profile key and a private pinned-host-key
//! configuration. Neither path changes the daemon configuration, and
//! credentials exist only in a temporary tree inside the root. A decoy
//! engine stands behind `DOCKER_HOST` and `DOCKER_CONTEXT`, so a fallback
//! from the explicit context would be observed.
use super::processes::{Birth, birth, gone};
use super::{Live, Must, capture_env, panic_text, path_str, readable, write_json};
use crate::release::process::{self, Spec};
use crate::release::syntax::shell_quote;
use crate::support::harness_peers::Event;
use crate::support::tmp::TempDir;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A loopback transport the test owns, with witnesses for handshake
/// failures, a stall barrier and cleanup. Without a target it is a decoy
/// that answers 503; with a TLS configuration it requires a client
/// certificate before forwarding bytes to the target Unix socket.
pub(crate) struct Proxy {
    pub port: u16,
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
    closed: bool,
}

struct Shared {
    target: Option<PathBuf>,
    tls: Option<Arc<rustls::ServerConfig>>,
    closed: AtomicBool,
    stall: AtomicBool,
    entered: Event,
    release: Event,
    state: Mutex<State>,
    /// Signalled whenever one of the proxy's threads ends.
    ended: Condvar,
}

#[derive(Default)]
struct State {
    accepted: usize,
    forwarded: usize,
    tls_failures: usize,
    errors: Vec<String>,
    /// Open channels; each worker removes its own when it ends.
    channels: BTreeMap<u64, Channel>,
    next_channel: u64,
    accepting: bool,
    workers: usize,
}

enum Channel {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl Channel {
    fn shutdown(&self) {
        let _ = match self {
            Channel::Tcp(stream) => stream.shutdown(Shutdown::Both),
            Channel::Unix(stream) => stream.shutdown(Shutdown::Both),
        };
    }
}

/// Records the end of a proxy thread, even one ended by a panic.
struct ThreadEnd {
    shared: Arc<Shared>,
    worker: bool,
}

impl Drop for ThreadEnd {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.worker {
            state.workers -= 1;
        } else {
            state.accepting = false;
        }
        self.shared.ended.notify_all();
    }
}

impl Proxy {
    pub(crate) fn new(target: Option<&Path>, tls: Option<Arc<rustls::ServerConfig>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local address").port();
        let shared = Arc::new(Shared {
            target: target.map(Path::to_path_buf),
            tls,
            closed: AtomicBool::new(false),
            stall: AtomicBool::new(false),
            entered: Event::default(),
            release: Event::default(),
            state: Mutex::new(State { accepting: true, ..State::default() }),
            ended: Condvar::new(),
        });
        let accepting = Arc::clone(&shared);
        let accept = std::thread::spawn(move || {
            let _end = ThreadEnd { shared: Arc::clone(&accepting), worker: false };
            for connection in listener.incoming() {
                // `close` wakes this loop with a connection of its own.
                if accepting.closed.load(Ordering::SeqCst) {
                    return;
                }
                match connection {
                    Ok(connection) => spawn_worker(&accepting, connection),
                    Err(error) => {
                        accepting.state.lock().unwrap().errors.push(format!("accept: {error}"));
                        return;
                    }
                }
            }
        });
        Self { port, shared, accept: Some(accept), closed: false }
    }

    pub(crate) fn set_stall(&self, stall: bool) {
        self.shared.stall.store(stall, Ordering::SeqCst);
    }

    /// Whether a stalled request arrived, waiting up to `timeout`.
    pub(crate) fn entered(&self, timeout: Duration) -> bool {
        self.shared.entered.wait(timeout)
    }

    pub(crate) fn release(&self) {
        self.shared.release.set();
    }

    pub(crate) fn accepted(&self) -> usize {
        self.shared.state.lock().unwrap().accepted
    }

    pub(crate) fn forwarded(&self) -> usize {
        self.shared.state.lock().unwrap().forwarded
    }

    pub(crate) fn tls_failures(&self) -> usize {
        self.shared.state.lock().unwrap().tls_failures
    }

    fn stop(&self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.shared.release.set();
        let address = SocketAddr::from(([127, 0, 0, 1], self.port));
        let _ = TcpStream::connect_timeout(&address, Duration::from_secs(1));
    }

    fn shutdown_channels(&self) {
        let state = self.shared.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        for channel in state.channels.values() {
            channel.shutdown();
        }
    }

    fn wait_until(&self, done: impl Fn(&State) -> bool, timeout: Duration) -> bool {
        let state = self.shared.state.lock().unwrap();
        let (state, _) = self.shared.ended.wait_timeout_while(state, timeout, |state| !done(state)).unwrap();
        done(&state)
    }

    /// Stops accepting, shuts every open channel down and requires that all
    /// of the proxy's threads ended, each removed its channels, and no
    /// transfer failed unexpectedly.
    pub(crate) fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.stop();
        let accept_ended = self.wait_until(|state| !state.accepting, Duration::from_secs(3));
        self.shutdown_channels();
        let workers_ended = self.wait_until(|state| state.workers == 0, Duration::from_secs(6));
        if accept_ended && let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        let state = self.shared.state.lock().unwrap();
        assert!(accept_ended && workers_ended, "proxy threads survived close ({} workers)", state.workers);
        assert!(
            state.channels.is_empty() && state.errors.is_empty(),
            "{} channels left open; errors: {:?}",
            state.channels.len(),
            state.errors
        );
    }
}

impl Drop for Proxy {
    /// A proxy left open by a failed check still releases and shuts down
    /// its threads' channels, without judging them.
    fn drop(&mut self) {
        if !self.closed {
            self.closed = true;
            self.stop();
            self.shutdown_channels();
        }
    }
}

fn spawn_worker(shared: &Arc<Shared>, connection: TcpStream) {
    {
        let mut state = shared.state.lock().unwrap();
        state.accepted += 1;
        state.workers += 1;
    }
    let shared = Arc::clone(shared);
    std::thread::spawn(move || {
        let _end = ThreadEnd { shared: Arc::clone(&shared), worker: true };
        let mut opened = Vec::new();
        let result = handle(&shared, connection, &mut opened);
        let mut state = shared.state.lock().unwrap();
        if let Err(error) = result
            && !shared.closed.load(Ordering::SeqCst)
        {
            state.errors.push(error);
        }
        for channel in opened {
            state.channels.remove(&channel);
        }
    });
}

fn register(shared: &Shared, channel: Channel, opened: &mut Vec<u64>) {
    let mut state = shared.state.lock().unwrap();
    let id = state.next_channel;
    state.next_channel += 1;
    state.channels.insert(id, channel);
    opened.push(id);
}

/// Docker closes its transport once it has read the API response, so a
/// reset, a broken pipe or an end of file without TLS close_notify ends a
/// transfer normally.
fn ended(error: io::Error) -> Result<(), String> {
    match error.kind() {
        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::UnexpectedEof => Ok(()),
        _ => Err(error.to_string()),
    }
}

enum Client {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
}

impl Client {
    fn fd(&self) -> RawFd {
        match self {
            Client::Plain(stream) => stream.as_raw_fd(),
            Client::Tls(stream) => stream.sock.as_raw_fd(),
        }
    }

    /// Whether decrypted bytes wait in the TLS session (no socket event
    /// announces them).
    fn pending(&mut self) -> bool {
        match self {
            Client::Plain(_) => false,
            Client::Tls(stream) => {
                stream.conn.process_new_packets().map_or(true, |state| state.plaintext_bytes_to_read() > 0)
            }
        }
    }

    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Client::Plain(stream) => stream.read(buffer),
            Client::Tls(stream) => stream.read(buffer),
        }
    }

    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        match self {
            Client::Plain(stream) => stream.write_all(data),
            Client::Tls(stream) => stream.write_all(data).and_then(|()| stream.flush()),
        }
    }

    /// Ends this side of the transfer: TLS close_notify, then the socket's
    /// write side.
    fn shutdown_write(&mut self) -> io::Result<()> {
        match self {
            Client::Plain(stream) => stream.shutdown(Shutdown::Write),
            Client::Tls(stream) => {
                stream.conn.send_close_notify();
                stream.flush()?;
                stream.sock.shutdown(Shutdown::Write)
            }
        }
    }
}

fn handle(shared: &Shared, connection: TcpStream, opened: &mut Vec<u64>) -> Result<(), String> {
    let text = |error: io::Error| error.to_string();
    register(shared, Channel::Tcp(connection.try_clone().map_err(text)?), opened);
    connection.set_read_timeout(Some(Duration::from_secs(5))).map_err(text)?;
    connection.set_write_timeout(Some(Duration::from_secs(5))).map_err(text)?;
    let mut client = match &shared.tls {
        None => Client::Plain(connection),
        Some(config) => {
            let mut session = rustls::ServerConnection::new(Arc::clone(config)).map_err(|error| error.to_string())?;
            let mut socket = connection;
            while session.is_handshaking() {
                if let Err(error) = session.complete_io(&mut socket) {
                    return match error.kind() {
                        // A rejected certificate or an abandoned handshake.
                        ErrorKind::InvalidData | ErrorKind::UnexpectedEof => {
                            shared.state.lock().unwrap().tls_failures += 1;
                            Ok(())
                        }
                        _ => ended(error),
                    };
                }
            }
            Client::Tls(Box::new(rustls::StreamOwned::new(session, socket)))
        }
    };
    let Some(target) = &shared.target else {
        return client
            .write_all(b"HTTP/1.1 503 Decoy\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .or_else(ended);
    };
    if shared.stall.load(Ordering::SeqCst) {
        let mut request = [0u8; 65536];
        let count = client.read(&mut request).map_err(text)?;
        if count == 0 {
            return Err("stalled transport received no request".into());
        }
        shared.entered.set();
        if !shared.release.wait(Duration::from_secs(15)) {
            return Err("stalled fixture was not released".into());
        }
        return match client.shutdown_write() {
            Err(error) if error.kind() != ErrorKind::NotConnected => ended(error),
            _ => Ok(()),
        };
    }
    let backend = UnixStream::connect(target).map_err(|error| format!("{}: {error}", target.display()))?;
    backend.set_read_timeout(Some(Duration::from_secs(5))).map_err(text)?;
    backend.set_write_timeout(Some(Duration::from_secs(5))).map_err(text)?;
    register(shared, Channel::Unix(backend.try_clone().map_err(text)?), opened);
    shared.state.lock().unwrap().forwarded += 1;
    let result = relay(shared, &mut client, backend);
    if let Client::Tls(stream) = &mut client {
        stream.conn.send_close_notify();
        let _ = stream.flush();
    }
    result
}

/// Copies bytes both ways until either side ends, within 15 seconds.
fn relay(shared: &Shared, client: &mut Client, mut backend: UnixStream) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut buffer = vec![0u8; 65536];
    while !shared.closed.load(Ordering::SeqCst) {
        if Instant::now() >= deadline {
            return Err("proxy transfer exceeded fixture deadline".into());
        }
        let ready = if client.pending() {
            vec![client.fd()]
        } else {
            readable(&[client.fd(), backend.as_raw_fd()], Duration::from_millis(100))
        };
        for fd in ready {
            let from_client = fd == client.fd();
            let count = match if from_client { client.read(&mut buffer) } else { backend.read(&mut buffer) } {
                Ok(count) => count,
                Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => continue,
                Err(error) => return ended(error),
            };
            if count == 0 {
                return Ok(());
            }
            let written =
                if from_client { backend.write_all(&buffer[..count]) } else { client.write_all(&buffer[..count]) };
            if let Err(error) = written {
                return ended(error);
            }
        }
    }
    Ok(())
}

fn pem_chain(path: &Path) -> Vec<CertificateDer<'static>> {
    CertificateDer::pem_file_iter(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .map(|certificate| certificate.unwrap_or_else(|error| panic!("{}: {error}", path.display())))
        .collect()
}

fn pem_key(path: &Path) -> PrivateKeyDer<'static> {
    PrivateKeyDer::from_pem_file(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

pub(crate) fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The certificates in `root` named `authority`.pem, as trust anchors.
pub(crate) fn roots(path: &Path) -> Arc<rustls::RootCertStore> {
    let mut roots = rustls::RootCertStore::empty();
    for certificate in pem_chain(path) {
        roots.add(certificate).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }
    Arc::new(roots)
}

/// Creates, with the system OpenSSL, a CA and a wrong CA, a server
/// certificate for 127.0.0.1 and a client certificate, both from the CA,
/// in `directory` (keys mode 0600). Returns the server configuration: TLS
/// 1.2 or later, requiring a client certificate from the CA.
pub(crate) fn certificates(directory: &Path) -> Arc<rustls::ServerConfig> {
    let openssl = |args: &[&str]| {
        process::run(OsStr::new("/usr/bin/openssl"), args, &Spec::default(), Duration::from_secs(20)).must();
    };
    let file = |name: &str| path_str(directory).to_owned() + "/" + name;
    fs::write(
        directory.join("ca.conf"),
        "[req]\ndistinguished_name=dn\nx509_extensions=ca\n[dn]\n[ca]\n\
         basicConstraints=critical,CA:true\nkeyUsage=critical,keyCertSign,cRLSign\n\
         subjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid:always\n",
    )
    .unwrap();
    for authority in ["ca", "wrong-ca"] {
        openssl(&[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-config",
            &file("ca.conf"),
            "-subj",
            &format!("/CN=hamn-owned-{authority}"),
            "-keyout",
            &file(&format!("{authority}.key")),
            "-out",
            &file(&format!("{authority}.pem")),
        ]);
    }
    for (name, usage) in [("server", "serverAuth"), ("client", "clientAuth")] {
        openssl(&[
            "req",
            "-new",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-subj",
            &format!("/CN=hamn-owned-{name}"),
            "-keyout",
            &file(&format!("{name}.key")),
            "-out",
            &file(&format!("{name}.csr")),
        ]);
        fs::write(
            directory.join(format!("{name}.ext")),
            format!(
                "basicConstraints=critical,CA:false\nkeyUsage=critical,digitalSignature,keyEncipherment\n\
                 subjectKeyIdentifier=hash\nauthorityKeyIdentifier=keyid,issuer\n\
                 extendedKeyUsage={usage}\nsubjectAltName=IP:127.0.0.1\n"
            ),
        )
        .unwrap();
        openssl(&[
            "x509",
            "-req",
            "-in",
            &file(&format!("{name}.csr")),
            "-CA",
            &file("ca.pem"),
            "-CAkey",
            &file("ca.key"),
            "-CAcreateserial",
            "-days",
            "1",
            "-extfile",
            &file(&format!("{name}.ext")),
            "-out",
            &file(&format!("{name}.pem")),
        ]);
    }
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.extension() == Some(OsStr::new("key")) {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    let verifier =
        rustls::server::WebPkiClientVerifier::builder_with_provider(roots(&directory.join("ca.pem")), provider())
            .build()
            .expect("client verifier");
    let config = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .with_client_cert_verifier(verifier)
        .with_single_cert(pem_chain(&directory.join("server.pem")), pem_key(&directory.join("server.key")))
        .expect("server certificate");
    Arc::new(config)
}

/// The fixture a [`wrapper`] runs before the real program.
pub const WITNESS_FIXTURE: &str = "workspace-live-exec-witness";

/// Writes an executable `path` that records the birth of the process it
/// becomes in `witness`, then execs `executable ARGUMENTS... "$@"`, so the
/// real program keeps its transport. Only births are recorded: never key
/// contents, environment or arguments.
pub(crate) fn wrapper(path: &Path, executable: &Path, arguments: &[&str], witness: &Path) {
    let name = path.file_name().expect("wrapper name").to_string_lossy();
    let link = path.with_file_name(format!(".{name}-exec-witness"));
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(std::env::current_exe().expect("current executable"), &link)
        .unwrap_or_else(|error| panic!("{}: {error}", link.display()));
    let mut words =
        vec![shell_quote(path_str(&link)), shell_quote(path_str(witness)), shell_quote(path_str(executable))];
    words.extend(arguments.iter().map(|argument| shell_quote(argument)));
    let script = format!("#!/bin/sh\nHAMN_DEV_FIXTURE={WITNESS_FIXTURE} exec {} \"$@\"\n", words.join(" "));
    fs::write(path, script).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

/// `WITNESS PROGRAM [ARG...]`: appends this process's birth to WITNESS as
/// one JSON line, then becomes PROGRAM. Exec keeps the PID and start time,
/// so the birth names the real program's process.
pub fn exec_witness(_program: &str, args: &[String]) -> ExitCode {
    let [witness, program, arguments @ ..] = args else {
        eprintln!("usage: {WITNESS_FIXTURE} WITNESS PROGRAM [ARG...]");
        return ExitCode::from(2);
    };
    let own = birth(std::process::id() as i32).expect("this process");
    let line = format!("{}\n", serde_json::to_string(&own).expect("serializable birth"));
    let recorded =
        OpenOptions::new().create(true).append(true).open(witness).and_then(|mut log| log.write_all(line.as_bytes()));
    if let Err(error) = recorded {
        eprintln!("{witness}: {error}");
        return ExitCode::FAILURE;
    }
    let error = Command::new(program).args(arguments).env_remove("HAMN_DEV_FIXTURE").exec();
    eprintln!("exec {program}: {error}");
    ExitCode::from(127)
}

/// The births a wrapper recorded.
pub(crate) fn births(witness: &Path) -> Vec<Birth> {
    let text = fs::read_to_string(witness).unwrap_or_else(|error| panic!("{}: {error}", witness.display()));
    text.lines().map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}: {line}"))).collect()
}

/// Explicit mTLS and SSH contexts reach the real engine; wrong CAs, missing
/// or wrong client credentials and unpinned hosts fail; nothing falls back
/// to the decoy; a stalled engine times out and every transport process the
/// test caused is gone. The caller owns a running frozen `verify` profile
/// with stable test containers.
pub(crate) fn external_contexts(live: &Live) {
    live.assert_owned();
    let profile = live.profile();
    let vm_pid = live.vm_pid();
    let socket = profile.join("docker.sock");
    assert!(fs::metadata(&socket).is_ok_and(|info| info.file_type().is_socket()), "no profile Docker socket");
    let binary_sha256 = crate::release::files::sha256_file(&live.runtime.binary).must();
    let evidence = RefCell::new(json!({"binarySha256": binary_sha256,
        "tlsBoundary": "owned loopback mTLS proxy -> existing profile Unix socket -> real guest Docker Engine",
        "sshBoundary": "real /usr/bin/ssh, copied owned profile key, private pinned host key -> real guest docker system dial-stdio",
        "sshPinOrigin": "server public key read through the existing owned profile SSH bootstrap; not an independent out-of-band authenticity claim",
        "cases": []}));
    let output = live.root.join("external-contexts-results.json");
    let temporary = TempDir::new_in(&live.root, "external-contexts-");
    let work = temporary.path();
    let (home, config, tools) = (work.join("home"), work.join("config"), work.join("tools"));
    for directory in [&home, &config, &tools] {
        fs::DirBuilder::new().mode(0o700).create(directory).unwrap();
    }
    let (docker_log, ssh_log) = (work.join("docker-processes.jsonl"), work.join("ssh-processes.jsonl"));
    wrapper(&tools.join("docker"), &live.runtime.docker, &[], &docker_log);
    let mut environment = live.runtime.environment.clone();
    let path = format!("{}:{}", path_str(&tools), environment["PATH"]);
    environment.insert("HOME".into(), path_str(&home).into());
    environment.insert("PATH".into(), path);
    environment.insert("DOCKER_CONFIG".into(), path_str(&work.join("wrong-config")).into());
    environment.remove("DOCKER_API_VERSION");
    let mut proxies = (Proxy::new(Some(&socket), Some(certificates(work))), Proxy::new(None, None));
    environment.insert("DOCKER_HOST".into(), format!("tcp://127.0.0.1:{}", proxies.1.port));
    environment.insert("DOCKER_CONTEXT".into(), "decoy".into());
    let config_text = path_str(&config).to_owned();

    let direct = |args: &[&str], success: bool| -> String {
        let mut all = vec!["--config", &config_text];
        all.extend(args);
        let output = capture_env(live.runtime.docker.as_os_str(), &all, &environment, Duration::from_secs(15));
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(output.status.success() == success, "{args:?}: {stdout} {}", output.stderr_lossy());
        stdout
    };
    let context = |name: &str, endpoint: &str| {
        direct(&["context", "create", name, "--docker", endpoint], true);
    };
    let ids_and_states = |text: &str| -> Vec<(String, String)> {
        let mut rows: Vec<(String, String)> = text
            .lines()
            .map(|line| {
                let row: Value = serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}: {line}"));
                (
                    row["ID"].as_str().unwrap_or_default().to_owned(),
                    row["State"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        rows.sort();
        rows
    };
    let invoke = |name: Option<&str>, success: bool, error: Option<&str>, timeout: u64| -> Value {
        let timeout_text = timeout.to_string();
        let mut args = vec!["--headless", "docker", "containers", "list", "--timeout", &timeout_text];
        args.extend(["--docker-config", &config_text]);
        if let Some(name) = name {
            args.extend(["--context", name]);
        }
        let result =
            capture_env(live.runtime.binary.as_os_str(), &args, &environment, Duration::from_secs(timeout + 5));
        let value: Value = serde_json::from_slice(&result.stdout)
            .unwrap_or_else(|problem| panic!("{problem}: {}", String::from_utf8_lossy(&result.stdout)));
        assert!(result.status.success() == success && value["ok"] == success, "{name:?}: {value}");
        let rows = if success {
            let target = &value["target"];
            assert!(target["context"].as_str() == name && target["dockerConfig"] == config_text.as_str(), "{value}");
            assert_eq!(target.get("profile"), Some(&Value::Null), "{value}");
            let mut rows: Vec<(String, String)> = value["data"]
                .as_array()
                .expect("container rows")
                .iter()
                .map(|item| {
                    (
                        item["Id"].as_str().unwrap_or_default().to_owned(),
                        item["State"].as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect();
            rows.sort();
            assert!(!rows.is_empty() && rows.iter().all(|(id, _)| super::is_lower_hex(id, 64)), "{rows:?}");
            let format = ["ps", "-a", "--no-trunc", "--format", "{{json .}}"];
            let expected =
                ids_and_states(&direct(&[&["--context", name.expect("a context")][..], &format].concat(), true));
            let socket_rows = ids_and_states(&live.docker(&format));
            assert!(rows == expected && expected == socket_rows, "{name:?}: {rows:?} {expected:?} {socket_rows:?}");
            json!(rows)
        } else {
            if let Some(error) = error {
                assert_eq!(value["error"]["code"], error, "{value}");
            }
            Value::Null
        };
        let failure = if success { Value::Null } else { value["error"].clone() };
        let mut record = evidence.borrow_mut();
        record["cases"].as_array_mut().expect("cases").push(json!({"context": name, "ok": success,
            "idsAndStates": rows, "error": failure}));
        write_json(&output, &record);
        value
    };

    let body = panic::catch_unwind(AssertUnwindSafe(|| {
        let (tls, decoy) = (&proxies.0, &proxies.1);
        context("decoy", &format!("host=tcp://127.0.0.1:{}", decoy.port));
        direct(&["context", "use", "decoy"], true);
        let endpoint = format!("host=tcp://127.0.0.1:{}", tls.port);
        let work_text = path_str(work);
        context(
            "tls",
            &format!("{endpoint},ca={work_text}/ca.pem,cert={work_text}/client.pem,key={work_text}/client.key"),
        );
        context(
            "tls-wrong-ca",
            &format!("{endpoint},ca={work_text}/wrong-ca.pem,cert={work_text}/client.pem,key={work_text}/client.key"),
        );
        context("tls-no-client", &format!("{endpoint},ca={work_text}/ca.pem"));
        invoke(Some("tls"), true, None, 10);
        let forwarded = tls.forwarded();
        for name in ["tls-wrong-ca", "tls-no-client"] {
            direct(&["--context", name, "ps"], false);
            invoke(Some(name), false, None, 10);
        }
        assert!(tls.forwarded() == forwarded && tls.tls_failures() >= 4, "rejected TLS reached the engine");
        invoke(Some("missing-context"), false, None, 10);
        invoke(None, false, Some("invalidRequest"), 10);
        assert_eq!(decoy.accepted(), 0, "explicit target fell back to poisoned/default endpoint");

        // A TLS-authenticated connection withholds the Engine response:
        // Hamn must return its own timeout and reap its CLI group.
        tls.set_stall(true);
        invoke(Some("tls"), false, Some("timeout"), 1);
        assert!(tls.entered(Duration::ZERO), "the stalled request never reached the proxy");
        gone(&births(&docker_log), Duration::from_secs(5)).must();
        tls.set_stall(false);
        tls.release();

        let status = live.call(&["vm", "status"], &[]);
        let ip = status["ip"].as_str().expect("VM status has no IP address");
        let server_key: Vec<String> =
            live.ssh("cat /etc/ssh/ssh_host_ed25519_key.pub").split_whitespace().map(str::to_owned).collect();
        assert!(server_key.len() >= 2 && server_key[0] == "ssh-ed25519", "{server_key:?}");
        fs::copy(profile.join("id_ed25519"), work.join("ssh-key")).unwrap();
        fs::set_permissions(work.join("ssh-key"), fs::Permissions::from_mode(0o600)).unwrap();
        let wrong_key = work.join("wrong-key");
        process::run(
            OsStr::new("/usr/bin/ssh-keygen"),
            &["-q", "-t", "ed25519", "-N", "", "-f", path_str(&wrong_key)],
            &Spec::default(),
            Duration::from_secs(10),
        )
        .must();
        let wrong: Vec<String> =
            fs::read_to_string(work.join("wrong-key.pub")).unwrap().split_whitespace().map(str::to_owned).collect();
        fs::write(work.join("known-hosts"), format!("hamn-owned-engine {} {}\n", server_key[0], server_key[1]))
            .unwrap();
        fs::write(work.join("wrong-hosts"), format!("hamn-owned-engine {} {}\n", wrong[0], wrong[1])).unwrap();
        let ssh_config = work.join("ssh-config");
        let hosts = [
            ("owned-engine", work.join("ssh-key"), work.join("known-hosts")),
            ("wrong-host", work.join("ssh-key"), work.join("wrong-hosts")),
            ("wrong-client", wrong_key.clone(), work.join("known-hosts")),
        ];
        let text: String = hosts
            .iter()
            .map(|(name, key, known)| {
                format!(
                    "Host {name}\n  HostName {ip}\n  User hamn\n  HostKeyAlias hamn-owned-engine\n\
                     \x20 IdentityFile \"{}\"\n  UserKnownHostsFile \"{}\"\n\
                     \x20 GlobalKnownHostsFile /dev/null\n  StrictHostKeyChecking yes\n  CheckHostIP no\n\
                     \x20 HostKeyAlgorithms ssh-ed25519\n  IdentitiesOnly yes\n  IdentityAgent none\n\
                     \x20 BatchMode yes\n  ConnectTimeout 5\n  ConnectionAttempts 1\n\
                     \x20 ControlMaster no\n  ControlPath none\n  LogLevel ERROR\n",
                    key.display(),
                    known.display()
                )
            })
            .collect();
        fs::write(&ssh_config, text).unwrap();
        fs::set_permissions(&ssh_config, fs::Permissions::from_mode(0o600)).unwrap();
        wrapper(&tools.join("ssh"), Path::new("/usr/bin/ssh"), &["-F", path_str(&ssh_config)], &ssh_log);
        for (name, _, _) in &hosts {
            context(name, &format!("host=ssh://{name}"));
        }
        invoke(Some("owned-engine"), true, None, 10);
        for name in ["wrong-host", "wrong-client"] {
            direct(&["--context", name, "ps"], false);
            invoke(Some(name), false, None, 10);
        }
        assert!(ssh_log.exists(), "Docker did not execute the real SSH wrapper");
        let mut transports = births(&docker_log);
        transports.extend(births(&ssh_log));
        gone(&transports, Duration::from_secs(5)).must();
        assert_eq!(decoy.accepted(), 0, "an explicit context fell back to the decoy");
        assert!(!home.join(".hamn").exists(), "external Docker operation touched managed VM state");
        assert_eq!(live.vm_pid(), vm_pid, "external Docker operation replaced the managed VM");
        let mut record = evidence.borrow_mut();
        let object = record.as_object_mut().expect("evidence object");
        object.insert("tlsHandshakeFailures".into(), json!(tls.tls_failures()));
        object.insert("decoyConnections".into(), json!(decoy.accepted()));
        object.insert("ownedTransportProcessesReaped".into(), json!(true));
        object.insert("clientHamnDirectoryCreated".into(), json!(false));
        object.insert("managedVmPidUnchanged".into(), json!(true));
    }));
    // Both proxies are closed and the evidence written whatever failed; the
    // first failure is the one reported.
    let closes = [
        panic::catch_unwind(AssertUnwindSafe(|| proxies.0.close())),
        panic::catch_unwind(AssertUnwindSafe(|| proxies.1.close())),
    ];
    write_json(&output, &evidence.borrow());
    if let Err(payload) = body {
        panic::resume_unwind(payload);
    }
    for close in closes {
        if let Err(payload) = close {
            panic!("proxy cleanup failed: {}", panic_text(&*payload));
        }
    }
    println!("PASS: real Engine via explicit mTLS/SSH contexts, failed authentication, no fallback and owned cleanup");
}
