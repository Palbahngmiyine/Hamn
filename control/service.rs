use crate::{
    core,
    model::{Failure, Request, Result},
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

fn retirement_failure(operation: &str, error: Failure) -> Result<Option<Failure>> {
    if operation == "vm stop" {
        Ok(Some(error))
    } else {
        Err(error)
    }
}

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
    let managed = request.words.first().is_some_and(|word| word == "vm") && request.mutates();
    let mut migration_error = None;
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
            let mut migrate = request.clone();
            migrate.words = vec!["vm".into(), "migrate".into()];
            let migration = core::call_control(&migrate, cancel, events.as_ref()).await;
            if let Err(error) = migration {
                // A failed retirement must never prevent stopping the owned VM.
                // Preserve the failure in the result; its pending marker remains.
                migration_error = retirement_failure(&request.operation(), error)?;
            }
        }
    if cancel.is_cancelled() { return Err(Failure::new("cancelled", "operation cancelled")); }
    let run = async {
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
            let mut result = if managed {
                core::call_control(request, cancel, events.as_ref()).await?
            } else { core::call(request).await? };
            if let Some(error) = migration_error {
                result["migrationError"] = serde_json::json!(error);
            }
            Ok(result)
        }
    };
    if managed { return run.await; }
    tokio::select! {
        _ = cancel.cancelled() => Err(Failure::new(if request.mutates() {"outcomeUnknown"} else {"cancelled"}, "operation cancelled")),
        result = tokio::time::timeout(std::time::Duration::from_secs(request.timeout), run) => {
            result.map_err(|_| Failure::new(if request.mutates() {"outcomeUnknown"} else {"timeout"}, "operation deadline exceeded"))?
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_retirement_can_only_proceed_to_stop_and_retains_its_error() {
        for operation in [
            "vm stop",
            "vm delete",
            "vm configure",
            "docker containers start",
            "docker containers delete",
        ] {
            let result =
                retirement_failure(operation, Failure::new("coreError", "retirement failed"));
            if operation == "vm stop" {
                let retained = result.unwrap().unwrap();
                assert_eq!(retained.code, "coreError");
                assert_eq!(retained.message, "retirement failed");
            } else {
                assert_eq!(result.unwrap_err().code, "coreError");
            }
        }
    }
}
