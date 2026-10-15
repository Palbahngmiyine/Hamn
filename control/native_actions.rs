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

const PODS: &[&str] = &["pods", "pod", "po"];
/// Workloads that `kubectl rollout restart` accepts.
const RESTARTABLE: &[&str] = &[
    "deployments",
    "deployment",
    "deploy",
    "statefulsets",
    "sts",
    "daemonsets",
    "ds",
];
/// Resources whose `kubectl port-forward TYPE/NAME` selects a Pod to forward to.
const FORWARDABLE: &[&str] = &[
    "pods",
    "pod",
    "po",
    "services",
    "service",
    "svc",
    "deployments",
    "deployment",
    "deploy",
    "statefulsets",
    "statefulset",
    "sts",
    "replicasets",
    "replicaset",
    "rs",
    "daemonsets",
    "daemonset",
    "ds",
];
/// The shell that `shell` opens, as k9s opens it: bash where the image has
/// it, otherwise sh.
const SHELL: &str = "command -v bash >/dev/null && exec bash || exec sh";

/// Whether the resource of a list offers `action` for its rows at all, as
/// `selected` builds it; the state of a row can still rule it out (see
/// `available`). The keys of the TUI follow this, so a key appears only in
/// lists where it acts.
pub fn offers(invocation: &Invocation, action: &str) -> bool {
    let resource = invocation.resource.as_deref().unwrap_or("");
    if invocation.workspace == Workspace::Containers {
        let any = ["containers", "images", "volumes", "networks"].contains(&resource);
        return match action {
            "describe" | "yaml" | "delete" => any,
            "logs" | "shell" | "stats" | "start" | "stop" | "restart" => resource == "containers",
            _ => false,
        };
    }
    match action {
        "describe" | "yaml" | "json" | "delete" => !resource.is_empty(),
        "logs" => [
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
        .contains(&resource),
        "shell" | "stats" => PODS.contains(&resource),
        "restart" => RESTARTABLE.contains(&resource),
        "port-forward" => FORWARDABLE.contains(&resource),
        _ => false,
    }
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
/// The CLI command of `action` on the selected `row` of the list that
/// `invocation` queried, with the displayed connection and the row's
/// namespace. Changes (start, stop, restart, delete) ask to confirm; reads,
/// a shell and a port-forward run at once. Docker has no describe or YAML:
/// both are `docker inspect`, whose JSON is what Docker shows.
pub fn selected(invocation: &Invocation, row: &Value, action: &str) -> Result<Action> {
    build(invocation, row, action, &[])
}
/// A shell in the selected container or Pod; `container` picks one of a
/// Pod's containers and is passed as one argv element.
pub fn shell(invocation: &Invocation, row: &Value, container: Option<&str>) -> Result<Action> {
    let operands: Vec<String> = match container {
        Some(_) if invocation.workspace == Workspace::Containers => {
            return Err(Failure::new(
                "unsupportedOperation",
                "a Docker container has no containers to choose",
            ));
        }
        Some(container) => vec!["--container".into(), container.into()],
        None => Vec::new(),
    };
    build(invocation, row, "shell", &operands)
}
/// `kubectl port-forward TYPE/NAME` of the selected row with the mappings
/// typed at the prompt: `LOCAL:REMOTE`, `:REMOTE` or `REMOTE`, separated by
/// spaces. REMOTE is a port number or a port name; nothing else, such as an
/// option, reaches kubectl from the prompt.
pub fn port_forward(invocation: &Invocation, row: &Value, ports: &str) -> Result<Action> {
    build(invocation, row, "port-forward", &mappings(ports)?)
}
fn mappings(text: &str) -> Result<Vec<String>> {
    fn number(text: &str) -> Option<u16> {
        // u16::from_str also takes a leading `+`, which kubectl does not.
        (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
            .then(|| text.parse().ok())
            .flatten()
    }
    // A named port is an IANA service name: lowercase letters, digits and
    // inner hyphens, at most 15 characters, at least one letter.
    fn name(text: &str) -> bool {
        (1..=15).contains(&text.len())
            && text
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && text.bytes().any(|b| b.is_ascii_lowercase())
            && !text.starts_with('-')
            && !text.ends_with('-')
    }
    let invalid = |mapping: &str| {
        Failure::new(
            "invalidPorts",
            format!("{mapping}: enter LOCAL:REMOTE, :REMOTE or REMOTE, such as 8080:80"),
        )
    };
    let mappings: Vec<String> = text.split_whitespace().map(String::from).collect();
    if mappings.is_empty() {
        return Err(invalid("no port"));
    }
    for mapping in &mappings {
        let (local, remote) = mapping.split_once(':').unwrap_or(("", mapping));
        let remote_ok = number(remote).is_some_and(|port| port > 0) || name(remote);
        let local_ok = local.is_empty() || number(local).is_some();
        // A bare REMOTE also listens on that port locally, so it is a number.
        let bare_ok = mapping.contains(':') || number(remote).is_some();
        if !(remote_ok && local_ok && bare_ok) {
            return Err(invalid(mapping));
        }
    }
    Ok(mappings)
}
/// The mapping that the port-forward prompt starts with: the first port the
/// row declares (a Service port, or a container port of the Pod or its
/// template), to the same local port. Empty when the row declares none.
/// Fails for a resource that cannot be forwarded.
pub fn forward_ports(invocation: &Invocation, row: &Value) -> Result<String> {
    if invocation.workspace != Workspace::Kubernetes || !offers(invocation, "port-forward") {
        return Err(Failure::new(
            "unsupportedOperation",
            if invocation.workspace == Workspace::Containers {
                "Docker has no port-forward; publish ports when the container is created"
            } else {
                "port-forward applies to Pods, Services and workloads"
            },
        ));
    }
    row["metadata"]["name"]
        .as_str()
        .ok_or_else(|| Failure::new("noSelection", "CLI response has no resource name"))?;
    let port =
        if ["services", "service", "svc"].contains(&invocation.resource.as_deref().unwrap_or("")) {
            row["spec"]["ports"][0]["port"].as_u64()
        } else {
            let spec = if row["spec"]["template"].is_object() {
                &row["spec"]["template"]["spec"]
            } else {
                &row["spec"]
            };
            spec["containers"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|container| container["ports"].as_array().into_iter().flatten())
                .find_map(|port| port["containerPort"].as_u64())
        };
    Ok(port
        .filter(|port| (1..=65535).contains(port))
        .map(|port| format!("{port}:{port}"))
        .unwrap_or_default())
}
fn build(
    invocation: &Invocation,
    row: &Value,
    action: &str,
    operands: &[String],
) -> Result<Action> {
    assert!(
        operands.is_empty() || ["shell", "port-forward"].contains(&action),
        "only a shell and a port-forward take operands: {action}"
    );
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
            "inspect" | "describe" | "yaml" => {
                result
                    .args
                    .extend([singular.into(), "inspect".into(), name.into()])
            }
            "delete" => result
                .args
                .extend([singular.into(), "rm".into(), name.into()]),
            "shell" if resource == "containers" => result.args.extend([
                "exec".into(),
                "--interactive".into(),
                "--tty".into(),
                name.into(),
                "sh".into(),
                "-c".into(),
                SHELL.into(),
            ]),
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
            "inspect" | "yaml" | "json" => result.args.extend([
                "get".into(),
                resource.into(),
                name.into(),
                "-o".into(),
                if action == "json" { "json" } else { "yaml" }.into(),
            ]),
            "describe" => result
                .args
                .extend(["describe".into(), resource.into(), name.into()]),
            "shell" if PODS.contains(&resource) => {
                result.args.extend([
                    "exec".into(),
                    "--stdin".into(),
                    "--tty".into(),
                    format!("{resource}/{name}"),
                ]);
                result.args.extend(operands.iter().cloned());
                result
                    .args
                    .extend(["--".into(), "sh".into(), "-c".into(), SHELL.into()]);
            }
            "port-forward" if FORWARDABLE.contains(&resource) && !operands.is_empty() => {
                result
                    .args
                    .extend(["port-forward".into(), format!("{resource}/{name}")]);
                result.args.extend(operands.iter().cloned());
            }
            "delete" => crate::guarded_action::delete(&mut result, row, resource)?,
            "logs" => result.args.extend([
                "logs".into(),
                "--follow".into(),
                "--tail=200".into(),
                "--timestamps".into(),
                format!("{resource}/{name}"),
            ]),
            "stats" if PODS.contains(&resource) => {
                result
                    .args
                    .extend(["top".into(), "pod".into(), name.into()])
            }
            "restart" if RESTARTABLE.contains(&resource) => {
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
    let mut actions = Vec::new();
    if invocation.workspace == Workspace::Containers {
        // Docker has one detail, `inspect`; `describe` and `yaml` are the same.
        actions.push("inspect");
        if resource == "containers" {
            actions.push("logs");
            // `docker exec` needs a running container.
            if row["State"] == "running" {
                actions.push("shell");
            }
            actions.push("stats");
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
        actions.extend(["describe", "yaml", "json"]);
        for action in ["logs", "shell", "port-forward", "stats", "restart"] {
            // `kubectl exec` needs a running Pod.
            let runs = action != "shell" || row["status"]["phase"] == "Running";
            if offers(invocation, action) && runs {
                actions.push(action);
            }
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
    if PODS.contains(&resource) && row["metadata"]["uid"].as_str().is_some() {
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
            ["describe", "yaml", "json"]
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
            let mut actions: Vec<Action> = [
                "inspect", "delete", "logs", "stats", "describe", "yaml", "json", "shell",
            ]
            .into_iter()
            .map(|action| selected(&query, &row, action).unwrap())
            .collect();
            actions.push(shell(&query, &row, Some("app")).unwrap());
            actions.push(port_forward(&query, &row, "8080:80").unwrap());
            for selected in actions {
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
                    let mut actions: Vec<Action> = ["inspect", "describe", "yaml", "shell"]
                        .into_iter()
                        .map(|action| selected(&query, &row, action).unwrap())
                        .collect();
                    actions.push(port_forward(&query, &row, ":80").unwrap());
                    for action in actions {
                        assert!(
                            action
                                .invocation
                                .args
                                .starts_with(&crate::tui_state::split_command(&option).unwrap()),
                            "{command}: {:?}",
                            action.invocation.args
                        );
                    }
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

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    /// The k9s row keys build the kubectl command of the selected Pod, with
    /// the displayed connection and the Pod's own namespace, and none of
    /// them asks to confirm: only a change does.
    #[test]
    fn kubernetes_describe_yaml_shell_and_port_forward_target_the_selected_pod() {
        let mut state = crate::tui_state::State::new(Default::default());
        state.workspace = Workspace::Kubernetes;
        let query =
            crate::native::parse("get pods --context other -A -l app=test", &state).unwrap();
        let row = serde_json::json!({"metadata":{"name":"pod", "namespace":"actual"}});
        let connection = ["--context", "other", "--namespace", "actual"];
        let expect = |action: Action, command: &[&str]| {
            assert_eq!(
                action.invocation.args,
                strings(&[&connection[..], command].concat())
            );
            assert!(!action.changes, "{command:?}");
            assert!(action.invocation.resource.is_none(), "{command:?}");
        };
        expect(
            selected(&query, &row, "describe").unwrap(),
            &["describe", "pods", "pod"],
        );
        expect(
            selected(&query, &row, "yaml").unwrap(),
            &["get", "pods", "pod", "-o", "yaml"],
        );
        expect(
            selected(&query, &row, "json").unwrap(),
            &["get", "pods", "pod", "-o", "json"],
        );
        expect(
            selected(&query, &row, "shell").unwrap(),
            &[
                "exec", "--stdin", "--tty", "pods/pod", "--", "sh", "-c", SHELL,
            ],
        );
        // A container name is one argument before `--`, whatever it holds.
        expect(
            shell(&query, &row, Some("side car; rm")).unwrap(),
            &[
                "exec",
                "--stdin",
                "--tty",
                "pods/pod",
                "--container",
                "side car; rm",
                "--",
                "sh",
                "-c",
                SHELL,
            ],
        );
        expect(
            port_forward(&query, &row, " 8080:80  :9090 ").unwrap(),
            &["port-forward", "pods/pod", "8080:80", ":9090"],
        );
        // Changes ask to confirm; reads, shells and forwards do not.
        let deployments = crate::native::parse("get deployments --context other", &state).unwrap();
        let workload = serde_json::json!({"apiVersion":"apps/v1", "kind":"Deployment",
            "metadata":{"name":"api", "namespace":"actual", "uid":"uid", "resourceVersion":"1"}});
        assert!(
            selected(&deployments, &workload, "restart")
                .unwrap()
                .changes
        );
        assert!(selected(&deployments, &workload, "delete").unwrap().changes);
        assert!(
            !selected(&deployments, &workload, "describe")
                .unwrap()
                .changes
        );
        assert!(
            port_forward(&deployments, &workload, "8080:80")
                .unwrap()
                .invocation
                .args
                .ends_with(&strings(&["port-forward", "deployments/api", "8080:80"]))
        );
        // A shell is for Pods; a port-forward needs its ports and a target
        // kubectl can forward to.
        assert!(selected(&deployments, &workload, "shell").is_err());
        assert!(selected(&query, &row, "port-forward").is_err());
        let configmaps = crate::native::parse("get configmaps", &state).unwrap();
        assert!(port_forward(&configmaps, &row, "8080:80").is_err());
        assert!(forward_ports(&configmaps, &row).is_err());
        assert!(selected(&query, &serde_json::json!({}), "describe").is_err());
    }

    #[test]
    fn docker_describe_and_yaml_are_inspect_and_a_shell_needs_a_container() {
        let state = crate::tui_state::State::new(Default::default());
        let query =
            crate::native::parse("docker --context explicit ps --filter label=work", &state)
                .unwrap();
        let row = serde_json::json!({"ID":"exact-id", "State":"running"});
        for action in ["inspect", "describe", "yaml"] {
            let action = selected(&query, &row, action).unwrap();
            assert_eq!(
                action.invocation.args,
                ["--context", "explicit", "container", "inspect", "exact-id"]
            );
            assert!(!action.changes);
        }
        let action = shell(&query, &row, None).unwrap();
        assert_eq!(
            action.invocation.args,
            strings(&[
                "--context",
                "explicit",
                "exec",
                "--interactive",
                "--tty",
                "exact-id",
                "sh",
                "-c",
                SHELL
            ])
        );
        assert!(!action.changes);
        assert!(selected(&query, &row, "start").unwrap().changes);
        // A Docker container has no containers, and Docker no port-forward.
        assert!(shell(&query, &row, Some("app")).is_err());
        assert!(port_forward(&query, &row, "8080:80").is_err());
        let refused = forward_ports(&query, &row).unwrap_err();
        assert!(
            refused.message.contains("Docker has no port-forward"),
            "{}",
            refused.message
        );
        let images = crate::native::parse("images", &state).unwrap();
        let image = serde_json::json!({"ID":"sha256:image"});
        assert!(selected(&images, &image, "shell").is_err());
        assert!(
            selected(&images, &image, "describe")
                .unwrap()
                .invocation
                .args
                .ends_with(&strings(&["image", "inspect", "sha256:image"]))
        );
    }

    #[test]
    fn port_prompt_takes_only_local_remote_mappings() {
        let mut state = crate::tui_state::State::new(Default::default());
        state.workspace = Workspace::Kubernetes;
        let query = crate::native::parse("get pods", &state).unwrap();
        let row = serde_json::json!({"metadata":{"name":"pod"}});
        let ports = |text: &str| {
            port_forward(&query, &row, text).map(|action| {
                let args = action.invocation.args;
                let at = args.iter().position(|arg| arg == "pods/pod").unwrap();
                args[at + 1..].to_vec()
            })
        };
        for (text, expected) in [
            ("8080:80", vec!["8080:80"]),
            (":80", vec![":80"]),
            ("80", vec!["80"]),
            ("0:80", vec!["0:80"]),
            ("65535:65535", vec!["65535:65535"]),
            ("8443:https", vec!["8443:https"]),
            ("8080:web-1", vec!["8080:web-1"]),
            ("  8080:80   9090:90 ", vec!["8080:80", "9090:90"]),
        ] {
            assert_eq!(ports(text).unwrap(), strings(&expected), "{text}");
        }
        for text in [
            "",
            "   ",
            "--address=0.0.0.0",
            "8080:80 --address 0.0.0.0",
            "-1",
            "8080:",
            ":",
            "a:80",
            "+80",
            "8080:+80",
            "65536",
            "8080:65536",
            "8080:0",
            "0",
            "8080:80:90",
            "https",
            "8080:Https",
            "8080:-web",
            "8080:web-",
            "8080:1234567890abcdef",
            "8080:80;id",
            "８０",
        ] {
            let error = ports(text).unwrap_err();
            assert_eq!(error.code, "invalidPorts", "{text}");
        }
    }

    #[test]
    fn port_prompt_starts_with_the_first_declared_port() {
        let mut state = crate::tui_state::State::new(Default::default());
        state.workspace = Workspace::Kubernetes;
        let suggest = |command: &str, row: Value| {
            let query = crate::native::parse(command, &state).unwrap();
            forward_ports(&query, &row)
        };
        let pod = serde_json::json!({"metadata":{"name":"pod"}, "spec":{"containers":[
            {"name":"init-less"},
            {"name":"app", "ports":[{"containerPort":8080}, {"containerPort":9090}]},
            {"name":"sidecar", "ports":[{"containerPort":15000}]}]}});
        assert_eq!(suggest("get pods", pod).unwrap(), "8080:8080");
        let service = serde_json::json!({"metadata":{"name":"svc"},
            "spec":{"ports":[{"port":443, "targetPort":8443}, {"port":80}]}});
        assert_eq!(suggest("get svc", service).unwrap(), "443:443");
        let deployment = serde_json::json!({"metadata":{"name":"api"}, "spec":{"template":
            {"spec":{"containers":[{"name":"api", "ports":[{"containerPort":3000}]}]}}}});
        assert_eq!(suggest("get deploy", deployment).unwrap(), "3000:3000");
        // No declared port, or an impossible one, leaves the prompt empty.
        let bare =
            serde_json::json!({"metadata":{"name":"pod"}, "spec":{"containers":[{"name":"app"}]}});
        assert_eq!(suggest("get pods", bare).unwrap(), "");
        let impossible = serde_json::json!({"metadata":{"name":"pod"}, "spec":{"containers":[
            {"name":"app", "ports":[{"containerPort":70000}]}]}});
        assert_eq!(suggest("get pods", impossible).unwrap(), "");
        assert_eq!(
            suggest("get pods", serde_json::json!({})).unwrap_err().code,
            "noSelection"
        );
    }

    #[test]
    fn menu_lists_the_new_reads_and_a_shell_only_for_running_rows() {
        let mut state = crate::tui_state::State::new(Default::default());
        state.workspace = Workspace::Kubernetes;
        let pods = crate::native::parse("get pods", &state).unwrap();
        let running = serde_json::json!({"apiVersion":"v1", "kind":"Pod", "metadata":{"name":"pod",
            "namespace":"work", "uid":"uid", "resourceVersion":"1"}, "status":{"phase":"Running"}});
        assert_eq!(
            available(&pods, &running),
            [
                "describe",
                "yaml",
                "json",
                "logs",
                "shell",
                "port-forward",
                "stats",
                "delete",
                "related-events"
            ]
        );
        let pending = serde_json::json!({"metadata":{"name":"pod"}, "status":{"phase":"Pending"}});
        assert!(!available(&pods, &pending).contains(&"shell"));
        assert!(available(&pods, &pending).contains(&"port-forward"));
        state.workspace = Workspace::Containers;
        let containers = crate::native::parse("ps", &state).unwrap();
        let running = serde_json::json!({"ID":"id", "State":"running"});
        assert_eq!(
            available(&containers, &running),
            [
                "inspect", "logs", "shell", "stats", "stop", "restart", "delete"
            ]
        );
        let exited = serde_json::json!({"ID":"id", "State":"exited"});
        assert!(!available(&containers, &exited).contains(&"shell"));
        // Every action that the menu lists is one the list offers.
        for (query, row) in [(&containers, &running), (&pods, &pending)] {
            for action in available(query, row) {
                if action != "inspect" && !action.starts_with("related-") {
                    assert!(offers(query, action), "{action}");
                }
            }
        }
    }
}
