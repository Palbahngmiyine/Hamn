use crate::{
    core,
    model::{Failure, Request, Result},
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub async fn execute_stream(
    request: &Request,
    cancel: &CancellationToken,
    events: Option<crate::stream::Events>,
) -> Result<Value> {
    if request.operation() == "capabilities" {
        return Ok(crate::capabilities::describe());
    }
    request.validate()?;
    if request.operation() == "k8s contexts list" {
        return Ok(crate::kubeconfig::contexts(&crate::kubeconfig::load(
            request,
        )?));
    }
    // Upgrade workers own a transactional shell/helper tree. Like VM workers,
    // cancellation must reach C's signal forwarding and wait for its result;
    // dropping an outer timeout would kill only the worker and orphan helpers.
    let managed = (request.words.first().is_some_and(|word| word == "vm") && request.mutates())
        || matches!(
            request.operation().as_str(),
            "system upgrade"
        );
    if cancel.is_cancelled() {
        return Err(Failure::new("cancelled", "operation cancelled"));
    }
    let run = async {
        if request.words.first().is_some_and(|word| word == "docker") {
            if request.context.is_some() {
                return crate::docker_context::execute(request, events.as_ref()).await;
            }
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
        } else if managed {
            core::call_control(request, cancel, events.as_ref()).await
        } else {
            core::call(request).await
        }
    };
    if managed {
        return run.await;
    }
    tokio::select! {
        _ = cancel.cancelled() => Err(Failure::new(if request.mutates() {"outcomeUnknown"} else {"cancelled"}, "operation cancelled")),
        result = tokio::time::timeout(std::time::Duration::from_secs(request.timeout), run) => {
            result.map_err(|_| Failure::new(if request.mutates() {"outcomeUnknown"} else {"timeout"}, "operation deadline exceeded"))?
        }
    }
}
