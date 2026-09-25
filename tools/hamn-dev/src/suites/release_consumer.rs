//! Consumes unchanged publisher outputs through an isolated HTTPS fixture.
//!
//! The published v3 manifest's artifact URLs point at github.com. A local
//! CONNECT proxy terminates TLS for github.com with a throwaway CA that only
//! the test children trust, and serves exactly the declared artifact paths
//! from the candidate directory; it never forwards to the network.
//! Production curl, TLS hostname verification, the manifest and the
//! installer are unchanged.
//!
//! Inputs (environment): `HAMN_SOURCE_ROOT` (checkout with
//! scripts/update-host.sh), `HAMN_PUBLISHED_DIR`, `HAMN_CANDIDATE_DIR` and
//! `HAMN_CONSUMER_WORK` (created here; must not exist).
use crate::release::files::sha256_file;
use crate::release::process::{self, Spec};
use crate::runner::{self, case};
use crate::support::http::{Options, Reply, Request, Response, Server, Stream};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    runner::run(
        "release-publisher-consumer",
        "exact published v3 manifest consumed by native HTTPS; repeats and the unchanged cached installer need no network",
        vec![case("published_manifest_is_consumed_unchanged", published_manifest_is_consumed_unchanged)],
        filters,
    )
}

fn input(name: &str) -> PathBuf {
    let value = std::env::var_os(name).unwrap_or_else(|| panic!("{name} is required"));
    std::path::absolute(PathBuf::from(value)).unwrap()
}

#[derive(Default)]
struct Log {
    requests: Vec<String>,
    failures: Vec<String>,
}

/// github.com CA certificate and key, made by the system OpenSSL.
fn certificate(work: &Path) -> (PathBuf, PathBuf) {
    let config = work.join("tls.conf");
    fs::write(
        &config,
        "[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=ext\n\
         [dn]\nCN=github.com\n[ext]\nsubjectAltName=DNS:github.com\n\
         basicConstraints=critical,CA:TRUE\nkeyUsage=critical,digitalSignature,keyEncipherment,keyCertSign\n\
         extendedKeyUsage=serverAuth\n",
    )
    .unwrap();
    let (cert, key) = (work.join("cert.pem"), work.join("key.pem"));
    let args: Vec<OsString> = vec![
        "req".into(),
        "-x509".into(),
        "-newkey".into(),
        "rsa:2048".into(),
        "-nodes".into(),
        "-days".into(),
        "1".into(),
        "-config".into(),
        config.into(),
        "-keyout".into(),
        key.clone().into(),
        "-out".into(),
        cert.clone().into(),
    ];
    process::run("/usr/bin/openssl".as_ref(), &args, &Spec::default(), Duration::from_secs(30)).unwrap();
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).unwrap();
    (cert, key)
}

fn tls_config(cert: &Path, key: &Path) -> Arc<rustls::ServerConfig> {
    let certificates: Vec<CertificateDer<'static>> =
        CertificateDer::pem_file_iter(cert).unwrap().map(Result::unwrap).collect();
    let key = PrivateKeyDer::from_pem_file(key).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .unwrap();
    Arc::new(config)
}

/// Reads one request head (through the blank line) and returns its target.
fn request_target(stream: &mut dyn Read) -> std::io::Result<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 64 * 1024 || stream.read(&mut byte)? == 0 {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    let mut words = head.lines().next().unwrap_or("").split(' ');
    match (words.next(), words.next()) {
        (Some("GET"), Some(target)) => Ok(target.to_owned()),
        _ => Err(std::io::ErrorKind::InvalidData.into()),
    }
}

/// The proxied connection as a sized transport for rustls.
struct Socket<'a>(&'a mut dyn Stream);

impl Read for Socket<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buffer)
    }
}

impl Write for Socket<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.write(data)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

/// Serves one GET inside the tunnel; only declared artifact paths succeed.
fn tunnel(
    stream: &mut dyn Stream,
    config: Arc<rustls::ServerConfig>,
    allowed: &BTreeMap<String, PathBuf>,
    log: &Mutex<Log>,
) -> std::io::Result<()> {
    stream.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")?;
    stream.flush()?;
    let mut connection = rustls::ServerConnection::new(config).map_err(std::io::Error::other)?;
    let mut socket = Socket(stream);
    let mut tls = rustls::Stream::new(&mut connection, &mut socket);
    let target = request_target(&mut tls)?;
    let Some(payload) = allowed.get(&target) else {
        log.lock().unwrap().failures.push(target);
        tls.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
        return tls.flush();
    };
    log.lock().unwrap().requests.push(target);
    let mut file = fs::File::open(payload)?;
    let length = file.metadata()?.len();
    tls.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n").as_bytes())?;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        tls.write_all(&buffer[..count])?;
    }
    tls.flush()?;
    tls.conn.send_close_notify();
    tls.flush()
}

fn published_manifest_is_consumed_unchanged() {
    let root = input("HAMN_SOURCE_ROOT");
    let published = input("HAMN_PUBLISHED_DIR");
    let candidate = input("HAMN_CANDIDATE_DIR");
    let work = input("HAMN_CONSUMER_WORK");
    fs::DirBuilder::new().mode(0o700).create(&work).unwrap();
    let manifest_path = published.join("hamn-update-manifest-v3.json");
    let manifest_hash = sha256_file(&manifest_path).unwrap();
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let mut allowed = BTreeMap::new();
    for artifact in manifest["artifacts"].as_object().unwrap().values() {
        let url = artifact["url"].as_str().unwrap();
        let path = url.strip_prefix("https://github.com").expect("artifact URL is https://github.com/...");
        assert!(path.starts_with('/') && !path.contains(['?', '#']), "{url}");
        let payload = candidate.join(path.rsplit('/').next().unwrap());
        assert!(payload.is_file() && sha256_file(&payload).unwrap() == artifact["sha256"].as_str().unwrap(), "{url}");
        allowed.insert(path.to_owned(), payload);
    }
    let expected_paths: BTreeSet<String> = allowed.keys().cloned().collect();
    let (cert, key) = certificate(&work);
    let config = tls_config(&cert, &key);
    let log = Arc::new(Mutex::new(Log::default()));
    let server = {
        let (allowed, log) = (Arc::new(allowed), Arc::clone(&log));
        Server::tcp(Options::default(), move |request: &Request| {
            if request.method != "CONNECT" || request.target != "github.com:443" {
                log.lock().unwrap().failures.push(format!("{} {}", request.method, request.target));
                return Response::new(403, "").into();
            }
            let (config, allowed, log) = (Arc::clone(&config), Arc::clone(&allowed), Arc::clone(&log));
            Reply::Raw(Box::new(move |stream: &mut dyn Stream| {
                if let Err(error) = tunnel(stream, config, &allowed, &log) {
                    log.lock().unwrap().failures.push(error.to_string());
                }
            }))
        })
    };
    let proxy = server.url("http");
    let mut environment: BTreeMap<String, String> = std::env::vars().collect();
    for (name, value) in [
        ("HTTPS_PROXY", proxy.as_str()),
        ("https_proxy", proxy.as_str()),
        ("NO_PROXY", ""),
        ("no_proxy", ""),
        ("CURL_CA_BUNDLE", cert.to_str().unwrap()),
        ("SSL_CERT_FILE", cert.to_str().unwrap()),
        ("HAMN_NO_UPDATE_CHECK", "1"),
        ("HAMN_UPDATE_ALLOW_LOCAL_ARTIFACTS", "1"),
    ] {
        environment.insert(name.into(), value.into());
    }
    let command = |home: &Path, program: &Path, args: &[OsString]| -> String {
        let mut environment = environment.clone();
        environment.insert("HOME".into(), home.to_string_lossy().into_owned());
        let spec = Spec { environment: Some(&environment), input: None };
        process::run(program.as_os_str(), args, &spec, Duration::from_secs(90))
            .unwrap_or_else(|error| panic!("{program:?} {args:?}: {error}"))
    };
    let requests = || log.lock().unwrap().requests.clone();

    let home = work.join("home-v3");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let before = requests().len();
    let bootstrap: Vec<OsString> = vec![
        root.join("scripts/update-host.sh").into(),
        "--bootstrap".into(),
        "--output-json".into(),
        "--bindir".into(),
        home.join("bin").into(),
        "--datadir".into(),
        home.join("src").into(),
        "--manifest".into(),
        manifest_path.clone().into(),
    ];
    let result: Value = serde_json::from_str(&command(&home, Path::new("/bin/bash"), &bootstrap)).unwrap();
    assert!(result["completed"] == Value::Bool(true) && result["latestVersion"] == "0.0.1", "{result}");
    assert_eq!(result["profileDisksChanged"], Value::Bool(false), "{result}");
    let fetched = requests()[before..].to_vec();
    assert_eq!(fetched.len(), 2, "{fetched:?}");
    assert_eq!(fetched.into_iter().collect::<BTreeSet<_>>(), expected_paths);
    let count = requests().len();
    let repeat: Value = serde_json::from_str(&command(&home, Path::new("/bin/bash"), &bootstrap)).unwrap();
    assert_eq!(repeat["status"], "up-to-date", "{repeat}");
    assert_eq!(requests().len(), count, "a healthy repeat made a payload request");

    // The installer clears proxy and CA variables. Acquire the exact
    // published bytes through native TLS first, then prove the unchanged
    // installer consumes them with networking denied by the OS.
    let home = work.join("home-installer");
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    fs::DirBuilder::new().mode(0o700).create(home.join(".hamn")).unwrap();
    let cache = home.join(".hamn/cache");
    fs::DirBuilder::new().mode(0o755).create(&cache).unwrap();
    let before = requests().len();
    let native = work.join("home-v3/bin/hamn");
    for name in ["host", "guestImage"] {
        let args: Vec<OsString> = vec![
            "__install-support".into(),
            "upgrade".into(),
            "acquire".into(),
            manifest_path.clone().into(),
            name.into(),
            cache.clone().into(),
            work.join(format!("{name}-counts.json")).into(),
        ];
        command(&home, &native, &args);
    }
    let fetched = requests()[before..].to_vec();
    assert_eq!(fetched.len(), 2, "{fetched:?}");
    assert_eq!(fetched.into_iter().collect::<BTreeSet<_>>(), expected_paths);
    let before = requests().len();
    let sandboxed: Vec<OsString> = vec![
        "-p".into(),
        "(version 1)(allow default)(deny network*)".into(),
        "/bin/bash".into(),
        candidate.join("install.sh").into(),
    ];
    command(&home, Path::new("/usr/bin/sandbox-exec"), &sandboxed);
    let version = command(&home, &home.join(".local/bin/hamn"), &["--version".into()]);
    assert_eq!(version.trim(), "hamn 0.0.1");
    assert_eq!(requests().len(), before, "the cached installer used the network");
    drop(server);
    let failures = log.lock().unwrap().failures.clone();
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(sha256_file(&manifest_path).unwrap(), manifest_hash, "the published manifest changed");
}
