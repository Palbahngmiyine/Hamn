//! Inspect the routing/connection flags without rewriting the CLI's argv.
use crate::preferences::Workspace;

pub const DOCKER_VALUES: &[&str] = &["--context", "-c", "--host", "-H", "--config", "--log-level", "-l", "--tlscacert", "--tlscert", "--tlskey"];
pub fn takes_value(name: &str, workspace: Workspace) -> bool {
    if workspace == Workspace::Containers { DOCKER_VALUES.contains(&name) }
    else { crate::native::KUBE_CONNECTION_VALUES.contains(&name) ||
        ["--selector", "-l", "--field-selector", "--filename", "-f", "--kustomize", "-k", "--sort-by", "--template", "--chunk-size", "--subresource", "--v", "-v", "--vmodule", "--profile", "--profile-output", "--output", "-o", "--label-columns", "-L"].contains(&name) }
}

pub fn short_group(word: &str, workspace: Workspace) -> Option<Vec<String>> {
    if !word.starts_with('-') || word.starts_with("--") || word.len() < 2 { return None; }
    let booleans = if workspace == Workspace::Containers { "Dvh" } else { "Awh" };
    let mut parts = Vec::new();
    for (offset, flag) in word[1..].char_indices() {
        let name = format!("-{flag}");
        let rest = &word[offset + 1 + flag.len_utf8()..];
        if takes_value(&name, workspace) {
            parts.push(format!("{name}{rest}"));
            return Some(parts);
        }
        if !booleans.contains(flag) { return None; }
        if rest.starts_with('=') { parts.push(format!("{name}{rest}")); return Some(parts); }
        parts.push(name);
    }
    Some(parts)
}

// Docker connection options belong before its command. kubectl persistent
// options can occur throughout get's argv. Never split a consumed option value.
pub fn inspect(args: &[String], workspace: Workspace) -> Vec<String> {
    let mut result = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" || (workspace == Workspace::Containers && !arg.starts_with('-')) {
            result.push(arg.clone()); result.extend(args.cloned()); break;
        }
        let parts = short_group(arg, workspace).unwrap_or_else(|| vec![arg.clone()]);
        let consume = parts.last().is_some_and(|last| takes_value(last, workspace));
        result.extend(parts);
        if consume { result.extend(args.next().cloned()); }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
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
