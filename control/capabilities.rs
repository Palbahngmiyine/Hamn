use crate::model::{OPERATIONS, Request};
use serde_json::{Value, json};

pub fn describe() -> Value {
    json!({"operations": OPERATIONS.iter().map(|(name, mutation)| operation(name, *mutation)).collect::<Vec<_>>(),
        "formats": ["json", "ndjson"], "schemaVersion": 1,
        "invocation": "hamn --headless <operation> [name] [arguments]",
        "confirmation": "Mutations require explicit targets and --yes. TUI always confirms the target and impact.",
        "legacyRetirement": "Before VM/Docker mutations, pending managed K3s data and local volumes are permanently deleted. Docker data is preserved. Stopped profiles retire on their next start.",
        "cancellation": "Timeout and cancellation do not undo accepted mutations; uncertain mutation results use outcomeUnknown."})
}

fn operation(name: &str, mutation: bool) -> Value {
    let words: Vec<_> = name.split_whitespace().collect();
    let domain = words[0];
    let action = *words.last().unwrap();
    let resource = words.get(1).copied().unwrap_or("");
    let namespaced = domain == "k8s" && !matches!(resource, "contexts" | "namespaces" | "nodes");
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    let mut add = |flag: &str, schema: Value, mandatory: bool| {
        properties.insert(flag.into(), schema);
        if mandatory {
            required.push(flag.to_owned());
        }
    };
    add(
        "timeout",
        json!({"type":"integer", "minimum":1, "maximum":3600, "default":600, "description":"Overall operation timeout in seconds"}),
        false,
    );
    if mutation {
        add("yes", json!({"type":"boolean", "const":true}), true);
    }
    if matches!(domain, "vm" | "docker") && name != "vm list" {
        add(
            "profile",
            json!({"type":"string", "minLength":1, "maxLength":63, "pattern":"^[A-Za-z0-9_-]+$", "not":{"const":"cache"}}),
            true,
        );
    }
    if domain == "k8s" {
        add(
            "kubeconfig",
            json!({"type":"string", "description":"Explicit kubeconfig file; otherwise KUBECONFIG then ~/.kube/config"}),
            false,
        );
        if resource != "contexts" {
            add("context", json!({"type":"string", "minLength":1}), true);
        }
        if namespaced {
            add(
                "namespace",
                json!({"type":"string", "maxLength":63, "pattern":"^[a-z0-9]([-a-z0-9]*[a-z0-9])?$", "description":"Read default: selected context namespace, then default"}),
                mutation,
            );
            if action == "list" {
                add(
                    "all-namespaces",
                    json!({"type":"boolean", "default":false}),
                    false,
                );
            }
        }
    }
    if matches!(domain, "docker" | "k8s") && action != "list" {
        add(
            "name",
            json!({"type":"string", "minLength":1, "maxLength":253, "pattern":"^[A-Za-z0-9_.-]+$", "description":"Resource name or Docker container ID; also accepted as the fourth positional argument"}),
            true,
        );
        if domain == "k8s" {
            add(
                "uid",
                json!({"type":"string", "description":"Reject if the selected resource UID has changed"}),
                false,
            );
        }
    }
    if domain == "vm" && matches!(action, "create" | "configure" | "start") {
        for (flag, unit) in [
            ("cpu", "virtual CPUs"),
            ("memory", "GiB of memory"),
            ("disk", "GiB of disk capacity"),
        ] {
            add(
                flag,
                json!({"type":"integer", "minimum":1, "description":unit}),
                false,
            );
        }
    }
    if name == "vm diagnostics" {
        add(
            "path",
            json!({"type":"string", "description":"New archive path; defaults to a timestamped archive in the current directory; existing files are never overwritten"}),
            false,
        );
    }
    if name == "system update" {
        add(
            "manifest",
            json!({"type":"string", "description":"Release manifest URL; the normal signature and origin policy still applies"}),
            false,
        );
    }
    if action == "scale" {
        add("replicas", json!({"type":"integer", "minimum":0}), true);
    }
    if action == "logs" {
        add(
            "tail",
            json!({"type":"integer", "minimum":0, "maximum":10000, "default":200}),
            false,
        );
        add("follow", json!({"type":"boolean", "default":false}), false);
        if domain == "k8s" {
            add(
                "container",
                json!({"type":"string", "description":"Pod container name"}),
                false,
            );
            add(
                "previous",
                json!({"type":"boolean", "default":false}),
                false,
            );
        }
    }
    if !mutation {
        add(
            "watch",
            json!({"type":"boolean", "default":false, "description":"Repeat snapshots every 2 seconds; mutually exclusive with follow"}),
            false,
        );
    }
    let request = Request {
        words: words.iter().map(|w| (*w).into()).collect(),
        ..Default::default()
    };
    json!({"name":name, "mutates":mutation, "impact":request.impact(),
        "arguments":{"type":"object", "properties":properties, "required":required},
        "output": if action == "logs" {"ndjson log events followed by a result"} else {"json result; ndjson snapshots with --watch"}})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_names_and_mandatory_arguments_match_the_registry() {
        let value = describe();
        let operations = value["operations"].as_array().unwrap();
        assert_eq!(operations.len(), OPERATIONS.len());
        for op in operations {
            let required = op["arguments"]["required"].as_array().unwrap();
            assert_eq!(required.contains(&json!("yes")), op["mutates"] == true);
            for flag in required {
                assert!(!op["arguments"]["properties"][flag.as_str().unwrap()].is_null());
            }
        }
        let scale = operations
            .iter()
            .find(|op| op["name"] == "k8s deployments scale")
            .unwrap();
        for flag in ["yes", "context", "namespace", "name", "replicas"] {
            assert!(
                scale["arguments"]["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(flag))
            );
        }
        let vm = operations
            .iter()
            .find(|op| op["name"] == "vm create")
            .unwrap();
        assert_eq!(
            vm["arguments"]["properties"]["memory"]["description"],
            "GiB of memory"
        );
    }
}
