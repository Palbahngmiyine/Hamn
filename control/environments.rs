use crate::{core, model::{Failure, Request, Result}, native::{self, Invocation}, preferences::Workspace};
use serde_json::{Value, json};

pub async fn containers(config: Option<&str>) -> Result<Value> {
    let profile_query = Request { words: vec!["vm".into(), "list".into()], timeout: 30, ..Default::default() };
    let profiles = core::call(&profile_query);
    let mut args = config.map(|path| vec!["--config".into(), path.into()]).unwrap_or_default();
    args.extend(["context".into(), "ls".into()]);
    let context_query = Invocation { workspace: Workspace::Containers, args,
        hamn_profile: None, resource: Some("contexts".into()), target: "Docker configuration".into(), reset_selection: false, body: None };
    let (profiles, contexts) = tokio::join!(profiles, native::query(&context_query));
    let mut rows = Vec::new();
    match profiles {
        Ok(Value::Array(profiles)) => for mut profile in profiles { profile["environmentKind"] = "hamn".into(); rows.push(profile); },
        Err(error) => rows.push(json!({"name":"Hamn profiles unavailable", "reason":error.message, "disabled":true})),
        _ => {},
    }
    match contexts {
        Ok(Value::Array(contexts)) => for context in contexts {
            rows.push(json!({"name":context["Name"], "environmentKind":"docker", "endpoint":context["DockerEndpoint"],
                "reason":context["Description"], "current":context["Current"]}));
        },
        Err(error) => rows.push(json!({"name":"Docker contexts unavailable", "reason":error.message, "disabled":true})),
        _ => {},
    }
    Ok(Value::Array(rows))
}

fn flag(args: &[String], name: &str, workspace: Workspace) -> Option<String> {
    let root = crate::native_flags::command(args, workspace);
    let inspected = crate::native_flags::inspect(args, workspace);
    let mut args = inspected.iter();
    let mut selected = None;
    while let Some(value) = args.next() {
        if value == "--" { break; }
        if value == name { selected = args.next().cloned(); }
        else if let Some(value) = value.strip_prefix(&format!("{name}=")) { selected = Some(value.into()); }
        else if crate::native_flags::takes_value_in(value, workspace, root) { args.next(); }
    }
    selected
}
fn docker_context_query(invocation: &Invocation) -> Result<Invocation> {
    let index = native::command_index(&invocation.args, Workspace::Containers)
        .ok_or_else(|| Failure::new("cliProtocol", "Cannot locate Docker context command"))?;
    let mut query = invocation.clone();
    query.args.truncate(index);
    query.args.extend(["context".into(), "ls".into()]);
    query.resource = Some("contexts".into());
    Ok(query)
}
// Reload observes CLI configuration without mutating a screen. The owning UI
// job may discard this result on navigation; only a current result is applied.
pub enum Selection {
    Docker { context: Option<String>, config: Option<String> },
    Kubernetes { context: Option<String>, namespace: Option<String>, config: Option<String> },
}
pub async fn reload(invocation: &Invocation) -> Result<Selection> {
    if invocation.workspace == Workspace::Containers {
        let query = docker_context_query(invocation)?;
        let contexts = native::query(&query).await?;
        let context = contexts.as_array().into_iter().flatten().find(|row| row["Current"] == true)
            .and_then(|row| row["Name"].as_str()).map(String::from);
        Ok(Selection::Docker { context,
            config: flag(&query.args[..query.args.len() - 2], "--config", Workspace::Containers) })
    } else {
        let mut query = invocation.clone();
        query.args = flag(&invocation.args, "--kubeconfig", Workspace::Kubernetes).map(|path| vec!["--kubeconfig".into(), path]).unwrap_or_default();
        query.args.extend(["config".into(), "view".into(), "-o".into(), "json".into()]); query.resource = None;
        let configs = native::query(&query).await?;
        let config = &configs[0];
        let context = config["current-context"].as_str().filter(|s| !s.is_empty()).map(String::from);
        let namespace = config["contexts"].as_array().into_iter().flatten()
            .find(|c| c["name"].as_str() == context.as_deref())
            .and_then(|c| c["context"]["namespace"].as_str()).map(String::from);
        Ok(Selection::Kubernetes { context, namespace,
            config: flag(&invocation.args, "--kubeconfig", Workspace::Kubernetes) })
    }
}
impl Selection {
    pub fn apply(self, state: &mut crate::tui_state::State) -> Result<()> {
        match self {
            Self::Docker { context, config } => {
                assert_eq!(state.workspace, Workspace::Containers);
                state.docker_context = context; state.docker_config = config;
                state.request.profile = None;
            },
            Self::Kubernetes { context, namespace, config } => {
                assert_eq!(state.workspace, Workspace::Kubernetes);
                state.request.context = context; state.request.namespace = namespace;
                state.request.kubeconfig = config;
            },
        }
        state.environment_picker = false;
        if state.workspace == Workspace::Kubernetes && state.request.context.is_none() {
            state.native = None; state.request.words = vec!["k8s".into(), "contexts".into(), "list".into()];
            return Ok(());
        }
        state.native = Some(native::parse("", state)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_reload_keeps_command_boundary_and_last_config() {
        let state = crate::tui_state::State::new(Default::default());
        for (command, expected, path) in [
            ("docker --config context context show", vec!["--config", "context", "context", "ls"], "context"),
            ("docker --config first --config last context show", vec!["--config", "first", "--config", "last", "context", "ls"], "last"),
            ("docker --config first --config=last context use shared", vec!["--config", "first", "--config=last", "context", "ls"], "last"),
            ("docker --config --config=literal context show", vec!["--config", "--config=literal", "context", "ls"], "--config=literal"),
        ] {
            let invocation = native::parse(command, &state).unwrap();
            let query = docker_context_query(&invocation).unwrap();
            assert_eq!(query.args, expected);
            assert_eq!(flag(&query.args, "--config", Workspace::Containers).as_deref(), Some(path));
        }
        let args = ["--kubeconfig", "first", "config", "view", "--kubeconfig=last"].map(String::from);
        assert_eq!(flag(&args, "--kubeconfig", Workspace::Kubernetes).as_deref(), Some("last"));
        for (input, workspace, name, expected) in [
            ("--config actual --tlskey --config=literal context show", Workspace::Containers, "--config", "actual"),
            ("--config actual -Dl --config=literal context show", Workspace::Containers, "--config", "actual"),
            ("config view --raw --kubeconfig actual", Workspace::Kubernetes, "--kubeconfig", "actual"),
            ("config set-credentials user --kubeconfig actual --exec-arg --kubeconfig=literal", Workspace::Kubernetes, "--kubeconfig", "actual"),
        ] {
            assert_eq!(flag(&crate::tui_state::split_command(input).unwrap(), name, workspace).as_deref(), Some(expected));
        }
    }
}
