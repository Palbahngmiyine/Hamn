use crate::{core, model::{Request, Result}, native::{self, Invocation}, preferences::Workspace};
use serde_json::{Value, json};

pub async fn containers() -> Result<Value> {
    let profile_query = Request { words: vec!["vm".into(), "list".into()], timeout: 30, ..Default::default() };
    let profiles = core::call(&profile_query);
    let context_query = Invocation { workspace: Workspace::Containers, args: vec!["context".into(), "ls".into()],
        resource: Some("contexts".into()), target: "Docker configuration".into(), reset_selection: false };
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
