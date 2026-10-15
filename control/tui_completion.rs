//! Completion of the `:` command prompt, as in k9s: while a command is
//! typed, the first command that extends it is shown dimmed after the
//! cursor, and Tab or → takes it.
//!
//! Contract: `suggestion(typed, workspace, history, cluster_kinds)` returns
//! the text that taking the completion appends to `typed`; it is never
//! empty, and `None` means that nothing extends `typed`. The candidates are,
//! in this order, the session's command `history`, newest first, and then
//! the `commands` of the workspace that `typed` addresses
//! (`native::command_workspace`). In Kubernetes, `get` completes the
//! built-in kinds and then the `cluster_kinds` that are not among them: the
//! words `cluster_kinds::words` made from the cluster's own list, which the
//! caller passes only for its current cluster (Containers passes none).
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
/// resources, offered for every cluster and before the cluster's own kinds.
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
/// commands after their program word. Kubernetes `get` takes the built-in
/// kinds, then the `cluster_kinds` that are not built in, in their order.
pub fn commands(workspace: Workspace, cluster_kinds: &[String]) -> Vec<String> {
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
                .chain(
                    KUBE_GET_KINDS
                        .iter()
                        .copied()
                        .chain(
                            cluster_kinds
                                .iter()
                                .map(String::as_str)
                                .filter(|kind| !KUBE_GET_KINDS.contains(kind)),
                        )
                        .flat_map(|kind| {
                            let get = format!("get {kind}");
                            let all_namespaces = (kind == "pods").then(|| format!("{get} -A"));
                            std::iter::once(get).chain(all_namespaces)
                        }),
                )
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
pub fn suggestion(
    typed: &str,
    workspace: Workspace,
    history: &[String],
    cluster_kinds: &[String],
) -> Option<String> {
    if typed.is_empty() {
        return None;
    }
    let commands = commands(command_workspace(typed, workspace), cluster_kinds);
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
            assert_eq!(suggestion("", workspace, &history, &[]), None);
            assert_eq!(suggestion("", workspace, &[], &[]), None);
        }
    }

    #[test]
    fn input_that_no_candidate_extends_suggests_nothing() {
        let history = history(&["ps -a"]);
        // No candidate starts with it.
        assert_eq!(suggestion("zz", Workspace::Containers, &history, &[]), None);
        assert_eq!(
            suggestion("get zz", Workspace::Kubernetes, &history, &[]),
            None
        );
        // A complete command that nothing longer starts with.
        assert_eq!(
            suggestion("ps -a", Workspace::Containers, &history, &[]),
            None
        );
        assert_eq!(suggestion("pods", Workspace::Kubernetes, &[], &[]), None);
        // Matching is exact: no case folding and no leading space.
        assert_eq!(suggestion("PS", Workspace::Containers, &[], &[]), None);
        assert_eq!(suggestion(" ps", Workspace::Containers, &[], &[]), None);
        // The commands of the other workspace are not offered without the
        // word that addresses it.
        assert_eq!(suggestion("dep", Workspace::Containers, &[], &[]), None);
        assert_eq!(suggestion("ima", Workspace::Kubernetes, &[], &[]), None);
        assert_eq!(suggestion("v", Workspace::Kubernetes, &[], &[]), None);
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
                suggestion(typed, workspace, &[], &[]).as_deref(),
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
            suggestion("get p", Workspace::Kubernetes, &entries, &[]).as_deref(),
            Some("ods -l app=web")
        );
        // The newest of two matching entries wins.
        let entries = history(&["ps -a", "ps --filter status=exited", "images"]);
        assert_eq!(
            suggestion("ps", Workspace::Containers, &entries, &[]).as_deref(),
            Some(" --filter status=exited")
        );
        // An entry equal to the input extends nothing; the commands still do.
        let entries = history(&["ps"]);
        assert_eq!(
            suggestion("ps", Workspace::Containers, &entries, &[]).as_deref(),
            Some(" -a")
        );
        // An entry that addresses the other workspace is offered too: the
        // history holds what was run from this prompt.
        let entries = history(&["docker ps -a"]);
        assert_eq!(
            suggestion("d", Workspace::Kubernetes, &entries, &[]).as_deref(),
            Some("ocker ps -a")
        );
        assert_eq!(
            suggestion("d", Workspace::Kubernetes, &[], &[]).as_deref(),
            Some("eployments")
        );
    }

    #[test]
    fn unicode_input_completes_after_whole_characters() {
        let entries = history(&["get pods -l app=작업", "logs é-api"]);
        assert_eq!(
            suggestion("get pods -l app=작", Workspace::Kubernetes, &entries, &[]).as_deref(),
            Some("업")
        );
        assert_eq!(
            suggestion("logs ", Workspace::Kubernetes, &entries, &[]).as_deref(),
            Some("é-api")
        );
        // Unicode that nothing starts with, and a decomposed "é" that differs
        // from the stored one, suggest nothing.
        assert_eq!(suggestion("작", Workspace::Kubernetes, &entries, &[]), None);
        assert_eq!(
            suggestion("logs e\u{301}", Workspace::Kubernetes, &entries, &[]),
            None
        );
    }

    fn sample_kinds() -> Vec<String> {
        crate::cluster_kinds::words(crate::cluster_kinds::SAMPLE_OUTPUT.as_bytes()).unwrap()
    }

    #[test]
    fn get_completes_the_clusters_kinds_after_the_built_in_ones() {
        let kinds = sample_kinds();
        for (typed, rest) in [
            // Custom resources, the shortest that extends the input first.
            ("get cert", "ificates"),
            ("get certificater", "equests"),
            ("get certificates", "igningrequests"),
            ("get iss", "uers"),
            ("kubectl get iss", "uers"),
            ("get cha", "llenges"),
            // A built-in kind comes first, even where a cluster's sorts before.
            ("get p", "ods"),
            ("get pods", " -A"),
            ("get ev", "ents"),
            ("get cu", "stomresourcedefinitions"),
            // A plural that two groups serve is completed with its group.
            ("get pods.", "metrics.k8s.io"),
            ("get events.", "events.k8s.io"),
            ("get lim", "itranges"),
        ] {
            assert_eq!(
                suggestion(typed, Workspace::Kubernetes, &[], &kinds).as_deref(),
                Some(rest),
                "{typed:?}"
            );
            // Without the cluster's kinds (none looked up yet, a failed
            // lookup, or the Containers prompt) only built-in kinds complete.
            assert_eq!(
                suggestion(typed, Workspace::Kubernetes, &[], &[]).is_some(),
                ["get p", "get pods", "get ev", "get cu"].contains(&typed),
                "{typed:?}"
            );
        }
        // History still comes first.
        let entries = history(&["get issuers -A"]);
        assert_eq!(
            suggestion("get iss", Workspace::Kubernetes, &entries, &kinds).as_deref(),
            Some("uers -A")
        );
        // A built-in kind that the cluster serves is offered once, in its
        // built-in place, before every kind of the cluster's.
        let offered = commands(Workspace::Kubernetes, &kinds);
        for kind in [
            "get pods",
            "get pods -A",
            "get deployments",
            "get events",
            "get customresourcedefinitions",
        ] {
            assert_eq!(
                offered.iter().filter(|command| *command == kind).count(),
                1,
                "{kind}"
            );
        }
        let position = |command: &str| {
            offered
                .iter()
                .position(|offered| offered == command)
                .unwrap()
        };
        let last_built_in = position("get customresourcedefinitions");
        for kind in ["get issuers", "get bindings", "get pods.metrics.k8s.io"] {
            assert!(position(kind) > last_built_in, "{kind}");
        }
        // `jobs` is built in: it keeps its place although the cluster serves it.
        assert!(position("get jobs") < last_built_in);
        // Docker commands do not change.
        assert_eq!(
            commands(Workspace::Containers, &kinds),
            commands(Workspace::Containers, &[])
        );
        assert_eq!(
            suggestion("docker im", Workspace::Kubernetes, &[], &kinds).as_deref(),
            Some("ages")
        );
    }

    #[test]
    fn every_command_opens_a_view_of_its_workspace() {
        for workspace in [Workspace::Containers, Workspace::Kubernetes] {
            let mut state = State::new(Request::default());
            state.workspace = workspace;
            let commands = commands(workspace, &sample_kinds());
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
