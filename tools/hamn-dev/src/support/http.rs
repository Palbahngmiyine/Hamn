//! Loopback HTTP fixtures over TCP, TLS or a Unix socket. Each connection
//! runs on its own thread (like Python's ThreadingHTTPServer), and a
//! response closes the connection unless the server keeps connections alive.
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// The request target, for example `/v1.47/containers/json?all=1`.
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }

    /// The target without its query string.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self { status, headers: Vec::new(), body: body.into() }
    }

    pub fn json(status: u16, value: &serde_json::Value) -> Self {
        Self::new(status, value.to_string()).header("Content-Type", "application/json")
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

/// What a handler does with a request: answer it, or take over the raw
/// connection (for streaming, stalling or closing without a response).
pub enum Reply {
    Response(Response),
    Raw(Box<dyn FnOnce(&mut dyn Stream) + Send>),
}

impl From<Response> for Reply {
    fn from(response: Response) -> Self {
        Reply::Response(response)
    }
}

pub trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

type Handler = dyn Fn(&Request) -> Reply + Send + Sync;

pub enum Address {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

pub struct Server {
    pub address: Address,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

pub struct Options {
    /// Serve further requests on a connection after a response (HTTP/1.1).
    pub keep_alive: bool,
    /// Serve TLS with this certificate and key (PEM files).
    pub tls: Option<(PathBuf, PathBuf)>,
}

impl Default for Options {
    fn default() -> Self {
        Self { keep_alive: false, tls: None }
    }
}

impl Server {
    /// Serves `handler` on 127.0.0.1 at an ephemeral port.
    pub fn tcp(options: Options, handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let tls = options.tls.as_ref().map(|(cert, key)| tls_config(cert, key));
        let keep_alive = options.keep_alive;
        Self::start(Address::Tcp(address), Arc::new(handler), move |stopped, handler| loop {
            if stopped.load(Ordering::SeqCst) {
                return;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    let handler = Arc::clone(&handler);
                    let tls = tls.clone();
                    std::thread::spawn(move || match tls {
                        Some(config) => {
                            let connection = rustls::ServerConnection::new(config).unwrap();
                            let mut stream = rustls::StreamOwned::new(connection, stream);
                            serve(&mut stream, &*handler, keep_alive);
                        }
                        None => {
                            let mut stream = stream;
                            serve(&mut stream, &*handler, keep_alive);
                        }
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => panic!("accept: {error}"),
            }
        })
    }

    /// Serves `handler` on a Unix socket at `path`.
    pub fn unix(path: &Path, options: Options, handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let listener = UnixListener::bind(path).unwrap_or_else(|error| panic!("bind {}: {error}", path.display()));
        listener.set_nonblocking(true).unwrap();
        let keep_alive = options.keep_alive;
        Self::start(Address::Unix(path.to_path_buf()), Arc::new(handler), move |stopped, handler| loop {
            if stopped.load(Ordering::SeqCst) {
                return;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    let handler = Arc::clone(&handler);
                    std::thread::spawn(move || {
                        let mut stream: UnixStream = stream;
                        serve(&mut stream, &*handler, keep_alive);
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => panic!("accept: {error}"),
            }
        })
    }

    fn start(
        address: Address,
        handler: Arc<Handler>,
        accept: impl FnOnce(Arc<AtomicBool>, Arc<Handler>) + Send + 'static,
    ) -> Self {
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stopped);
        let thread = std::thread::spawn(move || accept(flag, handler));
        Self { address, stopped, thread: Some(thread) }
    }

    pub fn port(&self) -> u16 {
        match &self.address {
            Address::Tcp(address) => address.port(),
            Address::Unix(_) => panic!("a Unix socket server has no port"),
        }
    }

    /// `scheme://127.0.0.1:PORT`
    pub fn url(&self, scheme: &str) -> String {
        format!("{scheme}://127.0.0.1:{}", self.port())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        if let Address::Unix(path) = &self.address {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn serve(stream: &mut dyn Stream, handler: &Handler, keep_alive: bool) {
    loop {
        let request = match read_request(stream) {
            Ok(Some(request)) => request,
            Ok(None) | Err(_) => return,
        };
        match handler(&request) {
            Reply::Response(response) => {
                if write_response(stream, &response, keep_alive).is_err() || !keep_alive {
                    return;
                }
            }
            Reply::Raw(take_over) => {
                take_over(stream);
                return;
            }
        }
    }
}

/// Reads one request; `None` when the peer closed before a request line.
fn read_request(stream: &mut dyn Stream) -> io::Result<Option<Request>> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.trim_end().splitn(3, ' ');
    let method = parts.next().unwrap_or("").to_owned();
    let target = parts.next().unwrap_or("").to_owned();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_owned(), value.trim().to_owned()));
        }
    }
    let chunked = headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("transfer-encoding") && value.eq_ignore_ascii_case("chunked"));
    let body = if chunked {
        read_chunked(&mut reader)?
    } else {
        let length = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        body
    };
    // A buffered read past this request's body would belong to the next
    // request; clients here send one request at a time.
    Ok(Some(Request { method, target, headers, body }))
}

/// Decodes a chunked request body (Go's HTTP client, and so kubectl, sends
/// streamed bodies this way): chunks of `HEX-SIZE\r\nDATA\r\n`, then
/// `0\r\n\r\n`. Chunk extensions, trailers and any other framing are
/// errors, which close the connection without a response.
fn read_chunked(reader: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, format!("chunked body: {what}"));
    let mut body = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let size = usize::from_str_radix(line.trim(), 16).map_err(|_| invalid("chunk size"))?;
        if size == 0 {
            let mut end = String::new();
            reader.read_line(&mut end)?;
            return if end == "\r\n" { Ok(body) } else { Err(invalid("trailer")) };
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader.read_exact(&mut body[start..])?;
        let mut end = [0; 2];
        reader.read_exact(&mut end)?;
        if end != *b"\r\n" {
            return Err(invalid("chunk end"));
        }
    }
}

fn write_response(stream: &mut dyn Stream, response: &Response, keep_alive: bool) -> io::Result<()> {
    let mut head = format!("HTTP/1.1 {} {}\r\n", response.status, reason(response.status));
    let mut has_length = false;
    for (name, value) in &response.headers {
        has_length |= name.eq_ignore_ascii_case("content-length") || name.eq_ignore_ascii_case("transfer-encoding");
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if !has_length {
        head.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    }
    if !keep_alive {
        head.push_str("Connection: close\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        206 => "Partial Content",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        416 => "Range Not Satisfiable",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

fn tls_config(cert: &Path, key: &Path) -> Arc<rustls::ServerConfig> {
    let certificates: Vec<CertificateDer<'static>> =
        CertificateDer::pem_file_iter(cert).unwrap().map(|certificate| certificate.unwrap()).collect();
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

/// A self-signed CA certificate for 127.0.0.1 (and CN localhost), made by
/// the system OpenSSL. Returns the certificate and key paths in `root`.
pub fn certificate(root: &Path) -> (PathBuf, PathBuf) {
    let config = root.join("tls.conf");
    std::fs::write(
        &config,
        "[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=ext\n\
         [dn]\nCN=localhost\n[ext]\nsubjectAltName=IP:127.0.0.1\n\
         basicConstraints=critical,CA:TRUE\nkeyUsage=critical,digitalSignature,keyEncipherment,keyCertSign\n\
         extendedKeyUsage=serverAuth\n",
    )
    .unwrap();
    let (cert, key) = (root.join("certificate.pem"), root.join("key.pem"));
    let output = Command::new("/usr/bin/openssl")
        .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-config"])
        .arg(&config)
        .arg("-keyout")
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .output()
        .expect("openssl");
    assert!(output.status.success(), "openssl req: {}", String::from_utf8_lossy(&output.stderr));
    std::fs::set_permissions(&key, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    (cert, key)
}

/// Connects to a TCP fixture, for tests that probe a server directly.
pub fn connect(port: u16) -> TcpStream {
    TcpStream::connect(("127.0.0.1", port)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::tmp::TempDir;

    fn curl(args: &[&str]) -> String {
        let output = Command::new("/usr/bin/curl").args(["-sS", "--max-time", "10"]).args(args).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    }

    fn echo(request: &Request) -> Reply {
        Response::new(200, format!("{} {} {}", request.method, request.target, String::from_utf8_lossy(&request.body))).into()
    }

    #[test]
    fn tcp_tls_and_unix_servers_answer_requests() {
        let server = Server::tcp(Options::default(), echo);
        assert_eq!(curl(&["-d", "body", &format!("{}/a?b=1", server.url("http"))]), "POST /a?b=1 body");

        let directory = TempDir::new("hamn-dev-http-");
        let (cert, key) = certificate(directory.path());
        let tls = Server::tcp(Options { tls: Some((cert.clone(), key)), ..Options::default() }, echo);
        assert_eq!(curl(&["--cacert", cert.to_str().unwrap(), &format!("{}/secure", tls.url("https"))]), "GET /secure ");

        let socket = directory.path().join("engine.sock");
        let unix = Server::unix(&socket, Options { keep_alive: true, tls: None }, echo);
        assert_eq!(curl(&["--unix-socket", socket.to_str().unwrap(), "http://localhost/_ping"]), "GET /_ping ");
        drop(unix);
        assert!(!socket.exists());
    }

    /// Sends a raw request, ends the upload, and returns the whole reply
    /// ("" when the server closes without one).
    fn exchange(port: u16, request: &[u8]) -> String {
        let mut stream = connect(port);
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        stream.write_all(request).unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        reply
    }

    #[test]
    fn chunked_request_bodies_are_decoded_and_bad_framing_gets_no_response() {
        let server = Server::tcp(Options::default(), echo);
        let head = "DELETE /pods/victim HTTP/1.1\r\nHost: fixture\r\nTransfer-Encoding: chunked\r\n\r\n";
        let reply = exchange(server.port(), format!("{head}4\r\n{{\"a\"\r\nB\r\n:\"chunked\"}}\r\n0\r\n\r\n").as_bytes());
        assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"), "{reply}");
        assert!(reply.ends_with("\r\n\r\nDELETE /pods/victim {\"a\":\"chunked\"}"), "{reply}");
        for framing in ["4\r\nabcdXX0\r\n\r\n", "0\r\nTrailer: x\r\n\r\n", "z\r\n\r\n", "4;ext=1\r\nabcd\r\n0\r\n\r\n", "4\r\nab"] {
            assert_eq!(exchange(server.port(), format!("{head}{framing}").as_bytes()), "", "{framing:?}");
        }
    }
}
