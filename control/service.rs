use crate::{
    core,
    model::{Failure, OPERATIONS, Request, Result},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

pub async fn execute_stream(
    request: &Request,
    cancel: &CancellationToken,
    events: Option<crate::stream::Events>,
) -> Result<Value> {
    if request.operation() == "capabilities" {
        return Ok(json!({"operations":OPERATIONS.iter().map(|(name, mutation)|
            json!({"name":name,"mutates":mutation})).collect::<Vec<_>>(),
            "arguments":{"profile":"string","context":"string","namespace":"string",
                "name":"string","yes":"boolean","timeout":"seconds","replicas":"integer"},
            "formats":["json","ndjson"]}));
    }
    request.validate()?;
    if request.operation() == "k8s contexts list" {
        return Ok(crate::kubeconfig::contexts(&crate::kubeconfig::load(
            request,
        )?));
    }
    let run = async {
        if request.mutates()
            && matches!(
                request.words.first().map(String::as_str),
                Some("vm" | "docker")
            )
            && !matches!(
                request.operation().as_str(),
                "vm create" | "vm start" | "vm migrate"
            )
        {
            crate::migration::prepare(request.profile.as_deref().unwrap()).await?;
        }
        if request.words.first().is_some_and(|word| word == "docker") {
            let mut status_request = request.clone();
            status_request.words = vec!["vm".into(), "status".into()];
            status_request.follow = false;
            status_request.watch = false;
            let status = core::call(&status_request).await?;
            let socket = status["dockerSocket"]
                .as_str()
                .ok_or_else(|| Failure::new("coreProtocol", "Docker socket missing"))?;
            crate::docker::execute(request, socket, events.as_ref()).await
        } else if request.words.first().is_some_and(|word| word == "k8s") {
            crate::kubernetes::execute(request, events.as_ref()).await
        } else {
            core::call(request).await
        }
    };
    tokio::select! {
        _ = cancel.cancelled() => Err(Failure::new(if request.mutates() {"outcomeUnknown"} else {"cancelled"}, "operation cancelled")),
        result = tokio::time::timeout(std::time::Duration::from_secs(request.timeout), run) => {
            result.map_err(|_| Failure::new(if request.mutates() {"outcomeUnknown"} else {"timeout"}, "operation deadline exceeded"))?
        }
    }
}
