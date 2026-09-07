use crate::{model::{Failure, Result}, native::Invocation, preferences::Workspace};
use serde_json::Value;
#[derive(Clone)]
pub struct Action { pub invocation: Invocation, pub changes: bool, pub description: String }
pub(crate) fn connections(original: &[String], workspace: Workspace, row_namespace: Option<&str>) -> Vec<String> {
    let inspected = crate::native_flags::inspect(original, workspace);
    let mut args = Vec::new();
    let mut i = 0;
    let docker = workspace == Workspace::Containers;
    let end = if docker { crate::native::command_index(&inspected, workspace).unwrap_or(0) } else { inspected.len() };
    while i < end {
        let arg = &inspected[i];
        if arg == "--" { break; }
        let value: &[&str] = if docker { &["--context", "-c", "--host", "-H", "--config", "--tlscacert", "--tlscert", "--tlskey"] } else { crate::native::KUBE_CONNECTION_VALUES };
        let boolean: &[&str] = if docker { &["--tls", "--tlsverify"] } else { crate::native::KUBE_CONNECTION_FLAGS };
        if let Some(name) = value.iter().find(|name| arg == **name || arg.starts_with(&format!("{name}=")) || (name.len() == 2 && arg.starts_with(**name) && arg.len() > 2)) {
            let skip = row_namespace.is_some() && ["--namespace", "-n"].contains(name);
            if !skip { args.push(arg.clone()); }
            if arg == name {
                i += 1;
                if !skip { if let Some(value) = inspected.get(i) { args.push(value.clone()); } }
            }
        } else if boolean.iter().any(|name| arg == *name || arg.starts_with(&format!("{name}="))) { args.push(arg.clone()); }
        else if crate::native_flags::takes_value(arg, workspace) { i += 1; }
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
        result.args = connections(&invocation.args, invocation.workspace, None);
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
        result.args = connections(&invocation.args, invocation.workspace, namespace);
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
    fn grouped_targets_survive_defaults_headers_and_selected_actions() {
        let mut state = crate::tui_state::State::new(Default::default());
        state.docker_context = Some("ui-default".into());
        for (text, target) in [("docker -DHunix:///query.sock ps", "-Hunix:///query.sock"),
            ("docker -DH unix:///query.sock ps", "-H unix:///query.sock"),
            ("docker -Dcother ps", "-cother")] {
            let query = crate::native::parse(text, &state).unwrap();
            assert_eq!(query.args, crate::tui_state::split_command(text).unwrap()[1..]);
            assert_eq!(query.resource.as_deref(), Some("containers"));
            assert!(query.hamn_profile.is_none());
            assert!(query.target.contains(target), "{}", query.target);
            let action = selected(&query, &serde_json::json!({"ID":"exact-id"}), "inspect").unwrap();
            let expected = crate::tui_state::split_command(&format!("{target} container inspect exact-id")).unwrap();
            assert_eq!(action.invocation.args, expected);
        }
        state.workspace = Workspace::Kubernetes;
        state.request.context = Some("ui-cluster".into());
        state.request.namespace = Some("ui-ns".into());
        let row = serde_json::json!({"metadata":{"name":"pod", "namespace":"actual"}});
        for flag in ["-Ashttps://explicit", "-As https://explicit", "-As=https://explicit"] {
            let query = crate::native::parse(&format!("get pods {flag}"), &state).unwrap();
            assert_eq!(query.resource.as_deref(), Some("pods"));
            assert!(query.target.contains("https://explicit") && query.target.contains("all namespaces"));
            assert!(!query.args.iter().any(|a| a == "ui-ns"));
            let action = selected(&query, &row, "inspect").unwrap();
            assert!(action.invocation.args.iter().any(|a| a.contains("https://explicit")));
            assert!(action.invocation.args.ends_with(&["--namespace", "actual", "get", "pods", "pod", "-o", "yaml"].map(String::from)));
        }
        let query = crate::native::parse("get pods --token -sprivate --selector -slabel", &state).unwrap();
        assert!(!query.target.contains("private") && !query.target.contains("label"));
        let action = selected(&query, &row, "inspect").unwrap();
        assert!(!action.invocation.args.iter().any(|a| a == "-slabel"));
        assert!(action.invocation.args.windows(2).any(|a| a == ["--token", "-sprivate"]));
    }
    #[test]
    fn selected_actions_preserve_tls_proxy_and_authentication_overrides() {
        let mut state = crate::tui_state::State::new(Default::default());
        state.workspace = Workspace::Kubernetes;
        let row = serde_json::json!({"metadata":{"name":"pod", "namespace":"actual"}});
        for flag in ["--tls-server-name", "--username", "--password", "--proxy-url", "--as-user-extra", "--kuberc"] {
            for option in [format!("{flag} fixture-value"), format!("{flag}=fixture-value")] {
                for command in [format!("{option} get pods"), format!("get pods {option}")] {
                    let query = crate::native::parse(&command, &state).unwrap();
                    assert_eq!(query.resource.as_deref(), Some("pods"));
                    let action = selected(&query, &row, "inspect").unwrap();
                    assert!(action.invocation.args.starts_with(&crate::tui_state::split_command(&option).unwrap()), "{command}");
                }
            }
        }
        let query = crate::native::parse("get pods --as-group first --as-group second --match-server-version", &state).unwrap();
        let action = selected(&query, &row, "inspect").unwrap();
        assert_eq!(&action.invocation.args[..5], ["--as-group", "first", "--as-group", "second", "--match-server-version"]);
    }
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
