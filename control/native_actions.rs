use crate::{model::{Failure, Result}, native::Invocation, preferences::Workspace};
use serde_json::Value;
#[derive(Clone)]
pub struct Action { pub invocation: Invocation, pub changes: bool, pub description: String }
fn connections(invocation: &Invocation, row_namespace: Option<&str>) -> Vec<String> {
    let mut args = Vec::new();
    let mut i = 0;
    let docker = invocation.workspace == Workspace::Containers;
    let end = if docker { crate::native::command_index(&invocation.args, invocation.workspace).unwrap_or(0) } else { invocation.args.len() };
    while i < end {
        let arg = &invocation.args[i];
        if arg == "--" { break; }
        let value: &[&str] = if docker { &["--context", "-c", "--host", "-H", "--config", "--tlscacert", "--tlscert", "--tlskey"] } else { &["--context", "--kubeconfig", "--namespace", "-n", "--cluster", "--user", "--server", "-s", "--token", "--certificate-authority", "--client-certificate", "--client-key", "--request-timeout", "--as", "--as-group", "--as-uid", "--cache-dir"] };
        let boolean: &[&str] = if docker { &["--tls", "--tlsverify"] } else { &["--insecure-skip-tls-verify", "--disable-compression", "--warnings-as-errors"] };
        if let Some(name) = value.iter().find(|name| arg == **name || arg.starts_with(&format!("{name}=")) || (name.len() == 2 && arg.starts_with(**name) && arg.len() > 2)) {
            let skip = row_namespace.is_some() && ["--namespace", "-n"].contains(name);
            if !skip { args.push(arg.clone()); }
            if arg == name {
                i += 1;
                if !skip { if let Some(value) = invocation.args.get(i) { args.push(value.clone()); } }
            }
        } else if boolean.iter().any(|name| arg == *name || arg.starts_with(&format!("{name}="))) { args.push(arg.clone()); }
        i += 1;
    }
    if let Some(namespace) = row_namespace { args.extend(["--namespace".into(), namespace.into()]); }
    args
}
pub fn selected(invocation: &Invocation, row: &Value, action: &str) -> Result<Action> {
    let resource = invocation.resource.as_deref().ok_or_else(|| Failure::new("noSelection", "select a resource list"))?;
    let changes = ["start", "stop", "restart", "delete"].contains(&action);
    let mut result = invocation.clone(); result.resource = None; result.reset_selection = false;
    let name;
    if invocation.workspace == Workspace::Containers {
        name = row["ID"].as_str().or_else(|| row["Id"].as_str()).or_else(|| row["Name"].as_str()).ok_or_else(|| Failure::new("noSelection", "CLI response has no resource identity"))?;
        result.args = connections(invocation, None);
        let singular = match resource { "containers" => "container", "images" => "image", "volumes" => "volume", "networks" => "network", _ => return Err(Failure::new("unsupportedOperation", "choose a Docker resource")) };
        match action {
            "inspect" => result.args.extend([singular.into(), "inspect".into(), name.into()]),
            "delete" => result.args.extend([singular.into(), "rm".into(), name.into()]),
            "start" | "stop" | "restart" if resource == "containers" => result.args.extend([action.into(), name.into()]),
            "logs" if resource == "containers" => result.args.extend(["logs".into(), "--follow".into(), name.into()]),
            "stats" if resource == "containers" => result.args.extend(["stats".into(), name.into()]),
            _ => return Err(Failure::new("unsupportedOperation", "this action does not apply to the selected Docker resource")),
        }
    } else {
        name = row["metadata"]["name"].as_str().ok_or_else(|| Failure::new("noSelection", "CLI response has no resource name"))?;
        let namespace = row["metadata"]["namespace"].as_str();
        result.args = connections(invocation, namespace);
        match action {
            "inspect" => result.args.extend(["get".into(), resource.into(), name.into(), "-o".into(), "yaml".into()]),
            "delete" => crate::guarded_action::delete(&mut result, row, resource)?,
            "logs" => result.args.extend(["logs".into(), "--follow".into(), format!("{resource}/{name}")]),
            "stats" if ["pods", "po", "pod"].contains(&resource) => result.args.extend(["top".into(), "pod".into(), name.into()]),
            "restart" if ["deployments", "deployment", "deploy", "statefulsets", "sts", "daemonsets", "ds"].contains(&resource) => crate::guarded_action::restart(&mut result, row, resource, name)?,
            _ => return Err(Failure::new("unsupportedOperation", "use a kubectl command for this resource action")),
        }
        if let Some(namespace) = namespace { result.target = format!("{}  selected namespace: {namespace}", result.target); }
    }
    Ok(Action { description: format!("{} {resource} {name}\n{}", action, result.target), invocation: result, changes })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_actions_use_the_displayed_connection_and_rows_namespace() {
        let mut state = crate::tui_state::State::new(Default::default());
        state.docker_context = Some("old-ui".into());
        let query = crate::native::parse("docker --context explicit ps --filter label=work", &state).unwrap();
        let action = selected(&query, &serde_json::json!({"ID":"exact-id"}), "delete").unwrap();
        assert_eq!(action.invocation.args, ["--context", "explicit", "container", "rm", "exact-id"]);
        assert!(action.changes);
        state.workspace = Workspace::Kubernetes;
        let query = crate::native::parse("get pods --context other -A -l app=test", &state).unwrap();
        let action = selected(&query, &serde_json::json!({"metadata":{"name":"pod", "namespace":"actual"}}), "inspect").unwrap();
        assert_eq!(action.invocation.args, ["--context", "other", "--namespace", "actual", "get", "pods", "pod", "-o", "yaml"]);
        assert!(!action.changes);
        assert!(selected(&query, &serde_json::json!({}), "delete").is_err());
    }
}
