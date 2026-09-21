use crate::{
    model::{Failure, Result},
    native::Invocation,
    preferences::Workspace,
};
use serde_json::Value;
#[derive(Clone)]
pub struct Action {
    pub invocation: Invocation,
    pub changes: bool,
    pub description: String,
}
pub(crate) fn connections(
    original: &[String],
    workspace: Workspace,
    row_namespace: Option<&str>,
) -> Vec<String> {
    let inspected = crate::native_flags::inspect(original, workspace);
    let root = crate::native_flags::command(original, workspace);
    let mut args = Vec::new();
    let mut i = 0;
    let docker = workspace == Workspace::Containers;
    let end = if docker {
        crate::native::command_index(&inspected, workspace).unwrap_or(0)
    } else {
        inspected.len()
    };
    while i < end {
        let arg = &inspected[i];
        if arg == "--" {
            break;
        }
        let value: &[&str] = if docker {
            &[
                "--context",
                "-c",
                "--host",
                "-H",
                "--config",
                "--tlscacert",
                "--tlscert",
                "--tlskey",
            ]
        } else {
            crate::native::KUBE_CONNECTION_VALUES
        };
        let boolean: &[&str] = if docker {
            &["--tls", "--tlsverify"]
        } else {
            crate::native::KUBE_CONNECTION_FLAGS
        };
        if let Some(name) = value.iter().find(|name| {
            arg == **name
                || arg.starts_with(&format!("{name}="))
                || (name.len() == 2 && arg.starts_with(**name) && arg.len() > 2)
        }) {
            let skip = row_namespace.is_some() && ["--namespace", "-n"].contains(name);
            if !skip {
                args.push(arg.clone());
            }
            if arg == name {
                i += 1;
                if !skip {
                    if let Some(value) = inspected.get(i) {
                        args.push(value.clone());
                    }
                }
            }
        } else if boolean
            .iter()
            .any(|name| arg == *name || arg.starts_with(&format!("{name}=")))
        {
            args.push(arg.clone());
        } else if crate::native_flags::takes_value_in(arg, workspace, root) {
            i += 1;
        }
        i += 1;
    }
    if let Some(namespace) = row_namespace {
        args.extend(["--namespace".into(), namespace.into()]);
    }
    args
}
pub fn selected(invocation: &Invocation, row: &Value, action: &str) -> Result<Action> {
    let resource = invocation
        .resource
        .as_deref()
        .ok_or_else(|| Failure::new("noSelection", "select a resource list"))?;
    let changes = ["start", "stop", "restart", "delete"].contains(&action);
    let mut result = invocation.clone();
    result.resource = None;
    result.reset_selection = false;
    let name;
    if invocation.workspace == Workspace::Containers {
        name = row["ID"]
            .as_str()
            .or_else(|| row["Id"].as_str())
            .or_else(|| row["Name"].as_str())
            .ok_or_else(|| Failure::new("noSelection", "CLI response has no resource identity"))?;
        result.args = connections(&invocation.args, invocation.workspace, None);
        let singular = match resource {
            "containers" => "container",
            "images" => "image",
            "volumes" => "volume",
            "networks" => "network",
            _ => {
                return Err(Failure::new(
                    "unsupportedOperation",
                    "choose a Docker resource",
                ));
            }
        };
        match action {
            "inspect" => result
                .args
                .extend([singular.into(), "inspect".into(), name.into()]),
            "delete" => result
                .args
                .extend([singular.into(), "rm".into(), name.into()]),
            "start" | "stop" | "restart" if resource == "containers" => {
                result.args.extend([action.into(), name.into()])
            }
            "logs" if resource == "containers" => result.args.extend([
                "logs".into(),
                "--follow".into(),
                "--tail=200".into(),
                "--timestamps".into(),
                name.into(),
            ]),
            "stats" if resource == "containers" => {
                result.args.extend(["stats".into(), name.into()])
            }
            _ => {
                return Err(Failure::new(
                    "unsupportedOperation",
                    "this action does not apply to the selected Docker resource",
                ));
            }
        }
    } else {
        name = row["metadata"]["name"]
            .as_str()
            .ok_or_else(|| Failure::new("noSelection", "CLI response has no resource name"))?;
        let namespace = row["metadata"]["namespace"].as_str();
        result.args = connections(&invocation.args, invocation.workspace, namespace);
        match action {
            "inspect" => result.args.extend([
                "get".into(),
                resource.into(),
                name.into(),
                "-o".into(),
                "yaml".into(),
            ]),
            "delete" => crate::guarded_action::delete(&mut result, row, resource)?,
            "logs" => result.args.extend([
                "logs".into(),
                "--follow".into(),
                "--tail=200".into(),
                "--timestamps".into(),
                format!("{resource}/{name}"),
            ]),
            "stats" if ["pods", "po", "pod"].contains(&resource) => {
                result
                    .args
                    .extend(["top".into(), "pod".into(), name.into()])
            }
            "restart"
                if [
                    "deployments",
                    "deployment",
                    "deploy",
                    "statefulsets",
                    "sts",
                    "daemonsets",
                    "ds",
                ]
                .contains(&resource) =>
            {
                crate::guarded_action::restart(&mut result, row, resource, name)?
            }
            _ => {
                return Err(Failure::new(
                    "unsupportedOperation",
                    "use a kubectl command for this resource action",
                ));
            }
        }
        if let Some(namespace) = namespace {
            result.target = format!("{}  selected namespace: {namespace}", result.target);
        }
    }
    Ok(Action {
        description: format!("{} {resource} {name}\n{}", action, result.target),
        invocation: result,
        changes,
    })
}
/// Menu-generated logs are bounded initially. A chosen container is passed as
/// one argv element; explicitly typed native commands never use this helper.
pub fn logs(
    invocation: &Invocation,
    row: &Value,
    container: Option<&str>,
    previous: bool,
) -> Result<Action> {
    let mut action = selected(invocation, row, "logs")?;
    if invocation.workspace == Workspace::Kubernetes {
        if let Some(container) = container {
            action
                .invocation
                .args
                .extend(["--container".into(), container.into()]);
        }
        if previous {
            action.invocation.args.retain(|arg| arg != "--follow");
            action.invocation.args.push("--previous".into());
        }
    }
    Ok(action)
}
pub fn available(invocation: &Invocation, row: &Value) -> Vec<&'static str> {
    let resource = invocation.resource.as_deref().unwrap_or("");
    if invocation.workspace == Workspace::Containers && resource == "projects" {
        return vec!["related-pods"];
    }
    let mut actions = vec!["inspect"];
    if invocation.workspace == Workspace::Containers {
        if resource == "containers" {
            actions.extend(["logs", "stats"]);
            if row["State"] == "running" || row["State"] == "restarting" {
                actions.extend(["stop", "restart"]);
            } else if row["State"] == "created" || row["State"] == "exited" {
                actions.push("start");
            }
        }
        if ["containers", "images", "volumes", "networks"].contains(&resource) {
            actions.push("delete");
        }
    } else {
        if [
            "pods",
            "pod",
            "po",
            "deployments",
            "deployment",
            "deploy",
            "statefulsets",
            "statefulset",
            "sts",
            "daemonsets",
            "daemonset",
            "ds",
            "jobs",
            "job",
        ]
        .contains(&resource)
        {
            actions.push("logs");
        }
        if ["pods", "pod", "po"].contains(&resource) {
            actions.push("stats");
        }
        if [
            "deployments",
            "deployment",
            "deploy",
            "statefulsets",
            "sts",
            "daemonsets",
            "ds",
        ]
        .contains(&resource)
        {
            actions.push("restart");
        }
        let mut guarded = invocation.clone();
        if crate::guarded_action::delete(&mut guarded, row, resource).is_ok() {
            actions.push("delete");
        }
    }
    if [
        "deployments",
        "deployment",
        "deploy",
        "statefulsets",
        "statefulset",
        "sts",
        "daemonsets",
        "daemonset",
        "ds",
        "jobs",
        "job",
    ]
    .contains(&resource)
    {
        actions.push("related-pods");
    }
    if ["pods", "pod", "po"].contains(&resource) && row["metadata"]["uid"].as_str().is_some() {
        actions.push("related-events");
    }
    if ["services", "service", "svc"].contains(&resource) {
        actions.push("related-endpoints");
    }
    actions
}
/// Related lists carry the displayed connection and row namespace forward.
/// An absent workload selector fails instead of widening the query to all Pods.
pub fn related(invocation: &Invocation, row: &Value, relation: &str) -> Result<Invocation> {
    let mut result = invocation.clone();
    result.body = None;
    result.reset_selection = false;
    if invocation.workspace == Workspace::Containers
        && invocation.resource.as_deref() == Some("projects")
    {
        let name = row["Name"]
            .as_str()
            .ok_or_else(|| Failure::new("noSelection", "Compose project has no name"))?;
        result.args = connections(&invocation.args, invocation.workspace, None);
        result.args.extend([
            "ps".into(),
            "--all".into(),
            "--filter".into(),
            format!("label=com.docker.compose.project={name}"),
        ]);
        result.resource = Some("containers".into());
        return Ok(result);
    }
    let namespace = row["metadata"]["namespace"].as_str();
    result.args = connections(&invocation.args, invocation.workspace, namespace);
    let resource = match relation {
        "related-events" => {
            let uid = row["metadata"]["uid"]
                .as_str()
                .ok_or_else(|| Failure::new("noSelection", "Pod has no UID"))?;
            result.args.extend([
                "get".into(),
                "events".into(),
                "--field-selector".into(),
                format!("involvedObject.uid={uid}"),
            ]);
            "events"
        }
        "related-endpoints" => {
            let name = row["metadata"]["name"]
                .as_str()
                .ok_or_else(|| Failure::new("noSelection", "Service has no name"))?;
            result.args.extend([
                "get".into(),
                "endpointslices".into(),
                "--selector".into(),
                format!("kubernetes.io/service-name={name}"),
            ]);
            "endpointslices"
        }
        "related-pods" => {
            let selector = &row["spec"]["selector"];
            let mut labels: Vec<String> = selector["matchLabels"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(name, value)| {
                    value
                        .as_str()
                        .map(|value| format!("{name}={value}"))
                        .ok_or_else(|| {
                            Failure::new(
                                "invalidResponse",
                                "Workload selector contains a non-string label",
                            )
                        })
                })
                .collect::<Result<_>>()?;
            for expression in selector["matchExpressions"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let key = expression["key"].as_str().ok_or_else(|| {
                    Failure::new("invalidResponse", "Selector expression has no key")
                })?;
                let values = expression["values"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|value| {
                        value.as_str().ok_or_else(|| {
                            Failure::new(
                                "invalidResponse",
                                "Selector expression value is not a string",
                            )
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                labels.push(match expression["operator"].as_str() {
                    Some("In") if !values.is_empty() => format!("{key} in ({})", values.join(",")),
                    Some("NotIn") if !values.is_empty() => {
                        format!("{key} notin ({})", values.join(","))
                    }
                    Some("Exists") => key.into(),
                    Some("DoesNotExist") => format!("!{key}"),
                    _ => {
                        return Err(Failure::new(
                            "invalidResponse",
                            "Invalid workload selector expression",
                        ));
                    }
                });
            }
            if labels.is_empty() {
                return Err(Failure::new(
                    "noSelector",
                    "Workload has no selector; refusing an unrelated all-Pod query",
                ));
            }
            result.args.extend([
                "get".into(),
                "pods".into(),
                "--selector".into(),
                labels.join(","),
            ]);
            "pods"
        }
        _ => {
            return Err(Failure::new(
                "unsupportedOperation",
                "No related resource list for this action",
            ));
        }
    };
    if let Some(namespace) = namespace {
        result.target = format!("{}  selected namespace: {namespace}", result.target);
    }
    result.resource = Some(resource.into());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn related_queries_preserve_namespace_and_fail_closed_without_workload_selector() {
        let mut state = crate::tui_state::State::new(Default::default());
        state.workspace = Workspace::Kubernetes;
        let query = crate::native::parse("get deployments -A --context other", &state).unwrap();
        let row = serde_json::json!({"metadata":{"name":"api","namespace":"work","uid":"owned-uid"},"spec":{"selector":{"matchLabels":{"app":"api"},"matchExpressions":[{"key":"tier","operator":"In","values":["web","api"]}]}}});
        let pods = related(&query, &row, "related-pods").unwrap();
        assert_eq!(
            pods.args,
            [
                "--context",
                "other",
                "--namespace",
                "work",
                "get",
                "pods",
                "--selector",
                "app=api,tier in (web,api)"
            ]
        );
        let events = related(&query, &row, "related-events").unwrap();
        assert!(
            events.args.ends_with(
                &[
                    "get",
                    "events",
                    "--field-selector",
                    "involvedObject.uid=owned-uid"
                ]
                .map(String::from)
            )
        );
        assert!(related(&query, &serde_json::json!({}), "related-pods").is_err());
        let mut custom = query.clone();
        custom.resource = Some("widgets.example.test".into());
        assert_eq!(
            available(
                &custom,
                &serde_json::json!({"metadata":{"uid":"uid","resourceVersion":"1"}})
            ),
            ["inspect"]
        );
        state.workspace = Workspace::Containers;
        let mut projects = crate::native::parse("ps", &state).unwrap();
        projects.resource = Some("projects".into());
        assert_eq!(
            available(&projects, &serde_json::json!({"Name":"sample"})),
            ["related-pods"]
        );
        let containers = related(
            &projects,
            &serde_json::json!({"Name":"sample"}),
            "related-pods",
        )
        .unwrap();
        assert!(
            containers.args.ends_with(
                &[
                    "ps",
                    "--all",
                    "--filter",
                    "label=com.docker.compose.project=sample"
                ]
                .map(String::from)
            )
        );
    }
    #[test]
    fn menu_logs_bound_history_and_preserve_container_and_previous_as_arguments() {
        let mut state = crate::tui_state::State::new(Default::default());
        state.workspace = Workspace::Kubernetes;
        let query = crate::native::parse("get pods -A", &state).unwrap();
        let row = serde_json::json!({"metadata":{"name":"pod","namespace":"actual"}});
        let action = logs(&query, &row, Some("sidecar"), true).unwrap();
        assert!(
            action.invocation.args.ends_with(
                &[
                    "logs",
                    "--tail=200",
                    "--timestamps",
                    "pods/pod",
                    "--container",
                    "sidecar",
                    "--previous"
                ]
                .map(String::from)
            )
        );
        assert!(!action.invocation.args.iter().any(|arg| arg == "--follow"));
        let typed = crate::native::parse("logs pod --tail=7", &state).unwrap();
        assert!(
            !typed
                .args
                .iter()
                .any(|arg| arg == "--tail=200" || arg == "--timestamps")
        );
        state.workspace = Workspace::Containers;
        let query = crate::native::parse("ps", &state).unwrap();
        let running = serde_json::json!({"ID":"id","State":"running"});
        assert!(available(&query, &running).contains(&"stop"));
        assert!(!available(&query, &running).contains(&"start"));
        let stopped = serde_json::json!({"ID":"id","State":"exited"});
        assert!(available(&query, &stopped).contains(&"start"));
        assert!(!available(&query, &stopped).contains(&"stop"));
        for state in ["paused", "dead", "removing", "unknown", ""] {
            let row = serde_json::json!({"ID":"id", "State":state});
            assert!(!available(&query, &row).contains(&"start"), "{state}");
        }
    }
    #[test]
    fn cluster_override_is_visible_without_exposing_consumed_credentials() {
        let mut state = crate::tui_state::State::new(Default::default());
        state.workspace = Workspace::Kubernetes;
        state.request.context = Some("dev-context".into());
        state.request.namespace = Some("test".into());
        let row = serde_json::json!({"apiVersion":"v1", "kind":"Pod", "metadata":{
            "name":"pod", "namespace":"test", "uid":"fixture-uid", "resourceVersion":"1"}});
        for option in ["--cluster alternate", "--cluster=alternate"] {
            let query = crate::native::parse(
                &format!("get pods {option} --token --cluster=secret"),
                &state,
            )
            .unwrap();
            assert!(query.target.contains(option));
            assert!(query.target.contains("--context dev-context"));
            assert!(!query.target.contains("secret"));
            for action in ["inspect", "delete", "logs", "stats"] {
                let selected = selected(&query, &row, action).unwrap();
                assert!(selected.invocation.target.contains(option));
                assert!(selected.description.contains(option));
                assert!(!selected.description.contains("secret"));
                assert!(
                    selected
                        .invocation
                        .args
                        .windows(2)
                        .any(|v| v == ["--token", "--cluster=secret"])
                );
                assert!(selected.invocation.args.join(" ").contains(option));
            }
        }
    }
    #[test]
    fn grouped_targets_survive_defaults_headers_and_selected_actions() {
        let mut state = crate::tui_state::State::new(Default::default());
        state.docker_context = Some("ui-default".into());
        for (text, target) in [
            ("docker -DHunix:///query.sock ps", "-Hunix:///query.sock"),
            ("docker -DH unix:///query.sock ps", "-H unix:///query.sock"),
            ("docker -Dcother ps", "-cother"),
        ] {
            let query = crate::native::parse(text, &state).unwrap();
            assert_eq!(
                query.args,
                crate::tui_state::split_command(text).unwrap()[1..]
            );
            assert_eq!(query.resource.as_deref(), Some("containers"));
            assert!(query.hamn_profile.is_none());
            assert!(query.target.contains(target), "{}", query.target);
            let action =
                selected(&query, &serde_json::json!({"ID":"exact-id"}), "inspect").unwrap();
            let expected =
                crate::tui_state::split_command(&format!("{target} container inspect exact-id"))
                    .unwrap();
            assert_eq!(action.invocation.args, expected);
        }
        state.workspace = Workspace::Kubernetes;
        state.request.context = Some("ui-cluster".into());
        state.request.namespace = Some("ui-ns".into());
        let row = serde_json::json!({"metadata":{"name":"pod", "namespace":"actual"}});
        for flag in [
            "-Ashttps://explicit",
            "-As https://explicit",
            "-As=https://explicit",
        ] {
            let query = crate::native::parse(&format!("get pods {flag}"), &state).unwrap();
            assert_eq!(query.resource.as_deref(), Some("pods"));
            assert!(
                query.target.contains("https://explicit")
                    && query.target.contains("all namespaces")
            );
            assert!(!query.args.iter().any(|a| a == "ui-ns"));
            let action = selected(&query, &row, "inspect").unwrap();
            assert!(
                action
                    .invocation
                    .args
                    .iter()
                    .any(|a| a.contains("https://explicit"))
            );
            assert!(action.invocation.args.ends_with(
                &["--namespace", "actual", "get", "pods", "pod", "-o", "yaml"].map(String::from)
            ));
        }
        let query =
            crate::native::parse("get pods --token -sprivate --selector -slabel", &state).unwrap();
        assert!(!query.target.contains("private") && !query.target.contains("label"));
        let action = selected(&query, &row, "inspect").unwrap();
        assert!(!action.invocation.args.iter().any(|a| a == "-slabel"));
        assert!(
            action
                .invocation
                .args
                .windows(2)
                .any(|a| a == ["--token", "-sprivate"])
        );
    }
    #[test]
    fn selected_actions_preserve_tls_proxy_and_authentication_overrides() {
        let mut state = crate::tui_state::State::new(Default::default());
        state.workspace = Workspace::Kubernetes;
        let row = serde_json::json!({"metadata":{"name":"pod", "namespace":"actual"}});
        for flag in [
            "--tls-server-name",
            "--username",
            "--password",
            "--proxy-url",
            "--as-user-extra",
            "--kuberc",
        ] {
            for option in [
                format!("{flag} fixture-value"),
                format!("{flag}=fixture-value"),
            ] {
                for command in [format!("{option} get pods"), format!("get pods {option}")] {
                    let query = crate::native::parse(&command, &state).unwrap();
                    assert_eq!(query.resource.as_deref(), Some("pods"));
                    let action = selected(&query, &row, "inspect").unwrap();
                    assert!(
                        action
                            .invocation
                            .args
                            .starts_with(&crate::tui_state::split_command(&option).unwrap()),
                        "{command}"
                    );
                }
            }
        }
        let query = crate::native::parse(
            "get pods --as-group first --as-group second --match-server-version",
            &state,
        )
        .unwrap();
        let action = selected(&query, &row, "inspect").unwrap();
        assert_eq!(
            &action.invocation.args[..5],
            [
                "--as-group",
                "first",
                "--as-group",
                "second",
                "--match-server-version"
            ]
        );
    }
    #[test]
    fn selected_actions_use_the_displayed_connection_and_rows_namespace() {
        let mut state = crate::tui_state::State::new(Default::default());
        state.docker_context = Some("old-ui".into());
        let query =
            crate::native::parse("docker --context explicit ps --filter label=work", &state)
                .unwrap();
        let action = selected(&query, &serde_json::json!({"ID":"exact-id"}), "delete").unwrap();
        assert_eq!(
            action.invocation.args,
            ["--context", "explicit", "container", "rm", "exact-id"]
        );
        assert!(action.changes);
        state.workspace = Workspace::Kubernetes;
        let query =
            crate::native::parse("get pods --context other -A -l app=test", &state).unwrap();
        let action = selected(
            &query,
            &serde_json::json!({"metadata":{"name":"pod", "namespace":"actual"}}),
            "inspect",
        )
        .unwrap();
        assert_eq!(
            action.invocation.args,
            [
                "--context",
                "other",
                "--namespace",
                "actual",
                "get",
                "pods",
                "pod",
                "-o",
                "yaml"
            ]
        );
        assert!(!action.changes);
        assert!(selected(&query, &serde_json::json!({}), "delete").is_err());
    }
}
