//! A disposable Kubernetes API for installed-kubectl suites: the port of
//! `test_tui_tls_target.Api`, which `test_tui_kubectl_output` and
//! `test_tui_cluster_target` extended as `DiscoveryApi`. It answers GET
//! only: discovery documents and a single `tls-pod` in namespace `test`.
use super::http::{Reply, Request, Response};
use super::py_http;
use serde_json::{Value, json};

/// The API's answer to `GET target` (the request target, query included).
pub fn discovery(target: &str) -> Response {
    let pod = json!({"apiVersion": "v1", "kind": "Pod", "metadata": {
        "name": "tls-pod", "namespace": "test", "uid": "uid-1", "resourceVersion": "1"}});
    let body = if target.starts_with("/api/v1/namespaces/test/pods/") {
        pod
    } else if target.starts_with("/api/v1/namespaces/test/pods") {
        json!({"apiVersion": "v1", "kind": "PodList", "items": [pod]})
    } else if target.starts_with("/api/v1") {
        json!({"apiVersion": "v1", "kind": "APIResourceList", "groupVersion": "v1",
            "resources": [{"name": "pods", "singularName": "pod", "namespaced": true,
                "kind": "Pod", "verbs": ["get", "list"]}]})
    } else if target.starts_with("/apis") {
        json!({"apiVersion": "v1", "kind": "APIGroupList", "groups": []})
    } else {
        json!({"apiVersion": "v1", "kind": "APIVersions", "versions": ["v1"]})
    };
    json_response(&body)
}

/// A 200 response carrying `value` as JSON with its length.
pub fn json_response(value: &Value) -> Response {
    Response::new(200, value.to_string()).header("Content-Type", "application/json")
}

/// The handler of the base API: `discovery` for GET, 501 otherwise.
pub fn handle(request: &Request) -> Reply {
    py_http::dispatch(request, &["GET"], || discovery(&request.target).into())
}
