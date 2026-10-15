//! `:` command completion in a real Hamn PTY with the recorded native peers:
//! the dimmed completion follows the typed text, Tab or → takes it, Enter
//! runs what the prompt holds, the session's history comes before the known
//! commands, and Tab switches the workspace only while no command is typed.
use crate::runner::{self, Case, case};
use crate::support::tui::Harness;
use std::process::ExitCode;

pub fn main(filters: &[String]) -> ExitCode {
    let cases: Vec<Case> = vec![
        case("tab_takes_a_kubectl_alias_and_keeps_the_workspace", tab_takes_a_kubectl_alias_and_keeps_the_workspace),
        case("right_arrow_takes_a_docker_command", right_arrow_takes_a_docker_command),
        case("history_completes_before_the_known_commands", history_completes_before_the_known_commands),
        case("tab_switches_the_workspace_after_the_prompt_closes", tab_switches_the_workspace_after_the_prompt_closes),
    ];
    runner::run(
        "tui-command-completion",
        "Tab and → complete : commands from history and known commands; Tab still switches workspaces",
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
