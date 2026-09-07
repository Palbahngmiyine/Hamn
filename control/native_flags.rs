//! Inspect the routing/connection flags without rewriting the CLI's argv.
use crate::preferences::Workspace;

pub const DOCKER_VALUES: &[&str] = &["--context", "-c", "--host", "-H", "--config", "--log-level", "-l", "--tlscacert", "--tlscert", "--tlskey"];
pub const KUBE_GLOBAL_VALUES: &[&str] = &["--v", "-v", "--vmodule", "--profile", "--profile-output", "--log-flush-frequency"];
pub fn takes_value(name: &str, workspace: Workspace) -> bool {
    takes_value_in(name, workspace, "get")
}

// This is flag arity for routing, not validation or an implementation of commands.
// tests/host/test_native_flag_inventory.py checks the installed kubectl's whole
// public command tree. The CLI still receives and interprets the original argv.
const KUBE_COMMAND_VALUES: &[&str] = &[
    "--accept-hosts", "--accept-paths", "--address", "--aggregation-rule", "--allowlist-entry",
    "--annotation", "--annotations", "--api-group", "--api-prefix", "--api-version",
    "--appendarg", "--audience", "--auth-provider", "--auth-provider-arg", "--bound-object-kind",
    "--bound-object-name", "--bound-object-uid", "--categories", "--cert", "--class",
    "--cluster-ip", "--clusterip", "--clusterrole", "--command", "--concurrency", "--container",
    "--containers", "--copy-to", "--cpu", "--current-replicas", "--custom", "--default-backend",
    "--description", "--detach-keys", "--docker-email", "--docker-password", "--docker-server",
    "--docker-username", "--duration", "--env", "--exec-api-version", "--exec-arg", "--exec-command",
    "--exec-env", "--exec-interactive-mode", "--external-ip", "--external-name", "--field-manager",
    "--for", "--from", "--from-env-file", "--from-file", "--from-literal", "--grace-period",
    "--group", "--hard", "--helm-api-versions", "--helm-command", "--helm-kube-version", "--image",
    "--image-pull-policy", "--keepalive", "--key", "--keys", "--labels", "--limit-bytes", "--limits",
    "--load-balancer-ip", "--load-restrictor", "--max", "--max-depth", "--max-log-requests",
    "--max-unavailable", "--memory", "--min", "--min-available", "--mount", "--name", "--namespaces",
    "--network-name", "--node-port", "--non-resource-url", "--option", "--output-directory",
    "--override-type", "--overrides", "--patch", "--patch-file", "--pod-running-timeout",
    "--pod-selector", "--policy", "--port", "--preemption-policy", "--prefix", "--prependarg",
    "--protocol", "--prune-allowlist", "--raw", "--reject-methods", "--reject-paths", "--replicas",
    "--requests", "--resource", "--resource-name", "--resource-version", "--restart", "--retries",
    "--revision", "--role", "--rule", "--schedule", "--scopes", "--section", "--serviceaccount",
    "--session-affinity", "--set-image", "--since", "--since-time", "--skip-wait-for-delete-timeout",
    "--tail", "--target", "--target-port", "--tcp", "--timeout", "--to-revision", "--type", "--types",
    "--unix-socket", "--value", "--verb", "--verbs", "--www", "--www-prefix",
    "-P", "-c", "-e", "-p", "-r", "-u",
];

pub fn command(args: &[String], workspace: Workspace) -> &str {
    crate::native::command_index(args, workspace).and_then(|i| args.get(i)).map_or("", String::as_str)
}

pub fn takes_value_in(name: &str, workspace: Workspace, command: &str) -> bool {
    if workspace == Workspace::Containers { DOCKER_VALUES.contains(&name) }
    else if (command == "logs" && ["-f", "-p", "--prefix"].contains(&name)) ||
        (command == "run" && name == "--command") || (command == "top" && name == "--containers") ||
        (command == "config" && name == "--raw") { false }
    else if name == "-w" { command == "proxy" }
    else { crate::native::KUBE_CONNECTION_VALUES.contains(&name) ||
        KUBE_COMMAND_VALUES.contains(&name) || KUBE_GLOBAL_VALUES.contains(&name) ||
        ["--selector", "-l", "--field-selector", "--filename", "-f", "--kustomize", "-k", "--sort-by", "--template", "--chunk-size", "--subresource", "--output", "-o", "--label-columns", "-L"].contains(&name) }
}

pub fn short_group(word: &str, workspace: Workspace) -> Option<Vec<String>> {
    short_options(word, workspace, false, "get")
}

fn option_takes_value(name: &str, workspace: Workspace, docker_query: bool, command: &str) -> bool {
    if docker_query { ["--filter", "-f", "--last", "-n", "--format"].contains(&name) }
    else { takes_value_in(name, workspace, command) }
}

fn short_options(word: &str, workspace: Workspace, docker_query: bool, command: &str) -> Option<Vec<String>> {
    if !word.starts_with('-') || word.starts_with("--") || word.len() < 2 { return None; }
    let booleans = if docker_query { "aslqh" } else if workspace == Workspace::Containers { "Dvh" } else { "ARfhipqtw" };
    let mut parts = Vec::new();
    for (offset, flag) in word[1..].char_indices() {
        let name = format!("-{flag}");
        let rest = &word[offset + 1 + flag.len_utf8()..];
        if option_takes_value(&name, workspace, docker_query, command) {
            parts.push(format!("{name}{rest}"));
            return Some(parts);
        }
        if !booleans.contains(flag) { return None; }
        if rest.starts_with('=') { parts.push(format!("{name}{rest}")); return Some(parts); }
        parts.push(name);
    }
    Some(parts)
}

// Return only option positions for presence checks. A separately consumed value
// must never be reinterpreted as a connection or output flag. Docker's -l is a
// root log-level value but a boolean in list queries, so keep the command boundary.
pub fn options(args: &[String], workspace: Workspace) -> Vec<String> {
    let root = command(args, workspace);
    let command = crate::native::command_index(args, workspace).unwrap_or(args.len());
    let mut options = Vec::new();
    let mut args = args.iter().enumerate();
    while let Some((index, arg)) = args.next() {
        if arg == "--" { options.push(arg.clone()); break; }
        if !arg.starts_with('-') { continue; }
        let query = workspace == Workspace::Containers && index >= command;
        let parts = short_options(arg, workspace, query, root).unwrap_or_else(|| vec![arg.clone()]);
        let consume = parts.last().is_some_and(|last| option_takes_value(last, workspace, query, root));
        options.extend(parts);
        if consume { args.next(); }
    }
    options
}

// Docker connection options belong before its command. kubectl persistent
// options can occur throughout get's argv. Never split a consumed option value.
pub fn inspect(args: &[String], workspace: Workspace) -> Vec<String> {
    let root = command(args, workspace);
    let mut result = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" || (workspace == Workspace::Containers && !arg.starts_with('-')) {
            result.push(arg.clone()); result.extend(args.cloned()); break;
        }
        let parts = short_options(arg, workspace, false, root).unwrap_or_else(|| vec![arg.clone()]);
        let consume = parts.last().is_some_and(|last| takes_value_in(last, workspace, root));
        result.extend(parts);
        if consume { result.extend(args.next().cloned()); }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "run with installed CLI metadata by tests/host/test_native_flag_inventory.py"]
    fn installed_kubectl_flag_inventory() {
        let path = std::env::var("HAMN_KUBECTL_FLAG_INVENTORY").unwrap();
        let inventory: std::collections::BTreeMap<String, std::collections::BTreeMap<String, bool>> =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(inventory.len() > 50);
        for (path, flags) in inventory {
            let root = path.split_whitespace().next().unwrap();
            for (name, consumes) in flags {
                assert_eq!(takes_value_in(&name, Workspace::Kubernetes, root), consumes, "{path} {name}");
                for grouped in [false, true].into_iter().filter(|grouped| !grouped || name.len() == 2) {
                    let mut args = crate::tui_state::split_command(&path).unwrap();
                    args.extend([if grouped { format!("-h{}", &name[1..]) } else { name.clone() },
                        "--namespace=literal".into(), "--context=actual".into()]);
                    let positions = options(&args, Workspace::Kubernetes);
                    assert_eq!(positions.contains(&"--namespace=literal".into()), !consumes, "{path} {name} group={grouped}");
                    assert!(positions.contains(&"--context=actual".into()), "{path} {name}");
                }
            }
        }
    }

    #[test]
    fn inspection_splits_known_groups_without_splitting_values_or_original_argv() {
        for (workspace, input, expected) in [
            (Workspace::Containers, "-DHunix:///socket ps -as", "-D -Hunix:///socket ps -as"),
            (Workspace::Containers, "-DH unix:///socket ps", "-D -H unix:///socket ps"),
            (Workspace::Containers, "--context -DHliteral ps", "--context -DHliteral ps"),
            (Workspace::Kubernetes, "get pods -Ashttps://api -Anteam", "get pods -A -shttps://api -A -nteam"),
            (Workspace::Kubernetes, "get pods -Al -swvalue", "get pods -A -l -swvalue"),
            (Workspace::Kubernetes, "get pods --token -sprivate -Aw=false", "get pods --token -sprivate -A -w=false"),
            (Workspace::Kubernetes, "get pods -- -Asliteral", "get pods -- -Asliteral"),
            (Workspace::Kubernetes, "get pods -éw", "get pods -éw"),
        ] {
            let args = crate::tui_state::split_command(input).unwrap();
            assert_eq!(inspect(&args, workspace), crate::tui_state::split_command(expected).unwrap());
            assert_eq!(args, crate::tui_state::split_command(input).unwrap());
        }
    }
}
