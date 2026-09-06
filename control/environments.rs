use crate::{core, model::{Request, Result}, native::{self, Invocation}, preferences::Workspace};
use serde_json::{Value, json};

pub async fn containers() -> Result<Value> {
    let profile_query = Request { words: vec!["vm".into(), "list".into()], timeout: 30, ..Default::default() };
    let profiles = core::call(&profile_query);
    let context_query = Invocation { workspace: Workspace::Containers, args: vec!["context".into(), "ls".into()],
        hamn_profile: None, resource: Some("contexts".into()), target: "Docker configuration".into(), reset_selection: false };
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

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().enumerate().find_map(|(i, value)| if value == name { args.get(i + 1).cloned() }
        else { value.strip_prefix(&format!("{name}=")).map(String::from) })
}
pub async fn reload(invocation: &Invocation, state: &mut crate::tui_state::State) -> Result<()> {
    let mut query = invocation.clone();
    if invocation.workspace == Workspace::Containers {
        let index = invocation.args.iter().position(|arg| arg == "context").unwrap_or(0);
        query.args.truncate(index); query.args.extend(["context".into(), "ls".into()]); query.resource = Some("contexts".into());
        let contexts = native::query(&query).await?;
        state.docker_context = contexts.as_array().into_iter().flatten().find(|row| row["Current"] == true)
            .and_then(|row| row["Name"].as_str()).map(String::from);
        state.docker_config = flag(&invocation.args, "--config");
        state.request.profile = None;
    } else {
        query.args = flag(&invocation.args, "--kubeconfig").map(|path| vec!["--kubeconfig".into(), path]).unwrap_or_default();
        query.args.extend(["config".into(), "view".into(), "-o".into(), "json".into()]); query.resource = None;
        let configs = native::query(&query).await?;
        let config = &configs[0];
        state.request.kubeconfig = flag(&invocation.args, "--kubeconfig");
        state.request.context = config["current-context"].as_str().filter(|s| !s.is_empty()).map(String::from);
        state.request.namespace = config["contexts"].as_array().into_iter().flatten()
            .find(|c| c["name"].as_str() == state.request.context.as_deref())
            .and_then(|c| c["context"]["namespace"].as_str()).map(String::from);
        if state.request.context.is_none() {
            state.native = None; state.request.words = vec!["k8s".into(), "contexts".into(), "list".into()];
            return Ok(());
        }
    }
    state.native = Some(native::parse("", state)?);
    state.environment_picker = false; state.selected = 0; state.filter.clear(); state.detail = None;
    Ok(())
}
