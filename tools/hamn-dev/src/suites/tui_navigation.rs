//! Direct TUI reads (`k8s pods logs|inspect NAME`) return to their list after
//! Esc without replaying the read; all state is disposable.
use crate::runner::{self, case};
use crate::support::exec::{self, Session};
use crate::support::http::{Options, Response, Server};
use crate::support::pty::{self, Pty};
use crate::support::screen::Screen;
use crate::support::{hamn, tmp::TempDir};
use serde_json::json;
use std::os::fd::AsRawFd;
use std::process::{Command, ExitCode};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn main(filters: &[String]) -> ExitCode {
    let cases =
        ["logs", "inspect"].into_iter().map(|action| case(format!("exercise/{action}"), move || exercise(action))).collect();
    runner::run("tui-navigation", "TUI direct logs/inspect return to the list without replaying reads", cases, filters)
}

fn exercise(action: &str) {
    let requests: Arc<Mutex<Vec<String>>> = Arc::default();
    let recorded = Arc::clone(&requests);
    let server = Server::tcp(Options::default(), move |request| {
        recorded.lock().unwrap().push(request.target.clone());
        let path = request.path();
        let mut object = json!({"apiVersion": "v1", "kind": "Pod", "metadata": {
            "name": "sample", "namespace": "default", "uid": "original",
            "resourceVersion": "10", "annotations": {"marker": "detail-response-seen"}}});
        let body = if path.ends_with("/log") {
            b"detail-response-seen\n".to_vec()
        } else if path.ends_with("/pods") {
            object["metadata"]["name"] = json!("returned-list-row");
            json!({"apiVersion": "v1", "kind": "PodList", "items": [object]}).to_string().into_bytes()
        } else {
            object.to_string().into_bytes()
        };
        Response::new(200, body).into()
    });
    let directory = TempDir::new("hamn-tui-navigation-");
    let home = directory.path();
    let config = home.join("config");
    std::fs::write(
        &config,
        json!({"apiVersion": "v1", "kind": "Config",
            "contexts": [{"name": "dev", "context": {"cluster": "dev", "namespace": "default"}}],
            "clusters": [{"name": "dev", "cluster": {"server": server.url("http")}}]})
        .to_string(),
    )
    .unwrap();
    let pty = Pty::open(32, 120);
    let mut command = Command::new(hamn());
    command.env("HOME", home).env("KUBECONFIG", &config).env("TERM", "xterm-256color");
    let mut session = Session(pty.spawn(&mut command));
    let master = pty.master.as_raw_fd();
    let mut output = Vec::new();
    let mut screen = Screen::new(40, 160);
    let paths = || requests.lock().unwrap().clone();
    let mut until = |output: &mut Vec<u8>, marker: &str| {
        let deadline = Instant::now() + Duration::from_secs(10);
        let raw = marker.starts_with('\x1b');
        while !(if raw { contains(output, marker.as_bytes()) } else { screen.text().contains(marker) }) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || pty::readable(&[master], remaining).is_empty() {
                let tail = &output[output.len().saturating_sub(3000)..];
                panic!("{action} {marker:?} {:?} {}", paths(), String::from_utf8_lossy(tail));
            }
            let data = pty::read_some(master);
            output.extend_from_slice(&data);
            screen.feed(&data);
        }
    };
    let logs = |paths: &[String]| paths.iter().filter(|path| path.contains("/pods/sample/log?")).count();
    let details = |paths: &[String]| paths.iter().filter(|path| path.ends_with("/pods/sample")).count();
    until(&mut output, "Hamn");
    pty::write_all(master, b"2\r");
    until(&mut output, "k8s contexts list");
    pty::write_all(master, format!(":k8s pods {action} sample --context dev --namespace default\r").as_bytes());
    until(&mut output, "detail-response-seen");
    let original = paths();
    assert_eq!(logs(&original), usize::from(action == "logs"), "{original:?}");
    assert_eq!(details(&original), 1, "{original:?}");
    output.clear();
    pty::write_all(master, b"\x1b");
    until(&mut output, "returned-list-row");
    let all = paths();
    let later = &all[original.len()..];
    assert!(!later.is_empty() && later.iter().all(|path| path.split('?').next().unwrap().ends_with("/pods")), "{later:?}");
    assert_eq!(logs(&all), usize::from(action == "logs"), "{all:?}");
    assert_eq!(details(&all), 1, "{all:?}");
    pty::write_all(master, b"q");
    until(&mut output, "\x1b[?1049l");
    let status = exec::wait_timeout(&mut session.0, Duration::from_secs(5)).expect("exit within 5 s");
    assert_eq!(status.code(), Some(0), "{status:?}");
    let mut names: Vec<String> = std::fs::read_dir(home.join(".hamn"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(names == ["tui.json"] || names == ["tui.json", "tui.lock"], "{names:?}");
    // Dropped in reverse order, also on failure: the session is reaped, the
    // PTY closes, the directory is removed, and then the server stops.
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}
