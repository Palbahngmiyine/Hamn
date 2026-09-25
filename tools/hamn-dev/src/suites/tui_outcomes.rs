//! Invalid settings and hidden-workspace uncertainty remain visible in the
//! real TUI: malformed or FIFO preferences return to the workspace choice,
//! and cancelling an accepted delete from another workspace reports its
//! unknown outcome after the terminal is restored.
use crate::runner::{self, Case, case};
use crate::support::harness_peers::Event;
use crate::support::http::{Options as HttpOptions, Reply, Response, Server};
use crate::support::pty;
use crate::support::tui::{Harness, Options};
use serde_json::{Value, json};
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn main(filters: &[String]) -> ExitCode {
    let mut cases: Vec<Case> = Vec::new();
    for fifo in [false, true] {
        cases.push(case(format!("invalid_preferences_return_to_choice/fifo={fifo}"), move || {
            invalid_preferences_return_to_choice(fifo)
        }));
    }
    cases.push(case(
        "hidden_workspace_warning_survives_cancel_and_exit",
        hidden_workspace_warning_survives_cancel_and_exit,
    ));
    runner::run(
        "tui-outcomes",
        "malformed/FIFO settings recover; hidden-workspace cancellation reports its unknown target",
        cases,
        filters,
    )
}

fn invalid_preferences_return_to_choice(fifo: bool) {
    let prepare = move |path: &Path| {
        if fifo {
            fs::remove_file(path).unwrap();
            let name = CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: the path is a valid C string.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "{}", std::io::Error::last_os_error());
        } else {
            fs::write(path, "{broken").unwrap();
        }
    };
    let mut harness = Harness::with(Options { prepare_preferences: Some(&prepare), ..Options::new("containers") });
    harness.until("Choose your default workspace");
    assert!(harness.text().contains("Invalid or unsafe"), "{}", harness.text());
    harness.send(b"1\r", "old-target-row");
    let preferences = harness.root.join(".hamn/tui.json");
    assert!(preferences.is_file());
    let saved: Value = serde_json::from_str(&fs::read_to_string(&preferences).unwrap()).unwrap();
    assert_eq!(
        saved,
        json!({"version": 1, "defaultWorkspace": "containers", "recentTargets": [{"kind": "hamn", "name": "default"}]})
    );
    assert_eq!(fs::metadata(&preferences).unwrap().permissions().mode() & 0o777, 0o600);
}

fn hidden_workspace_warning_survives_cancel_and_exit() {
    let (accepted, release, answered) =
        (Arc::new(Event::default()), Arc::new(Event::default()), Arc::new(Event::default()));
    let requests: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let server = {
        let (accepted, release, answered, requests) =
            (accepted.clone(), release.clone(), answered.clone(), requests.clone());
        Server::tcp(HttpOptions::default(), move |request| match request.method.as_str() {
            "GET" => Response::json(
                200,
                &json!({"apiVersion": "v1", "kind": "Pod", "metadata": {
                    "name": "sample", "namespace": "test", "uid": "uid-original", "resourceVersion": "1"}}),
            )
            .into(),
            "DELETE" => {
                let body = serde_json::from_slice(&request.body).expect("DELETE body");
                requests.lock().unwrap().push((request.target.clone(), body));
                accepted.set();
                let (release, answered) = (release.clone(), answered.clone());
                // Hold the accepted request until the frontend finishes
                // cancellation, then close it without a response.
                Reply::Raw(Box::new(move |_| {
                    release.wait(Duration::from_secs(20));
                    answered.set();
                }))
            }
            _ => Response::new(501, "unsupported method").into(),
        })
    };
    let mut harness = Harness::new("kubernetes");
    let release_on_exit = SetOnDrop(release.clone());
    harness.until("old-target-row");
    let config = harness.root.join("kubeconfig");
    let mut content: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    content["clusters"][0]["cluster"]["server"] = json!(server.url("http"));
    fs::write(&config, content.to_string()).unwrap();
    let command = format!(
        "k8s pods delete sample --context old-cluster --namespace test --kubeconfig {} --uid uid-original",
        config.display()
    );
    harness.send(format!(":{command}\r").as_bytes(), "Impact:");
    harness.write(b"y");
    assert!(accepted.wait(Duration::from_secs(5)), "{}", harness.text());
    harness.send(b"\t", "[Containers]");
    harness.send(b"q", "Cancel the active operation and exit?");
    harness.output.clear();
    harness.write(b"y");
    // Exit warnings follow terminal restoration, outside Ratatui draw frames.
    harness.wait(|harness| contains(&harness.output, b"outcomeUnknown") && contains(&harness.output, b"uid-original"));
    let master = harness.master();
    let status = pty::wait_for_exit(&mut harness.child, master, Duration::from_secs(5)).expect("Hamn did not exit");
    assert_eq!(status.code(), Some(0), "{status:?}");
    let text = String::from_utf8_lossy(&harness.output).into_owned();
    for value in ["k8s pods delete", "sample", "uid-original", "Inspect it before retrying"] {
        assert!(text.contains(value), "{text}");
    }
    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].1["preconditions"], json!({"uid": "uid-original", "resourceVersion": "1"}));
    let mut names: Vec<String> = fs::read_dir(harness.root.join(".hamn"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(names == ["tui.json"] || names == ["tui.json", "tui.lock"], "{names:?}");
    // The Python suite's finally: release, close the frontend, stop the
    // server and require its request handler to have finished.
    drop(release_on_exit);
    drop(harness);
    drop(server);
    assert!(answered.wait(Duration::from_secs(5)), "the held DELETE handler did not finish");
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// Sets the event when dropped, so a failing case still releases the held
/// request before the harness closes.
struct SetOnDrop(Arc<Event>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.set();
    }
}
