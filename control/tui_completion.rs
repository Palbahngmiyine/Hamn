//! Completion of the `:` command prompt, as in k9s: while a command is
//! typed, the first command that extends it is shown dimmed after the
//! cursor, and Tab or → takes it.
//!
//! Contract: `suggestion(typed, workspace, history)` returns the text that
//! taking the completion appends to `typed`; it is never empty, and `None`
//! means that nothing extends `typed`. The candidates are, in this order,
//! the session's command `history`, newest first, and then the `commands`
//! of the workspace that `typed` addresses (`native::command_workspace`).
//! A candidate extends `typed` when it starts with all of `typed`, compared
//! exactly: taking it only appends, so the typed text, its case and its
//! Unicode are never rewritten. Empty input has no suggestion; Up recalls
//! the history there. Nothing here reads files or runs processes, so a
//! keystroke never waits for completion.

use crate::native::{DOCKER_VIEW_ALIASES, KUBE_VIEW_ALIASES, command_workspace};
use crate::preferences::Workspace;

/// Docker list commands that open a resource view, the most used first.
const DOCKER_COMMANDS: &[&str] = &[
    "ps",
    "ps -a",
    "images",
    "volume ls",
    "network ls",
    "compose ls",
    "image ls",
    "container ls",
];

/// Kinds offered after `get`, the most used first: built-in Kubernetes API
/// resources only. Custom resource kinds would need the cluster's discovery.
const KUBE_GET_KINDS: &[&str] = &[
    "pods",
    "deployments",
    "services",
    "namespaces",
    "nodes",
    "configmaps",
    "secrets",
    "statefulsets",
    "daemonsets",
    "replicasets",
    "jobs",
    "cronjobs",
    "ingresses",
    "persistentvolumeclaims",
    "persistentvolumes",
    "storageclasses",
    "serviceaccounts",
    "events",
    "roles",
    "rolebindings",
    "clusterroles",
    "clusterrolebindings",
    "networkpolicies",
    "horizontalpodautoscalers",
    "poddisruptionbudgets",
    "customresourcedefinitions",
];

/// The commands that complete in `workspace`, in the order they are
/// offered: the Docker or kubectl commands that open a resource view, the
/// Hamn views that `:` opens by name, and the same Docker or kubectl
/// commands after their program word.
pub fn commands(workspace: Workspace) -> Vec<String> {
    let (program, hamn_views, native): (&str, &[&str], Vec<String>) = match workspace {
        Workspace::Containers => (
            "docker",
            &["vm"],
            DOCKER_COMMANDS
                .iter()
                .chain(DOCKER_VIEW_ALIASES.iter().map(|(alias, _)| alias))
                .map(|command| command.to_string())
                .collect(),
        ),
        Workspace::Kubernetes => (
            "kubectl",
            &["contexts", "ctx"],
            KUBE_VIEW_ALIASES
                .iter()
                .map(|alias| alias.to_string())
                .chain(KUBE_GET_KINDS.iter().flat_map(|kind| {
                    let get = format!("get {kind}");
                    let all_namespaces = (*kind == "pods").then(|| format!("{get} -A"));
                    std::iter::once(get).chain(all_namespaces)
                }))
                .collect(),
        ),
    };
    let prefixed: Vec<String> = native
        .iter()
        .map(|command| format!("{program} {command}"))
        .collect();
    native
        .into_iter()
        .chain(hamn_views.iter().map(|view| view.to_string()))
        .chain(prefixed)
        .collect()
}

/// What Tab or → appends to the typed `:` command; see the module contract.
pub fn suggestion(typed: &str, workspace: Workspace, history: &[String]) -> Option<String> {
    if typed.is_empty() {
        return None;
    }
    let commands = commands(command_workspace(typed, workspace));
    history
        .iter()
        .rev()
        .chain(&commands)
        .find(|candidate| candidate.len() > typed.len() && candidate.starts_with(typed))
        // `typed` is a prefix of the candidate, so its length is a character
        // boundary of the candidate.
        .map(|candidate| candidate[typed.len()..].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Request;
    use crate::tui_state::State;

    fn history(commands: &[&str]) -> Vec<String> {
        commands.iter().map(|command| command.to_string()).collect()
    }

    #[test]
    fn empty_input_suggests_nothing_even_with_history() {
        let history = history(&["ps -a", "get pods"]);
        for workspace in [Workspace::Containers, Workspace::Kubernetes] {
            assert_eq!(suggestion("", workspace, &history), None);
            assert_eq!(suggestion("", workspace, &[]), None);
        }
    }

    #[test]
    fn input_that_no_candidate_extends_suggests_nothing() {
        let history = history(&["ps -a"]);
        // No candidate starts with it.
        assert_eq!(suggestion("zz", Workspace::Containers, &history), None);
        assert_eq!(suggestion("get zz", Workspace::Kubernetes, &history), None);
        // A complete command that nothing longer starts with.
        assert_eq!(suggestion("ps -a", Workspace::Containers, &history), None);
        assert_eq!(suggestion("pods", Workspace::Kubernetes, &[]), None);
        // Matching is exact: no case folding and no leading space.
        assert_eq!(suggestion("PS", Workspace::Containers, &[]), None);
        assert_eq!(suggestion(" ps", Workspace::Containers, &[]), None);
        // The commands of the other workspace are not offered without the
        // word that addresses it.
        assert_eq!(suggestion("dep", Workspace::Containers, &[]), None);
        assert_eq!(suggestion("ima", Workspace::Kubernetes, &[]), None);
        assert_eq!(suggestion("v", Workspace::Kubernetes, &[]), None);
    }

    #[test]
    fn workspace_commands_complete_in_order() {
        for (typed, workspace, rest) in [
            ("p", Workspace::Containers, "s"),
            ("ps", Workspace::Containers, " -a"),
            ("ima", Workspace::Containers, "ges"),
            ("image ", Workspace::Containers, "ls"),
            // Docker's own commands come before Hamn's aliases and views.
            ("v", Workspace::Containers, "olume ls"),
            ("volume", Workspace::Containers, " ls"),
            ("volumes", Workspace::Containers, ""),
            ("co", Workspace::Containers, "mpose ls"),
            ("cont", Workspace::Containers, "ainer ls"),
            ("container", Workspace::Containers, " ls"),
            ("containers", Workspace::Containers, ""),
            ("docker ", Workspace::Containers, "ps"),
            ("docker net", Workspace::Containers, "work ls"),
            ("p", Workspace::Kubernetes, "ods"),
            ("dep", Workspace::Kubernetes, "loyments"),
            ("sv", Workspace::Kubernetes, "c"),
            ("co", Workspace::Kubernetes, "ntexts"),
            ("get ", Workspace::Kubernetes, "pods"),
            ("get pods", Workspace::Kubernetes, " -A"),
            ("get cr", Workspace::Kubernetes, "onjobs"),
            ("get cu", Workspace::Kubernetes, "stomresourcedefinitions"),
            ("kubectl get dep", Workspace::Kubernetes, "loyments"),
            // The program word addresses its workspace from the other one.
            ("kubectl get dep", Workspace::Containers, "loyments"),
            ("docker im", Workspace::Kubernetes, "ages"),
        ] {
            // "" marks a complete command that nothing longer starts with.
            assert_eq!(
                suggestion(typed, workspace, &[]).as_deref(),
                Some(rest).filter(|rest| !rest.is_empty()),
                "{typed:?} in {workspace:?}"
            );
        }
    }

    #[test]
    fn history_comes_before_workspace_commands_newest_first() {
        // Both the history and a command extend "get p": the history wins.
        let entries = history(&["get pods -l app=web"]);
        assert_eq!(
            suggestion("get p", Workspace::Kubernetes, &entries).as_deref(),
            Some("ods -l app=web")
        );
        // The newest of two matching entries wins.
        let entries = history(&["ps -a", "ps --filter status=exited", "images"]);
        assert_eq!(
            suggestion("ps", Workspace::Containers, &entries).as_deref(),
            Some(" --filter status=exited")
        );
        // An entry equal to the input extends nothing; the commands still do.
        let entries = history(&["ps"]);
        assert_eq!(
            suggestion("ps", Workspace::Containers, &entries).as_deref(),
            Some(" -a")
        );
        // An entry that addresses the other workspace is offered too: the
        // history holds what was run from this prompt.
        let entries = history(&["docker ps -a"]);
        assert_eq!(
            suggestion("d", Workspace::Kubernetes, &entries).as_deref(),
            Some("ocker ps -a")
        );
        assert_eq!(
            suggestion("d", Workspace::Kubernetes, &[]).as_deref(),
            Some("eployments")
        );
    }

    #[test]
    fn unicode_input_completes_after_whole_characters() {
        let entries = history(&["get pods -l app=작업", "logs é-api"]);
        assert_eq!(
            suggestion("get pods -l app=작", Workspace::Kubernetes, &entries).as_deref(),
            Some("업")
        );
        assert_eq!(
            suggestion("logs ", Workspace::Kubernetes, &entries).as_deref(),
            Some("é-api")
        );
        // Unicode that nothing starts with, and a decomposed "é" that differs
        // from the stored one, suggest nothing.
        assert_eq!(suggestion("작", Workspace::Kubernetes, &entries), None);
        assert_eq!(
            suggestion("logs e\u{301}", Workspace::Kubernetes, &entries),
            None
        );
    }

    #[test]
    fn every_command_opens_a_view_of_its_workspace() {
        for workspace in [Workspace::Containers, Workspace::Kubernetes] {
            let mut state = State::new(Request::default());
            state.workspace = workspace;
            let commands = commands(workspace);
            assert!(commands.len() > 10, "{commands:?}");
            for command in &commands {
                assert_eq!(
                    command_workspace(command, workspace),
                    workspace,
                    "{command}"
                );
                if ["vm", "contexts", "ctx"].contains(&command.as_str()) {
                    // The Hamn views; `tui.rs` sends these to `State::view`.
                    let request = state.view(command).unwrap();
                    assert!(!request.mutates(), "{command}");
                    continue;
                }
                let invocation = crate::native::parse(command, &state).unwrap();
                assert_eq!(invocation.workspace, workspace, "{command}");
                // An installed kubectl plugin owns its alias, as `parse` says.
                let words: Vec<String> = command.split(' ').map(String::from).collect();
                let plugin = crate::native::installed_kubectl_plugin(
                    &words,
                    Some(usize::from(words[0] == "kubectl")),
                );
                assert!(invocation.resource.is_some() || plugin, "{command}");
            }
        }
    }
}
