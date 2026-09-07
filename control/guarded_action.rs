use crate::{model::{Failure, Result}, native::Invocation};
use serde_json::{Value, json};

fn required<'a>(row: &'a Value, field: &str) -> Result<&'a str> {
    row.pointer(field).and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= 1024)
        .ok_or_else(|| Failure::new("invalidResponse", format!("selected resource lacks {field}")))
}
fn segment(value: &str) -> Result<&str> {
    if value.is_empty() || [".", ".."].contains(&value) || !value.bytes().all(|c| c.is_ascii_alphanumeric() || b"-._".contains(&c)) {
        return Err(Failure::new("invalidResponse", "unsafe resource identity"));
    }
    Ok(value)
}

pub fn delete(invocation: &mut Invocation, row: &Value) -> Result<()> {
    let uid = required(row, "/metadata/uid")?;
    let version = required(row, "/metadata/resourceVersion")?;
    let name = segment(required(row, "/metadata/name")?)?;
    let kind = required(row, "/kind")?;
    let (plural, namespaced) = match kind {
        "Pod" => ("pods", true), "Deployment" => ("deployments", true),
        "Service" => ("services", true), "Namespace" => ("namespaces", false),
        "Node" => ("nodes", false), "StatefulSet" => ("statefulsets", true),
        "DaemonSet" => ("daemonsets", true), "Event" => ("events", true),
        "Job" => ("jobs", true), "CronJob" => ("cronjobs", true),
        "Ingress" => ("ingresses", true), "PersistentVolumeClaim" => ("persistentvolumeclaims", true),
        _ => return Err(Failure::new("unsupportedOperation", "unsupported resource kind for guarded deletion")),
    };
    let api = required(row, "/apiVersion")?;
    let mut uri = if let Some((group, version)) = api.split_once('/') {
        format!("/apis/{}/{}", segment(group)?, segment(version)?)
    } else { format!("/api/{}", segment(api)?) };
    if namespaced { uri.push_str(&format!("/namespaces/{}", segment(required(row, "/metadata/namespace")?)?)); }
    uri.push_str(&format!("/{plural}/{name}"));
    invocation.args.extend(["delete".into(), "--raw".into(), uri]);
    invocation.body = Some(serde_json::to_vec(&json!({"apiVersion":"v1", "kind":"DeleteOptions",
        "preconditions":{"uid":uid, "resourceVersion":version}})).unwrap());
    Ok(())
}

pub fn restart(invocation: &mut Invocation, row: &Value, resource: &str, name: &str) -> Result<()> {
    let uid = required(row, "/metadata/uid")?;
    let version = required(row, "/metadata/resourceVersion")?;
    let mut annotations = row.pointer("/spec/template/metadata/annotations").cloned().unwrap_or_else(|| json!({}));
    if annotations.is_null() { annotations = json!({}); }
    let map = annotations.as_object_mut().ok_or_else(|| Failure::new("invalidResponse", "invalid pod template annotations"))?;
    map.insert("kubectl.kubernetes.io/restartedAt".into(), jiff::Timestamp::now().to_string().into());
    let patch = json!([
        {"op":"test", "path":"/metadata/uid", "value":uid},
        {"op":"test", "path":"/metadata/resourceVersion", "value":version},
        {"op":"add", "path":"/spec/template/metadata/annotations", "value":annotations}
    ]);
    invocation.args.extend(["patch".into(), resource.into(), name.into(), "--type=json".into(), "--patch".into(), patch.to_string()]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row() -> Value { json!({"apiVersion":"v1", "kind":"Pod", "metadata":{"name":"victim", "namespace":"test", "uid":"uid-a", "resourceVersion":"42"}}) }
    fn invocation() -> Invocation { crate::native::parse("kubectl get pods", &crate::tui_state::State::new(Default::default())).unwrap() }
    #[test]
    fn delete_binds_server_preconditions_and_rejects_missing_identity() {
        let mut command = invocation(); command.args.clear();
        delete(&mut command, &row()).unwrap();
        assert_eq!(command.args, ["delete", "--raw", "/api/v1/namespaces/test/pods/victim"]);
        let body: Value = serde_json::from_slice(command.body.as_ref().unwrap()).unwrap();
        assert_eq!(body["preconditions"], json!({"uid":"uid-a", "resourceVersion":"42"}));
        for field in ["uid", "resourceVersion", "name", "namespace"] {
            let mut value = row(); value["metadata"].as_object_mut().unwrap().remove(field);
            assert!(delete(&mut invocation(), &value).is_err());
        }
        for field in ["name", "namespace"] {
            for invalid in ["..", "a/b", "a?b", "a#b", "a%2fb"] {
                let mut value = row(); value["metadata"][field] = invalid.into();
                assert!(delete(&mut invocation(), &value).is_err());
            }
        }
        let mut value = row(); value["kind"] = "Namespace".into(); value["metadata"].as_object_mut().unwrap().remove("namespace");
        let mut command = invocation(); command.args.clear(); delete(&mut command, &value).unwrap();
        assert_eq!(command.args.last().unwrap(), "/api/v1/namespaces/victim");
    }
    #[test]
    fn restart_tests_identity_atomically_and_preserves_annotations() {
        let mut value = row(); value["spec"] = json!({"template":{"metadata":{"annotations":{"keep":"yes"}}}});
        let mut command = invocation(); command.args.clear(); restart(&mut command, &value, "deployments", "victim").unwrap();
        let patch: Value = serde_json::from_str(command.args.last().unwrap()).unwrap();
        assert_eq!(patch[0], json!({"op":"test", "path":"/metadata/uid", "value":"uid-a"}));
        assert_eq!(patch[1]["value"], "42"); assert_eq!(patch[2]["value"]["keep"], "yes");
    }
}
