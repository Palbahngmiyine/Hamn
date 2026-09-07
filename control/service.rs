use crate::{
    core,
    model::{Failure, Request, Result},
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

fn retirement_result(operation: &str, result: Result<Value>, cancelled: bool) -> Result<Option<Failure>> {
    let error = match result {
        Err(error) if operation != "vm stop" || cancelled => return Err(error),
        Err(error) => Some(error),
        Ok(_) => None,
    };
    if cancelled { return Err(Failure::new("cancelled", "operation cancelled")); }
    Ok(error)
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
            // Cancellation stops further dispatch, but cannot erase a failure
            // from the retirement that already ran, especially outcomeUnknown.
            migration_error = retirement_result(&request.operation(), migration, cancel.is_cancelled())?;
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
                retirement_result(operation, Err(Failure::new("coreError", "retirement failed")), false);
            if operation == "vm stop" {
                let retained = result.unwrap().unwrap();
                assert_eq!(retained.code, "coreError");
                assert_eq!(retained.message, "retirement failed");
            } else {
                assert_eq!(result.unwrap_err().code, "coreError");
            }
        }
    }
    #[test]
    fn cancellation_after_retirement_preserves_unknown_outcomes_and_diagnostics() {
        for operation in ["vm stop", "vm delete", "docker containers start"] {
            for cancelled in [false, true] {
                for code in ["outcomeUnknown", "operationFailed", "cancelled"] {
                    let result = retirement_result(operation, Err(Failure::new(code, "retirement diagnostic")), cancelled);
                    let error = if operation == "vm stop" && !cancelled {
                        result.unwrap().unwrap()
                    } else { result.unwrap_err() };
                    assert_eq!(error.code, code);
                    assert_eq!(error.message, "retirement diagnostic");
                }
                let result = retirement_result(operation, Ok(Value::Null), cancelled);
                if cancelled { assert_eq!(result.unwrap_err().code, "cancelled"); }
                else { assert!(result.unwrap().is_none()); }
            }
        }
    }
}
