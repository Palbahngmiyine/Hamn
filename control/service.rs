use crate::{
    core,
    model::{Failure, Request, Result},
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

fn cancellation_failure(previous: Option<Failure>) -> Failure {
    previous.unwrap_or_else(|| Failure::new("cancelled", "operation cancelled"))
}

fn retirement_result(operation: &str, result: Result<Value>, cancelled: bool) -> Result<Option<Failure>> {
    let error = match result {
        Err(error) if operation != "vm stop" => return Err(error),
        Err(error) => Some(error),
        Ok(_) => None,
    };
    if cancelled { return Err(cancellation_failure(error)); }
    Ok(error)
}

fn after_retirement(result: Result<Value>, previous: Option<Failure>) -> Result<Value> {
    match result {
        Err(error) if error.code == "cancelled" => Err(previous.unwrap_or(error)),
        Err(error) => Err(error),
        Ok(mut value) => {
            if let Some(error) = previous { value["migrationError"] = serde_json::json!(error); }
            Ok(value)
        }
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
            // Cancellation stops further dispatch, but cannot erase a failure
            // from the retirement that already ran, especially outcomeUnknown.
            migration_error = retirement_result(&request.operation(), migration, cancel.is_cancelled())?;
        }
    if cancel.is_cancelled() { return Err(cancellation_failure(migration_error)); }
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
            let result = if managed {
                core::call_control(request, cancel, events.as_ref()).await
            } else { core::call(request).await };
            after_retirement(result, migration_error)
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
    #[test]
    fn cancellation_at_later_dispatch_boundaries_cannot_erase_retirement_uncertainty() {
        let error = Failure::new("outcomeUnknown", "retirement diagnostic");
        let previous = retirement_result("vm stop", Err(error), false).unwrap();
        assert_eq!(cancellation_failure(previous.clone()).code, "outcomeUnknown");
        let result = after_retirement(Err(Failure::new("cancelled", "dispatch cancelled")), previous.clone());
        assert_eq!(result.unwrap_err().message, "retirement diagnostic");
        let result = after_retirement(Err(Failure::new("outcomeUnknown", "stop diagnostic")), previous.clone());
        assert_eq!(result.unwrap_err().message, "stop diagnostic");
        let result = after_retirement(Ok(serde_json::json!({"state":"stopped"})), previous).unwrap();
        assert_eq!(result["migrationError"]["code"], "outcomeUnknown");
        assert_eq!(after_retirement(Err(Failure::new("cancelled", "cancelled")), None).unwrap_err().code, "cancelled");
    }
}
