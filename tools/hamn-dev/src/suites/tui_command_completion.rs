//! `:` command completion in a real Hamn PTY with the recorded native peers:
//! the dimmed completion follows the typed text, Tab or → takes it, Enter
//! runs what the prompt holds, the session's history comes before the known
//! commands, and Tab switches the workspace only while no command is typed.
//! In Kubernetes, `get` also completes the kinds that the cluster serves,
//! which `kubectl api-resources -o name` lists in the background: a lookup
//! that blocks never delays typing, one that fails leaves the built-in kinds
//! and says why in the prompt's border, one that kubectl ends unsuccessfully
//! after listing some kinds offers those and says why others may be missing,
//! and a lookup for a context the UI left, or one still running when the TUI
//! quits, is killed.
use super::tui_native_regressions::record;
use crate::runner::{self, Case, case};
use crate::support::http::{Options as HttpOptions, Server};
use crate::support::tui::{self, Harness, Options, kinds_lookup};
use crate::support::{kube_api, pty, py_http, real_cli};
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn main(filters: &[String]) -> ExitCode {
    let mut cases: Vec<Case> = vec![
        case("tab_takes_a_kubectl_alias_and_keeps_the_workspace", tab_takes_a_kubectl_alias_and_keeps_the_workspace),
        case("right_arrow_takes_a_docker_command", right_arrow_takes_a_docker_command),
        case("history_completes_before_the_known_commands", history_completes_before_the_known_commands),
        case("tab_switches_the_workspace_after_the_prompt_closes", tab_switches_the_workspace_after_the_prompt_closes),
        case(
            "a_blocked_lookup_keeps_typing_responsive_and_a_failed_one_says_why",
            a_blocked_lookup_keeps_typing_responsive_and_a_failed_one_says_why,
        ),
        case("a_lookup_for_a_context_the_ui_left_is_killed", a_lookup_for_a_context_the_ui_left_is_killed),
        case(
            "a_partial_lookup_offers_what_kubectl_listed_and_says_why_kinds_are_missing",
            a_partial_lookup_offers_what_kubectl_listed_and_says_why_kinds_are_missing,
        ),
        case("quitting_kills_a_running_lookup", quitting_kills_a_running_lookup),
    ];
    match real_cli::which("kubectl") {
        Some(kubectl) => {
            let partial = kubectl.clone();
            cases.push(case("the_installed_kubectl_lists_the_kinds_that_complete", move || {
                the_installed_kubectl_lists_the_kinds_that_complete(&kubectl)
            }));
            cases.push(case("the_installed_kubectl_lists_what_it_can_when_an_api_group_fails", move || {
                the_installed_kubectl_lists_what_it_can_when_an_api_group_fails(&partial)
            }));
        }
        None => println!("SKIP: installed kubectl unavailable for the cases that run it against a fake API"),
    }
    runner::run(
        "tui-command-completion",
        "Tab and → complete : commands from history, known commands and the cluster's kinds; a slow or failed \
         kinds lookup never delays typing; Tab still switches workspaces",
        cases,
        filters,
    )
}

/// The argument lists of the recorded `program` calls that contain `word`.
fn calls_with(harness: &Harness, program: &str, word: &str) -> Vec<Vec<String>> {
    harness
        .calls()
        .into_iter()
        .filter(|(name, args)| name == program && args.iter().any(|arg| arg == word))
        .map(|(_, args)| args)
        .collect()
}

fn tab_takes_a_kubectl_alias_and_keeps_the_workspace() {
    let mut harness = Harness::new("kubernetes");
    harness.until("old-target-row");
    // The completion is drawn after the typed text before anything takes it.
    harness.send(b":dep", ":deployments");
    harness.write(b"\t\r");
    // The title changes on Enter; the query of the completed alias follows.
    harness.wait(|harness| !calls_with(harness, "kubectl", "deployments").is_empty());
    let text = harness.text();
    assert!(text.contains("[Kubernetes]") && !text.contains(" Command "), "{text}");
    let queries = calls_with(&harness, "kubectl", "deployments");
    assert!(queries.iter().all(|args| args.iter().any(|arg| arg == "get")), "{queries:?}");
    assert!(calls_with(&harness, "kubectl", "dep").is_empty(), "{:?}", harness.calls());
}

fn right_arrow_takes_a_docker_command() {
    let mut harness = Harness::new("containers");
    harness.until("old-target-row");
    harness.send(b":ima", ":images");
    // → in the terminal's normal cursor mode, then Enter.
    harness.write(b"\x1b[C\r");
    harness.wait(|harness| !calls_with(harness, "docker", "images").is_empty());
    assert!(harness.text().contains("[Containers]"), "{}", harness.text());
    assert!(calls_with(&harness, "docker", "ima").is_empty(), "{:?}", harness.calls());
    // A `:` in Containers looks up no Kubernetes kinds.
    assert!(harness.kinds_lookups().is_empty(), "{:?}", harness.calls());
}

fn history_completes_before_the_known_commands() {
    let mut harness = Harness::new("kubernetes");
    harness.until("old-target-row");
    // Without automatic refreshes, each query below is one typed command.
    harness.send(b"p", "Paused");
    harness.write(b":get pods -l app=web\r");
    harness.wait(|harness| calls_with(harness, "kubectl", "app=web").len() == 1);
    harness.until("old-target-row");
    // "get pods" is a known command too; the command run before comes first.
    harness.send(b":g", ":get pods -l app=web");
    harness.write(b"\t\r");
    harness.wait(|harness| calls_with(harness, "kubectl", "app=web").len() == 2);
    let queries = calls_with(&harness, "kubectl", "app=web");
    assert_eq!(queries[0], queries[1], "{queries:?}");
    let typed = ["get", "pods", "-l", "app=web"];
    assert!(queries[1].windows(typed.len()).any(|window| window == &typed[..]), "{queries:?}");
    // The first prompt looked up the kinds; the second kept its answer.
    harness.wait(|harness| !harness.kinds_lookups().is_empty());
    assert_eq!(harness.kinds_lookups(), [kinds_lookup("old-cluster")]);
}

fn tab_switches_the_workspace_after_the_prompt_closes() {
    let mut harness = Harness::new("containers");
    harness.until("old-target-row");
    harness.send(b":ima", ":images");
    harness.write(b"\x1b");
    // A lone Esc closes the prompt without running it; Tab then switches
    // the workspace.
    harness.wait(|harness| !harness.text().contains(" Command "));
    harness.send(b"\t", "[Kubernetes]");
    harness.wait(|harness| !calls_with(harness, "kubectl", "get").is_empty());
    assert!(calls_with(&harness, "docker", "images").is_empty(), "{:?}", harness.calls());
}

/// What the peer prints on stderr when its first lookup fails.
const LOOKUP_ERROR: &str = "error: You must be logged in to the server (Unauthorized)";

/// What the peer prints on stderr when its first lookup finds only some kinds.
const PARTIAL_ERROR: &str = "error: unable to retrieve the complete list of server APIs: metrics.k8s.io/v1beta1: the server is currently unable to handle the request";

/// A Kubernetes harness whose kinds lookups follow `mode`; see `kinds_peer`.
fn kinds_harness(mode: &str) -> Harness {
    let harness = Harness::with(Options { fixture: "tui-command-completion-kinds", ..Options::new("kubernetes") });
    fs::write(harness.root.join("kinds-mode"), mode).unwrap();
    harness
}

/// The PID that a blocked lookup wrote.
fn lookup_pid(root: &Path) -> libc::pid_t {
    fs::read_to_string(root.join("kinds-pid")).unwrap().trim().parse().unwrap()
}

/// Whether `pid` names no process, not even an unreaped one.
fn gone(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    (unsafe { libc::kill(pid, 0) }) == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn a_blocked_lookup_keeps_typing_responsive_and_a_failed_one_says_why() {
    let mut harness = kinds_harness("fail-first");
    harness.until("old-target-row");
    harness.send(b"p", "Paused");
    harness.write(b":");
    harness.noticed("kinds-blocked");
    // While the lookup is blocked, the typed text is drawn and the built-in
    // kinds complete; the completed command runs.
    harness.send(b"get dep", ":get deployments");
    harness.write(b"\t\r");
    harness.wait(|harness| !calls_with(harness, "kubectl", "deployments").is_empty());
    harness.wait(|harness| !harness.text().contains(" Command "));
    // A prompt opened while it runs starts no second lookup and offers no
    // kind that only the cluster could name: the completion would be drawn
    // in the same frame as the typed text.
    harness.send(b":get wid", ":get wid");
    assert!(!harness.text().contains(":get widg"), "{}", harness.text());
    assert_eq!(harness.kinds_lookups(), [kinds_lookup("old-cluster")]);
    // The lookup fails while the prompt is open: the prompt's border says
    // why in one line, and the message line does not.
    harness.release_gate();
    let reason = "Kubernetes rejected the credentials of this context";
    harness.until(&format!("! get completes built-in kinds only: {reason}"));
    assert_eq!(harness.text().matches(reason).count(), 1, "{}", harness.text());
    assert!(!harness.text().contains(":get widg"), "{}", harness.text());
    // kubectl's own words are kept for the diagnostics view.
    harness.write(b"\x1b");
    harness.wait(|harness| !harness.text().contains(" Command "));
    harness.send(b"!", LOOKUP_ERROR);
    assert!(harness.text().contains("Kinds lookup failed (cliError)"), "{}", harness.text());
    harness.write(b"!");
    harness.wait(|harness| !harness.text().contains(LOOKUP_ERROR));
    // The next prompt retries the lookup, which answers this time: the
    // cluster's own kind completes and the border's reason is gone.
    harness.write(b":get wid");
    harness.until(":get widgets");
    assert!(!harness.text().contains("built-in kinds only"), "{}", harness.text());
    harness.write(b"\t\r");
    harness.wait(|harness| !calls_with(harness, "kubectl", "widgets").is_empty());
    let queries = calls_with(&harness, "kubectl", "widgets");
    assert!(queries.iter().all(|args| args.iter().any(|arg| arg == "get")), "{queries:?}");
    assert_eq!(harness.kinds_lookups(), [kinds_lookup("old-cluster"), kinds_lookup("old-cluster")]);
}

fn a_lookup_for_a_context_the_ui_left_is_killed() {
    let mut harness = kinds_harness("old-blocks");
    // The native peer answers the new context's query without blocking.
    fs::write(harness.root.join("released"), "").unwrap();
    harness.until("old-target-row");
    harness.write(b":");
    harness.noticed("kinds-blocked");
    let pid = lookup_pid(&harness.root);
    assert!(!gone(pid));
    harness.write(b"\x1b");
    harness.wait(|harness| !harness.text().contains(" Command "));
    // Another context from the embedded terminal; the list reloads for it.
    harness.send(b":kubectl config use-context new-cluster\r", "CONFIG_DONE");
    harness.until("Exit code 0");
    harness.write(b"\r");
    harness.until("new-target-row");
    // The lookup of the old context was killed and reaped.
    let deadline = Instant::now() + Duration::from_secs(15);
    while !gone(pid) {
        assert!(Instant::now() < deadline, "the lookup {pid} for old-cluster outlived its context");
        std::thread::sleep(Duration::from_millis(10));
    }
    // A prompt for the new context looks up its own kinds.
    harness.write(b":get wid");
    harness.until(":get widgets");
    harness.write(b"\t\r");
    harness.wait(|harness| !calls_with(harness, "kubectl", "widgets").is_empty());
    let queries = calls_with(&harness, "kubectl", "widgets");
    assert!(queries.iter().all(|args| args.iter().any(|arg| arg == "new-cluster")), "{queries:?}");
    assert_eq!(harness.kinds_lookups(), [kinds_lookup("old-cluster"), kinds_lookup("new-cluster")]);
}

fn a_partial_lookup_offers_what_kubectl_listed_and_says_why_kinds_are_missing() {
    let mut harness = kinds_harness("partial");
    harness.until("old-target-row");
    // kubectl printed names and exited 1: the names complete, and the
    // border says that some kinds may be missing, and why.
    harness.send(b":get wid", ":get widgets");
    harness.until("! get completes only the kinds kubectl listed: These API groups could not be listed: metrics.k8s.io/v1beta1");
    let text = harness.text();
    assert_eq!(text.matches("metrics.k8s.io/v1beta1").count(), 1, "{text}");
    // The diagnostics view keeps kubectl's own words, wrapped at its width.
    harness.write(b"\x1b");
    harness.wait(|harness| !harness.text().contains(" Command "));
    harness.send(b"!", "Kinds lookup failed (cliError); get completes only the kinds kubectl listed");
    let words = "error: unable to retrieve the complete list of server APIs";
    harness.until(words);
    harness.write(b"!");
    harness.wait(|harness| !harness.text().contains(words));
    // The next prompt retries it. The kinds kubectl listed stay offered while
    // the retry runs, and its complete answer clears the border.
    harness.send(b":get wid", ":get widgets");
    harness.wait(|harness| !harness.text().contains("kinds kubectl listed"));
    harness.write(b"\t\r");
    harness.wait(|harness| !calls_with(harness, "kubectl", "widgets").is_empty());
    assert_eq!(harness.kinds_lookups(), [kinds_lookup("old-cluster"), kinds_lookup("old-cluster")]);
}

fn quitting_kills_a_running_lookup() {
    let mut harness = kinds_harness("old-blocks");
    harness.until("old-target-row");
    harness.write(b":");
    harness.noticed("kinds-blocked");
    let pid = lookup_pid(&harness.root);
    harness.write(b"\x1b");
    harness.wait(|harness| !harness.text().contains(" Command "));
    harness.write(b"q");
    let master = harness.master();
    let status = pty::wait_for_exit(&mut harness.child, master, Duration::from_secs(15));
    assert!(status.is_some_and(|status| status.success()), "{status:?}");
    // The TUI reaps its lookup before it exits.
    assert!(gone(pid), "the lookup {pid} outlived the TUI");
}

/// A fake API whose discovery serves pods in the core group, `widgets` in
/// `fixture.hamn.dev` and pods again in `metrics.k8s.io`, and one widget.
/// With `broken`, it also lists the group `broken.fixture.hamn.dev`, whose
/// resources answer 503, as an aggregated API whose service is down does.
/// Every request target is recorded.
fn kinds_api(requests: Arc<Mutex<Vec<String>>>, broken: bool) -> Server {
    let group = |name: &str, version: &str| {
        let version = json!({"groupVersion": format!("{name}/{version}"), "version": version});
        json!({"name": name, "versions": [version], "preferredVersion": version})
    };
    let resources = |group_version: &str, plural: &str, kind: &str| {
        json!({"apiVersion": "v1", "kind": "APIResourceList", "groupVersion": group_version, "resources": [
            {"name": plural, "singularName": "", "namespaced": true, "kind": kind, "verbs": ["get", "list"]}]})
    };
    let widget = json!({"apiVersion": "fixture.hamn.dev/v1", "kind": "Widget", "metadata": {
        "name": "listed-widget", "namespace": "test", "uid": "widget-uid", "resourceVersion": "1"}});
    Server::tcp(HttpOptions::default(), move |request| {
        py_http::dispatch(request, &["GET"], || {
            requests.lock().unwrap().push(request.target.clone());
            let path = request.target.split('?').next().unwrap_or_default();
            let body = match path {
                "/apis" => {
                    let mut groups = vec![group("fixture.hamn.dev", "v1"), group("metrics.k8s.io", "v1beta1")];
                    if broken {
                        groups.push(group("broken.fixture.hamn.dev", "v1"));
                    }
                    json!({"apiVersion": "v1", "kind": "APIGroupList", "groups": groups})
                }
                "/apis/broken.fixture.hamn.dev/v1" if broken => return py_http::send_error(request, 503, None),
                "/apis/fixture.hamn.dev/v1" => resources("fixture.hamn.dev/v1", "widgets", "Widget"),
                "/apis/metrics.k8s.io/v1beta1" => resources("metrics.k8s.io/v1beta1", "pods", "PodMetrics"),
                "/apis/fixture.hamn.dev/v1/namespaces/test/widgets" => {
                    json!({"apiVersion": "fixture.hamn.dev/v1", "kind": "WidgetList", "items": [widget.clone()]})
                }
                _ => return kube_api::discovery(&request.target).into(),
            };
            kube_api::json_response(&body).into()
        })
    })
}

fn the_installed_kubectl_lists_the_kinds_that_complete(kubectl: &Path) {
    let requests: Arc<Mutex<Vec<String>>> = Arc::default();
    // Declared before the harness so that it stops after Hamn does.
    let server = kinds_api(Arc::clone(&requests), false);
    let mut harness = Harness::new("kubernetes");
    harness.until("old-target-row");
    let config = harness.root.join("kubeconfig");
    let mut content: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    content["clusters"][0]["cluster"]["server"] = json!(server.url("http"));
    fs::write(&config, content.to_string()).unwrap();
    real_cli::wrap(&harness.root, "kubectl", kubectl, "exec-real");
    // What the installed kubectl printed for the fake API is what completes:
    // the custom resource by its plural, and the plural that two groups
    // serve with its group.
    harness.send(b":get wid", ":get widgets");
    harness.write(b"\x1b");
    harness.wait(|harness| !harness.text().contains(" Command "));
    harness.send(b":get pods.", ":get pods.metrics.k8s.io");
    harness.write(b"\x1b");
    harness.wait(|harness| !harness.text().contains(" Command "));
    // The completed custom kind opens its view from the same API.
    harness.write(b":get wid\t\r");
    harness.until("listed-widget");
    let requests = requests.lock().unwrap().clone();
    assert!(requests.iter().any(|target| target.starts_with("/apis/fixture.hamn.dev/v1/namespaces/test/widgets")), "{requests:?}");
    drop(harness);
    drop(server);
}

fn the_installed_kubectl_lists_what_it_can_when_an_api_group_fails(kubectl: &Path) {
    let requests: Arc<Mutex<Vec<String>>> = Arc::default();
    // Declared before the harness so that it stops after Hamn does.
    let server = kinds_api(Arc::clone(&requests), true);
    let mut harness = Harness::new("kubernetes");
    harness.until("old-target-row");
    let config = harness.root.join("kubeconfig");
    let mut content: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
    content["clusters"][0]["cluster"]["server"] = json!(server.url("http"));
    fs::write(&config, content.to_string()).unwrap();
    real_cli::wrap(&harness.root, "kubectl", kubectl, "exec-real");
    // kubectl exits 1 after it printed the kinds of the groups that
    // answered: those complete, and the border names the group that did not.
    harness.send(b":get wid", ":get widgets");
    harness.until("! get completes only the kinds kubectl listed: These API groups could not be listed: broken.fixture.hamn.dev/v1");
    harness.write(b"\x1b");
    harness.wait(|harness| !harness.text().contains(" Command "));
    harness.send(b"!", "Kinds lookup failed (cliError); get completes only the kinds kubectl listed");
    assert!(harness.text().contains("unable to retrieve the complete list of server APIs"), "{}", harness.text());
    let requests = requests.lock().unwrap().clone();
    assert!(requests.iter().any(|target| target.starts_with("/apis/broken.fixture.hamn.dev/v1")), "{requests:?}");
    drop(harness);
    drop(server);
}

/// The kubectl peer of the kinds cases: every call but a kinds lookup goes
/// to the recorded native peer. A lookup is recorded, then follows
/// `$FIXTURE_ROOT/kinds-mode`:
/// - `fail-first`: the first lookup tells the test it is blocked, waits for
///   the gate byte and fails with `LOOKUP_ERROR`; later ones answer.
/// - `old-blocks`: a lookup of `old-cluster` writes its PID to `kinds-pid`,
///   tells the test and blocks until it is killed; others answer.
/// - `partial`: the first lookup prints `CLUSTER_KINDS` and fails with
///   `PARTIAL_ERROR`, as kubectl does when one API group does not answer;
///   later ones answer.
pub fn kinds_peer(program: &str, args: &[String]) -> ExitCode {
    if !tui::is_kinds_lookup(args) {
        return super::tui_native_regressions::fixture(program, args);
    }
    let root = PathBuf::from(std::env::var_os("FIXTURE_ROOT").expect("FIXTURE_ROOT"));
    record(&root, program, args);
    let lookups = fs::read_to_string(root.join("calls"))
        .unwrap()
        .lines()
        .filter(|line| line.contains("\"api-resources\""))
        .count();
    match fs::read_to_string(root.join("kinds-mode")).unwrap().trim() {
        "fail-first" if lookups == 1 => {
            notify_blocked(&root);
            File::open(root.join("gate")).unwrap().read_exact(&mut [0u8]).unwrap();
            eprintln!("{LOOKUP_ERROR}");
            ExitCode::from(1)
        }
        "old-blocks" if args.iter().any(|arg| arg == "old-cluster") => {
            let partial = root.join("kinds-pid.partial");
            fs::write(&partial, std::process::id().to_string()).unwrap();
            fs::rename(&partial, root.join("kinds-pid")).unwrap();
            notify_blocked(&root);
            loop {
                std::thread::park();
            }
        }
        "partial" if lookups == 1 => {
            print!("{}", tui::CLUSTER_KINDS);
            eprintln!("{PARTIAL_ERROR}");
            ExitCode::from(1)
        }
        "fail-first" | "old-blocks" | "partial" => tui::answer_kinds_lookup(args).expect("a kinds lookup"),
        mode => panic!("unknown kinds mode {mode:?}"),
    }
}

fn notify_blocked(root: &Path) {
    let mut notice = OpenOptions::new().write(true).open(root.join("notice")).unwrap();
    notice.write_all(b"kinds-blocked\n").unwrap();
}
