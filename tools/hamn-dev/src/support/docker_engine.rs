//! Disposable Docker Engine APIs on Unix sockets for suites that compare
//! Hamn with the installed Docker CLI. Each suite supplies its routes; this
//! module keeps the conventions of the Python handlers they replace: GET
//! and HEAD only (HEAD runs the GET route without the body), API version
//! 1.47, and Python's 404 page for unknown paths.
use super::http::{Options, Reply, Request, Response, Server};
use super::py_http;
use std::path::Path;

/// Serves `route` on a Unix socket at `socket`, one connection per request.
pub fn serve(socket: &Path, route: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Server {
    Server::unix(socket, Options::default(), move |request| {
        py_http::dispatch(request, &["GET", "HEAD"], || route(request))
    })
}

/// A 200 answer with `API-Version: 1.47`, the optional content type and the
/// body's length; HEAD receives the headers only.
pub fn reply(request: &Request, body: &[u8], content_type: Option<&str>) -> Reply {
    let mut response = Response::new(200, if request.method == "HEAD" { Vec::new() } else { body.to_vec() })
        .header("API-Version", "1.47");
    if let Some(content_type) = content_type {
        response = response.header("Content-Type", content_type);
    }
    response.header("Content-Length", &body.len().to_string()).into()
}

/// `self.send_error(404)`.
pub fn not_found(request: &Request) -> Reply {
    py_http::send_error(request, 404, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::tmp::TempDir;
    use std::process::Command;

    /// The complete raw response to `request`: HEAD must end after the head.
    fn raw(socket: &Path, request: &str) -> String {
        use std::io::{Read, Write};
        let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    /// `curl` over the socket: (status line and headers, body).
    fn curl(socket: &Path, args: &[&str]) -> (String, String) {
        let output = Command::new("/usr/bin/curl")
            .args(["-sS", "--max-time", "10", "-D", "-", "--unix-socket", socket.to_str().unwrap()])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let text = String::from_utf8(output.stdout).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        (head.to_owned(), body.to_owned())
    }

    #[test]
    fn head_omits_bodies_and_unknown_routes_and_methods_get_python_errors() {
        let directory = TempDir::new("hamn-dev-engine-");
        let socket = directory.path().join("engine.sock");
        let _server = serve(&socket, |request| {
            if request.path() == "/_ping" {
                reply(request, b"OK", Some("application/json"))
            } else {
                not_found(request)
            }
        });
        let (head, body) = curl(&socket, &["http://localhost/_ping"]);
        assert!(head.starts_with("HTTP/1.1 200 OK\r\n") && head.contains("API-Version: 1.47"), "{head}");
        assert!(head.contains("Content-Length: 2"), "{head}");
        assert_eq!(body, "OK");
        let head = raw(&socket, "HEAD /_ping HTTP/1.0\r\n\r\n");
        assert!(head.contains("Content-Length: 2") && head.contains("Content-Type: application/json"), "{head}");
        assert!(head.ends_with("\r\n\r\n"), "{head:?}");
        let (head, body) = curl(&socket, &["http://localhost/v1.47/info"]);
        assert!(head.starts_with("HTTP/1.0 404 Not Found\r\n"), "{head}");
        assert!(head.contains("Content-Type: text/html;charset=utf-8"), "{head}");
        assert!(body.contains("<p>Message: Not Found.</p>"), "{body}");
        let head = raw(&socket, "HEAD /v1.47/info HTTP/1.0\r\n\r\n");
        assert!(head.starts_with("HTTP/1.0 404 Not Found\r\n") && head.ends_with("\r\n\r\n"), "{head:?}");
        let (head, body) = curl(&socket, &["-X", "POST", "http://localhost/_ping"]);
        assert!(head.starts_with("HTTP/1.0 501 Unsupported method ('POST')\r\n"), "{head}");
        assert!(body.contains("Error code explanation: 501 - Server does not support this operation."), "{body}");
    }
}
