use crate::{
    kubeconfig,
    model::{Failure, Request, Result},
};
use kube::{
    Api,
    api::{DeleteParams, DynamicObject, ListParams, LogParams, Patch, PatchParams, Preconditions},
    core::{ApiResource, GroupVersionKind},
};
use serde_json::{Value, json};

pub fn failure(error: kube::Error) -> Failure {
    let code = match &error {
        kube::Error::Api(response) => match response.code {
            401 => "authenticationFailed",
            403 => "permissionDenied",
            404 => "notFound",
            409 => "conflict",
            410 => "resourceVersionExpired",
            _ => "kubernetesError",
        },
        _ => "kubernetesUnavailable",
    };
    Failure::new(code, error)
}

fn mutation_failure(error: kube::Error) -> Failure {
    if matches!(&error, kube::Error::Api(response) if (400..500).contains(&response.code)) {
        failure(error)
    } else {
        Failure::new("outcomeUnknown", error)
    }
}

fn resource(name: &str) -> Result<(ApiResource, bool)> {
    let (group, version, kind, plural, cluster) = match name {
        "pods" => ("", "v1", "Pod", "pods", false),
        "namespaces" => ("", "v1", "Namespace", "namespaces", true),
        "nodes" => ("", "v1", "Node", "nodes", true),
        "services" => ("", "v1", "Service", "services", false),
        "events" => ("", "v1", "Event", "events", false),
        "pvcs" => (
            "",
            "v1",
            "PersistentVolumeClaim",
            "persistentvolumeclaims",
            false,
        ),
        "deployments" => ("apps", "v1", "Deployment", "deployments", false),
        "statefulsets" => ("apps", "v1", "StatefulSet", "statefulsets", false),
        "daemonsets" => ("apps", "v1", "DaemonSet", "daemonsets", false),
        "jobs" => ("batch", "v1", "Job", "jobs", false),
        "cronjobs" => ("batch", "v1", "CronJob", "cronjobs", false),
        "ingresses" => ("networking.k8s.io", "v1", "Ingress", "ingresses", false),
        _ => {
            return Err(Failure::new(
                "invalidRequest",
                "unknown Kubernetes resource",
            ));
        }
    };
    Ok((
        ApiResource::from_gvk_with_plural(&GroupVersionKind::gvk(group, version, kind), plural),
        cluster,
    ))
}

pub async fn execute(request: &Request, events: Option<&crate::stream::Events>) -> Result<Value> {
    let config = kubeconfig::load(request)?;
    let (client, namespace) = kubeconfig::client(request, config).await?;
    let (resource, cluster) = resource(&request.words[1])?;
    let api: Api<DynamicObject> = if cluster || request.all_namespaces {
        Api::all_with(client.clone(), &resource)
    } else {
        Api::namespaced_with(client.clone(), &namespace, &resource)
    };
    let action = request.words[2].as_str();
    if action == "list" {
        let mut parameters = ListParams::default().limit(500);
        let mut objects = Vec::new();
        let mut pages = std::collections::HashSet::new();
        loop {
            let page = api.list(&parameters).await.map_err(failure)?;
            objects.extend(page.items);
            if objects.len() > 20000 {
                return Err(Failure::new(
                    "responseTooLarge",
                    "narrow the namespace selection",
                ));
            }
            match page.metadata.continue_.filter(|token| !token.is_empty()) {
                Some(token) => {
                    if pages.len() >= 1024 || !pages.insert(token.clone()) {
                        return Err(Failure::new(
                            "invalidResponse",
                            "Kubernetes pagination did not advance",
                        ));
                    }
                    parameters.continue_token = Some(token);
                }
                None => break,
            }
        }
        return Ok(json!(objects));
    }
    let name = request
        .name
        .as_deref()
        .ok_or_else(|| Failure::new("invalidRequest", "--name required"))?;
    let object = api.get(name).await.map_err(failure)?;
    if request
        .uid
        .as_ref()
        .is_some_and(|uid| object.metadata.uid.as_ref() != Some(uid))
    {
        return Err(Failure::new(
            "conflict",
            "resource was replaced; refresh before acting",
        ));
    }
    if request.mutates()
        && (object.metadata.uid.as_ref().is_none_or(String::is_empty)
            || object
                .metadata
                .resource_version
                .as_ref()
                .is_none_or(String::is_empty))
    {
        return Err(Failure::new(
            "invalidResponse",
            "resource identity or version missing",
        ));
    }
    match action {
        "inspect" => Ok(json!({"object":object,"yaml":serde_yaml::to_string(&object)
            .map_err(|e| Failure::new("invalidResponse", e))?})),
        "logs" => {
            let pods: Api<k8s_openapi::api::core::v1::Pod> = Api::namespaced(client, &namespace);
            let parameters = LogParams {
                follow: request.follow,
                container: request.container.clone(),
                previous: request.previous,
                tail_lines: Some(request.tail.into()),
                limit_bytes: if request.follow {
                    None
                } else {
                    Some(1024 * 1024)
                },
                timestamps: true,
                ..Default::default()
            };
            if let Some(events) = events {
                use futures_util::io::AsyncReadExt;
                let mut source = pods.log_stream(name, &parameters).await.map_err(failure)?;
                let mut stream = crate::stream::TextStream::default();
                let mut bytes = [0; 8192];
                let mut total = 0usize;
                loop {
                    let count = source
                        .read(&mut bytes)
                        .await
                        .map_err(|e| Failure::new("streamDisconnected", e))?;
                    if count == 0 {
                        break;
                    }
                    total += count;
                    if !request.follow && total > 1024 * 1024 {
                        return Err(Failure::new(
                            "responseTooLarge",
                            "log response exceeds 1 MiB",
                        ));
                    }
                    stream.feed(&bytes[..count], events).await?;
                }
                stream.finish(events).await?;
                return Ok(json!({"ended":true}));
            }
            let logs = pods.logs(name, &parameters).await.map_err(failure)?;
            Ok(json!({"lines":logs.lines().collect::<Vec<_>>()}))
        }
        "delete" => {
            let parameters = DeleteParams {
                preconditions: Some(Preconditions {
                    uid: object.metadata.uid.clone(),
                    resource_version: object.metadata.resource_version.clone(),
                }),
                ..Default::default()
            };
            api.delete(name, &parameters)
                .await
                .map_err(mutation_failure)?;
            Ok(json!({"accepted":true,"uid":object.metadata.uid}))
        }
        "scale" => {
            let body = json!({"metadata":{"resourceVersion":object.metadata.resource_version},
                "spec":{"replicas":request.replicas}});
            Ok(json!(
                api.patch_scale(name, &PatchParams::default(), &Patch::Merge(&body))
                    .await
                    .map_err(mutation_failure)?
            ))
        }
        "restart" => {
            let body = json!({"metadata":{"resourceVersion":object.metadata.resource_version},
                "spec":{"template":{"metadata":{"annotations":{"kubectl.kubernetes.io/restartedAt":
                    k8s_openapi::jiff::Timestamp::now().to_string()}}}}});
            Ok(json!(
                api.patch(name, &PatchParams::default(), &Patch::Merge(&body))
                    .await
                    .map_err(mutation_failure)?
            ))
        }
        _ => Err(Failure::new(
            "invalidRequest",
            "unsupported Kubernetes operation",
        )),
    }
}
