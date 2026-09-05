use crate::{
    core,
    model::{Failure, OPERATIONS, Request, Result},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

pub async fn execute(request: &Request, cancel: &CancellationToken) -> Result<Value> {
    if request.operation() == "capabilities" {
        return Ok(json!({"operations":OPERATIONS.iter().map(|(name, mutation)|
            json!({"name":name,"mutates":mutation})).collect::<Vec<_>>(),
            "arguments":{"profile":"string","context":"string","namespace":"string",
                "name":"string","yes":"boolean","timeout":"seconds","replicas":"integer"},
            "formats":["json","ndjson"]}));
    }
    request.validate()?;
    let run = core::call(request);
    tokio::select! {
        _ = cancel.cancelled() => Err(Failure::new(if request.mutates() {"outcomeUnknown"} else {"cancelled"}, "operation cancelled")),
        result = tokio::time::timeout(std::time::Duration::from_secs(request.timeout), run) => {
            result.map_err(|_| Failure::new(if request.mutates() {"outcomeUnknown"} else {"timeout"}, "operation deadline exceeded"))?
        }
    }
}
