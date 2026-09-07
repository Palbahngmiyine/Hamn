use crate::{model::{Failure, Result}, preferences::Workspace, tui_state::{State, split_command}};
use serde_json::Value;
use std::process::Stdio;
use tokio::io::AsyncReadExt;

#[derive(Clone, Debug)]
pub struct Invocation {
    pub workspace: Workspace,
    pub args: Vec<String>,
    pub target: String,
    pub resource: Option<String>,
    pub hamn_profile: Option<String>,
    pub reset_selection: bool,
    pub body: Option<Vec<u8>>,
}
impl Invocation {
    pub fn program(&self) -> &'static str { if self.workspace == Workspace::Containers { "docker" } else { "kubectl" } }
    pub fn command(&self, structured: bool) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(self.program());
        command.args(&self.args);
        if structured && self.resource.is_some() {
            if self.workspace == Workspace::Containers { command.args(["--format", "{{json .}}"]); }
            else { command.args(["-o", "json"]); }
        }
        command
    }
}
fn has(args: &[String], names: &[&str]) -> bool {
    args.iter().take_while(|s| s.as_str() != "--").any(|s| names.iter().any(|n|
        s == n || s.starts_with(&format!("{n}=")) || (n.len() == 2 && s.starts_with(n) && s.len() > 2)))
}
pub(crate) fn command_index(args: &[String], workspace: Workspace) -> Option<usize> {
    let values = if workspace == Workspace::Containers {
        &["--context", "-c", "--host", "-H", "--config", "--log-level", "-l", "--tlscacert", "--tlscert", "--tlskey"][..]
    } else {
        &["--context", "--kubeconfig", "--namespace", "-n", "--cluster", "--user", "--server", "-s", "--token", "--certificate-authority", "--client-certificate", "--client-key", "--request-timeout", "--as", "--as-group", "--as-uid", "--cache-dir", "--v", "-v"][..]
    };
    let mut i = 0;
    while i < args.len() {
        let word = &args[i];
        if !word.starts_with('-') { return Some(i); }
        if values.contains(&word.as_str()) { i += 2; }
        else if word.contains('=') || values.iter().any(|n| n.len() == 2 && word.starts_with(n) && word.len() > 2) ||
            ["--debug", "-D", "--tls", "--tlsverify", "--insecure-skip-tls-verify", "--disable-compression", "--warnings-as-errors"].contains(&word.as_str()) { i += 1; }
        else { return None; }
    }
    None
}
fn resource(args: &[String], workspace: Workspace) -> Option<String> {
    if has(args, &["--help", "-h"]) || args.iter().any(|s| s == "--") { return None; }
    let i = command_index(args, workspace)?;
    let words: Vec<_> = args[i..].iter().map(String::as_str).collect();
    if workspace == Workspace::Containers {
        if has(args, &["--format", "--quiet", "-q"]) || args.iter().any(|s| s.starts_with('-') && !s.starts_with("--") && s[1..].contains('q')) { return None; }
        match words.as_slice() {
            ["ps", ..] | ["container", "ls" | "ps" | "list", ..] => Some("containers".into()),
            ["images", ..] | ["image", "ls" | "list", ..] => Some("images".into()),
            ["volume", "ls" | "list", ..] => Some("volumes".into()),
            ["network", "ls" | "list", ..] => Some("networks".into()),
            _ => None,
        }
    } else {
        if has(args, &["--output", "-o", "--watch", "-w", "--watch-only", "--raw", "--output-watch-events", "--no-headers", "--show-labels", "--label-columns", "-L", "--show-kind"]) { return None; }
        match words.as_slice() {
            ["get", resource, ..] if ["pods", "po", "pod", "deployments", "deploy", "deployment", "services", "svc", "service", "namespaces", "ns", "nodes", "no", "statefulsets", "sts", "daemonsets", "ds", "events", "jobs", "cronjobs", "ingresses", "pvcs"].contains(resource) => Some((*resource).into()),
            _ => None,
        }
    }
}
fn installed_kubectl_plugin(args: &[String], index: Option<usize>) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(index) = index else { return false; };
    // These are kubectl's built-in command roots, not an implementation of their flags.
    if ["annotate", "api-resources", "api-versions", "apply", "attach", "auth", "autoscale", "certificate", "cluster-info", "completion", "config", "cordon", "cp", "create", "debug", "delete", "describe", "diff", "drain", "edit", "events", "exec", "explain", "expose", "get", "help", "kustomize", "label", "logs", "options", "patch", "plugin", "port-forward", "proxy", "replace", "rollout", "run", "scale", "set", "taint", "top", "uncordon", "version", "wait"].contains(&args[index].as_str()) { return false; }
    let Some(path) = std::env::var_os("PATH") else { return false; };
    let mut candidate = String::from("kubectl");
    for part in &args[index..] {
        if part.starts_with('-') || part.contains('/') { break; }
        candidate.push('-'); candidate.push_str(&part.replace('-', "_"));
        // kubectl also accepts literal hyphens for plugin subcommands.
        for name in [candidate.clone(), candidate.replace('_', "-")] {
            if std::env::split_paths(&path).any(|dir| std::fs::metadata(dir.join(&name)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)) { return true; }
        }
    }
    false
}
pub fn command_workspace(text: &str, fallback: Workspace) -> Workspace {
    match split_command(text).ok().and_then(|words| words.first().cloned()).as_deref() {
        Some("docker" | "vm") => Workspace::Containers,
        Some("kubectl" | "k8s" | "contexts" | "ctx") => Workspace::Kubernetes,
        _ => fallback,
    }
}
pub fn parse(text: &str, state: &State) -> Result<Invocation> {
    let mut args = split_command(text)?;
    let explicit_program = args.first().is_some_and(|s| s == "docker" || s == "kubectl");
    let workspace = match args.first().map(String::as_str) {
        Some("docker") => { args.remove(0); Workspace::Containers },
        Some("kubectl") => { args.remove(0); Workspace::Kubernetes },
        _ => state.workspace,
    };
    if args.is_empty() && !explicit_program { args.push(if workspace == Workspace::Containers { "ps" } else { "get" }.into());
        if workspace == Workspace::Kubernetes { args.push("pods".into()); } }
    if workspace == Workspace::Containers {
        match args.first().map(String::as_str).unwrap_or("") {
            "containers" => args[0] = "ps".into(),
            "volumes" => args.splice(0..1, ["volume".into(), "ls".into()]).for_each(drop),
            "networks" => args.splice(0..1, ["network".into(), "ls".into()]).for_each(drop),
            _ => {},
        }
    } else if ["pods", "po", "deployments", "deploy", "services", "svc", "nodes", "namespaces", "ns", "statefulsets", "sts", "daemonsets", "ds", "jobs", "cronjobs", "ingresses", "pvcs"].contains(&args.first().map(String::as_str).unwrap_or("")) {
        args.insert(0, "get".into());
    }
    let index = command_index(&args, workspace);
    let plugin = workspace == Workspace::Kubernetes && installed_kubectl_plugin(&args, index);
    let config_command = index.is_some_and(|i| args[i] == if workspace == Workspace::Containers { "context" } else { "config" });
    let explicit = if workspace == Workspace::Containers { has(&args[..index.unwrap_or(args.len())], &["--context", "-c", "--host", "-H", "--config"]) }
        else { has(&args, &["--context", "--kubeconfig"]) };
    let mut defaults = Vec::new();
    let mut hamn_profile = None;
    if !config_command && !explicit && !plugin {
        if workspace == Workspace::Containers {
            if let Some(context) = &state.docker_context { defaults.extend(["--context".into(), context.clone()]); }
            else {
                let home = std::env::var("HOME").map_err(|e| Failure::new("configurationInvalid", e))?;
                hamn_profile = Some(state.request.profile.clone().unwrap_or_else(|| "default".into()));
                defaults.extend(["--host".into(), format!("unix://{home}/.hamn/{}/docker.sock", state.request.profile.as_deref().unwrap_or("default"))]);
            }
        } else {
            if let Some(context) = &state.request.context { defaults.extend(["--context".into(), context.clone()]); }
            if !has(&args, &["--namespace", "-n"]) && !has(&args, &["--all-namespaces", "-A"]) {
                if let Some(namespace) = &state.request.namespace { defaults.extend(["--namespace".into(), namespace.clone()]); }
            }
        }
    }
    if workspace == Workspace::Kubernetes && !plugin && !has(&args, &["--kubeconfig"]) {
        if let Some(config) = &state.request.kubeconfig { defaults.extend(["--kubeconfig".into(), config.clone()]); }
    }
    if workspace == Workspace::Containers && !config_command && !has(&args, &["--config"]) {
        if let Some(config) = &state.docker_config { defaults.splice(0..0, ["--config".into(), config.clone()]); }
    }
    defaults.extend(args);
    let resource = resource(&defaults, workspace);
    // Display all connection/scope arguments exactly; credentials are never included in the header.
    let mut target = Vec::new();
    for (i, arg) in defaults.iter().enumerate() {
        for name in ["--context", "-c", "--host", "-H", "--config", "--namespace", "-n", "--kubeconfig", "--server", "-s"] {
            if arg == name { target.push(format!("{name} {}", defaults.get(i + 1).map(String::as_str).unwrap_or(""))); }
            else if arg.starts_with(&format!("{name}=")) || (name.len() == 2 && arg.starts_with(name) && arg.len() > 2) { target.push(arg.clone()); }
        }
    }
    if has(&defaults, &["--all-namespaces", "-A"]) { target.push("all namespaces".into()); }
    Ok(Invocation { workspace, hamn_profile, body: None, args: defaults, target: if plugin { format!("Plugin-defined target / inherited CLI configuration {}", target.join("  ")) } else if target.is_empty() { "CLI environment / configuration".into() } else { target.join("  ") }, resource, reset_selection: config_command })
}
pub fn toggle_all(invocation: &mut Invocation) {
    let showing_all = invocation.args.iter().any(|s| s == "-a" || s == "--all" || s == "--all=true");
    invocation.args.retain(|s| s != "-a" && s != "--all" && !s.starts_with("--all="));
    if !showing_all { invocation.args.push("--all".into()); }
}
pub async fn query(invocation: &Invocation) -> Result<Value> {
    let mut command = invocation.command(true);
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true)
        .spawn().map_err(|e| Failure::new("cliUnavailable", format!("{}: {e}", invocation.program())))?;
    async fn read(reader: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new(); reader.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes).await?;
        if bytes.len() > 16 * 1024 * 1024 { return Err(std::io::Error::other("CLI output exceeds 16 MiB")); }
        Ok(bytes)
    }
    let stdout = child.stdout.take().unwrap(); let stderr = child.stderr.take().unwrap();
    let (status, out, err) = tokio::try_join!(child.wait(), read(stdout), read(stderr)).map_err(|e| Failure::new("cliError", e))?;
    if !status.success() { return Err(Failure::new("cliError", format!("{} exited {}: {}", invocation.program(), status, String::from_utf8_lossy(&err)))); }
    if out.len() > 16 * 1024 * 1024 { return Err(Failure::new("responseTooLarge", "CLI output exceeds 16 MiB")); }
    if invocation.workspace == Workspace::Containers {
        let rows: std::result::Result<Vec<Value>, _> = out.split(|b| *b == b'\n').filter(|line| !line.is_empty()).map(serde_json::from_slice).collect();
        rows.map(Value::Array).map_err(|e| Failure::new("cliProtocol", e))
    } else {
        let value: Value = serde_json::from_slice(&out).map_err(|e| Failure::new("cliProtocol", e))?;
        Ok(if value["items"].is_array() { value["items"].clone() } else { Value::Array(vec![value]) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_arguments_preserve_scope_output_and_plugin_semantics() {
        let mut state = State::new(Default::default());
        assert_eq!(command_workspace("  kubectl\tget pods", Workspace::Containers), Workspace::Kubernetes);
        assert_eq!(command_workspace("docker", Workspace::Kubernetes), Workspace::Containers);
        assert!(parse("docker", &state).unwrap().resource.is_none());
        assert!(parse("kubectl", &state).unwrap().resource.is_none());
        state.docker_context = Some("ui-selected".into());
        for command in ["ps", "docker ps -a --filter 'label=app=api'", "images", "volume ls", "network ls"] {
            let invocation = parse(command, &state).unwrap();
            assert_eq!(&invocation.args[..2], ["--context", "ui-selected"]);
            assert!(invocation.resource.is_some());
        }
        for command in ["ps -aq", "ps --format '{{.Names}}'", "compose up", "buildx build .", "exec -it app sh -c 'echo done'", "logs -f app"] {
            let invocation = parse(command, &state).unwrap();
            assert_eq!(&invocation.args[..2], ["--context", "ui-selected"]);
            assert!(invocation.resource.is_none());
        }
        let mut filtered = parse("ps -a --filter 'label=app=api'", &state).unwrap();
        toggle_all(&mut filtered);
        assert!(!filtered.args.iter().any(|s| s == "-a" || s == "--all"));
        assert!(filtered.args.iter().any(|s| s == "label=app=api"));
        toggle_all(&mut filtered);
        assert_eq!(filtered.args.last().unwrap(), "--all");
        let invocation = parse("docker --host unix:///explicit ps -a", &state).unwrap();
        assert_eq!(invocation.args, ["--host", "unix:///explicit", "ps", "-a"]);
        assert_eq!(parse("docker context use external", &state).unwrap().args, ["context", "use", "external"]);
        state.workspace = Workspace::Kubernetes;
        state.request.context = Some("ui-cluster".into()); state.request.namespace = Some("ui-ns".into());
        assert_eq!(parse("get pods -A", &state).unwrap().args, ["--context", "ui-cluster", "get", "pods", "-A"]);
        assert_eq!(parse("kubectl get pods --context explicit", &state).unwrap().args, ["get", "pods", "--context", "explicit"]);
        assert_eq!(parse("--kubeconfig '/tmp/my config' get pods", &state).unwrap().args, ["--kubeconfig", "/tmp/my config", "get", "pods"]);
        assert!(parse("get pods -oyaml", &state).unwrap().resource.is_none());
        assert!(parse("get pods --watch", &state).unwrap().resource.is_none());
        assert!(parse("custom-plugin --custom-option", &state).unwrap().resource.is_none());
        assert_eq!(parse("kubectl config current-context", &state).unwrap().args, ["config", "current-context"]);
    }
}
