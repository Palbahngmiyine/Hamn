//! The request-handling conventions of Python's `http.server`
//! `BaseHTTPRequestHandler`, which the ported API fixtures relied on: a
//! method without a `do_METHOD` gets 501, and `send_error` answers with
//! Python's HTML error page and closes the connection (HTTP/1.0).
use super::http::{Reply, Request};

/// Python 3.14's `http.server.DEFAULT_ERROR_MESSAGE`, filled in.
fn error_page(code: u16, message: &str, explain: &str) -> String {
    format!(
        "<!DOCTYPE HTML>\n<html lang=\"en\">\n    <head>\n        <meta charset=\"utf-8\">\n        \
         <style type=\"text/css\">\n            :root {{\n                color-scheme: light dark;\n            }}\n        \
         </style>\n        <title>Error response</title>\n    </head>\n    <body>\n        <h1>Error response</h1>\n        \
         <p>Error code: {code}</p>\n        <p>Message: {}.</p>\n        <p>Error code explanation: {code} - {}.</p>\n    \
         </body>\n</html>\n",
        escape(message),
        escape(explain)
    )
}

/// `html.escape(text, quote=False)`.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The (short, long) texts `BaseHTTPRequestHandler.responses` has for the
/// codes these fixtures send.
fn responses(code: u16) -> (&'static str, &'static str) {
    match code {
        404 => ("Not Found", "Nothing matches the given URI"),
        501 => ("Not Implemented", "Server does not support this operation"),
        _ => ("???", "???"),
    }
}

/// `self.send_error(code, message)`: the status line carries `message` (the
/// short text by default), the body Python's error page, omitted for HEAD.
pub fn send_error(request: &Request, code: u16, message: Option<&str>) -> Reply {
    let (short, explain) = responses(code);
    let message = message.unwrap_or(short).to_owned();
    let body = error_page(code, &message, explain);
    let head = format!(
        "HTTP/1.0 {code} {message}\r\nConnection: close\r\nContent-Type: text/html;charset=utf-8\r\n\
         Content-Length: {}\r\n\r\n",
        body.len()
    );
    let body = if request.method == "HEAD" { String::new() } else { body };
    Reply::Raw(Box::new(move |stream| {
        let _ = stream
            .write_all(head.as_bytes())
            .and_then(|()| stream.write_all(body.as_bytes()))
            .and_then(|()| stream.flush());
    }))
}

/// Dispatches like `BaseHTTPRequestHandler`: `handle` runs for the methods
/// the Python handler defined; any other method gets 501.
pub fn dispatch(request: &Request, methods: &[&str], handle: impl FnOnce() -> Reply) -> Reply {
    if methods.contains(&request.method.as_str()) {
        handle()
    } else {
        let message = format!("Unsupported method ('{}')", request.method);
        send_error(request, 501, Some(&message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_page_matches_python_3_14() {
        // `DEFAULT_ERROR_MESSAGE % {'code': 404, 'message': 'Not Found',
        // 'explain': 'Nothing matches the given URI'}` from Python 3.14.
        let expected = "<!DOCTYPE HTML>\n<html lang=\"en\">\n    <head>\n        <meta charset=\"utf-8\">\n        \
            <style type=\"text/css\">\n            :root {\n                color-scheme: light dark;\n            }\n        \
            </style>\n        <title>Error response</title>\n    </head>\n    <body>\n        <h1>Error response</h1>\n        \
            <p>Error code: 404</p>\n        <p>Message: Not Found.</p>\n        \
            <p>Error code explanation: 404 - Nothing matches the given URI.</p>\n    </body>\n</html>\n";
        assert_eq!(error_page(404, "Not Found", "Nothing matches the given URI"), expected);
        assert_eq!(escape("a<b>&c"), "a&lt;b&gt;&amp;c");
    }
}
