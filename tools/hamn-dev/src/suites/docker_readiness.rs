//! A forwarded socket can accept locally while guest access fails: Docker
//! readiness (build/tests/test_docker_readiness) requires an actual Engine
//! API `_ping` response, not only a connection.
use crate::runner::{self, Case, case};
use crate::support::bounded_process;
use crate::support::pty;
use crate::support::tmp::TempDir;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    let mut cases: Vec<Case> = Vec::new();
    let responses: [(&str, Option<Vec<u8>>, u16, bool); 5] = [
        ("OK", Some(b"OK".to_vec()), 200, true),
        ("wrong", Some(b"wrong".to_vec()), 200, false),
        ("OK", Some(b"OK".to_vec()), 403, false),
        ("no-response", None, 200, false),
        ("4096-byte-body", Some(vec![b'O'; 4096]), 200, false),
    ];
    for (label, body, status, ready) in responses {
        cases.push(case(format!("ping_response/{status}/{label}"), move || ping_response(body.as_deref(), status, ready)));
    }
    cases.push(case("no_socket_is_not_ready", no_socket_is_not_ready));
    runner::run("docker-readiness", "Docker readiness requires an actual Engine API response", cases, filters)
}

fn binary() -> PathBuf {
    let path = PathBuf::from("build/tests/test_docker_readiness");
    std::fs::canonicalize(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Serves one connection on `docker.sock`: reads a single request chunk (up
/// to 4096 bytes), answers with `status` and `body` unless `body` is `None`,
/// and closes. The client must report ready exactly when `ready`.
fn ping_response(body: Option<&[u8]>, status: u16, ready: bool) {
    let binary = binary();
    let directory = TempDir::new("hamn-ping-");
    let listener = UnixListener::bind(directory.path().join("docker.sock")).unwrap();
    // SAFETY: listen on a descriptor this listener owns only changes its
    // backlog, to the Python fixture's one.
    assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0, "listen: {}", std::io::Error::last_os_error());
    let requests = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let (finished, done) = mpsc::channel();
    let response = body.map(|body| {
        let mut response =
            format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes();
        response.extend_from_slice(body);
        response
    });
    let (received, failures) = (Arc::clone(&requests), Arc::clone(&errors));
    std::thread::spawn(move || {
        if let Err(error) = serve_once(&listener, response.as_deref(), &received) {
            failures.lock().unwrap().push(error);
        }
        drop(listener);
        let _ = finished.send(());
    });
    let result = bounded_process::output(Command::new(&binary).arg(directory.path()), Duration::from_secs(5));
    assert_eq!(result.status.code(), Some(if ready { 0 } else { 1 }), "{status} {body:?} {result:?}");
    let alive = done.recv_timeout(Duration::from_secs(6)).is_err();
    let requests = requests.lock().unwrap().clone();
    let errors = errors.lock().unwrap().clone();
    assert!(
        !alive && errors.is_empty(),
        "status={status} body_length={:?} requests={requests:?} errors={errors:?} client={result:?} alive={alive}",
        body.map(<[u8]>::len)
    );
    assert!(requests.first().is_some_and(|request| request.starts_with(b"GET /_ping HTTP/1.1\r\n")), "{requests:?}");
}

fn serve_once(listener: &UnixListener, response: Option<&[u8]>, requests: &Mutex<Vec<Vec<u8>>>) -> Result<(), String> {
    if pty::readable(&[listener.as_raw_fd()], Duration::from_secs(5)).is_empty() {
        return Err("no connection within 5 seconds".to_owned());
    }
    let (mut connection, _) = listener.accept().map_err(|error| format!("accept: {error}"))?;
    connection.set_read_timeout(Some(Duration::from_secs(5))).map_err(|error| error.to_string())?;
    connection.set_write_timeout(Some(Duration::from_secs(5))).map_err(|error| error.to_string())?;
    let mut buffer = vec![0u8; 4096];
    let count = connection.read(&mut buffer).map_err(|error| format!("read request: {error}"))?;
    requests.lock().unwrap().push(buffer[..count].to_vec());
    if let Some(response) = response {
        connection.write_all(response).map_err(|error| format!("write response: {error}"))?;
    }
    Ok(())
}

fn no_socket_is_not_ready() {
    let binary = binary();
    let directory = TempDir::new("hamn-ping-");
    let status = bounded_process::status(Command::new(&binary).arg(directory.path()), Duration::from_secs(5));
    assert_eq!(status.code(), Some(1), "{status:?}");
}
