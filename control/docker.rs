use crate::model::{Failure, Request, Result};
use bollard::{API_DEFAULT_VERSION, Docker, query_parameters::*};
use futures_util::TryStreamExt;
use serde_json::{Value, json};

fn failure(error: bollard::errors::Error) -> Failure {
    let code = match &error {
        bollard::errors::Error::DockerResponseServerError {
            status_code: 401, ..
        } => "authenticationFailed",
        bollard::errors::Error::DockerResponseServerError {
            status_code: 403, ..
        } => "permissionDenied",
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        } => "notFound",
        bollard::errors::Error::DockerResponseServerError {
            status_code: 409, ..
        } => "conflict",
        _ => "dockerUnavailable",
    };
    Failure::new(code, error)
}

fn mutation_failure(error: bollard::errors::Error) -> Failure {
    if matches!(
        &error,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 400..=499,
            ..
        }
    ) {
        failure(error)
    } else {
        Failure::new("outcomeUnknown", error)
    }
}

fn confirmation_failure(error: bollard::errors::Error) -> Failure {
    Failure::new(
        "outcomeUnknown",
        format!("mutation accepted; final state could not be read: {error}"),
    )
}

fn value<T: serde::Serialize>(data: T) -> Result<Value> {
    serde_json::to_value(data).map_err(|e| Failure::new("invalidResponse", e))
}

pub async fn execute(
    request: &Request,
    socket: &str,
    events: Option<&crate::stream::Events>,
) -> Result<Value> {
    let docker = Docker::connect_with_unix(socket, 30, API_DEFAULT_VERSION)
        .map_err(failure)?
        .negotiate_version()
        .await
        .map_err(failure)?;
    match request.operation().as_str() {
        "docker containers list" => {
            return value(
                docker
                    .list_containers(Some(ListContainersOptions {
                        all: true,
                        ..Default::default()
                    }))
                    .await
                    .map_err(failure)?,
            );
        }
        "docker images list" => {
            return value(
                docker
                    .list_images(None::<ListImagesOptions>)
                    .await
                    .map_err(failure)?,
            );
        }
        "docker volumes list" => {
            return value(
                docker
                    .list_volumes(None::<ListVolumesOptions>)
                    .await
                    .map_err(failure)?
                    .volumes
                    .unwrap_or_default(),
            );
        }
        "docker networks list" => {
            return value(
                docker
                    .list_networks(None::<ListNetworksOptions>)
                    .await
                    .map_err(failure)?,
            );
        }
        _ => {}
    }
    let name = request
        .name
        .as_deref()
        .ok_or_else(|| Failure::new("invalidRequest", "--name required"))?;
    let inspected = docker
        .inspect_container(name, None)
        .await
        .map_err(failure)?;
    // Resolve mutable names to an immutable container ID before acting.
    let id = inspected
        .id
        .as_deref()
        .ok_or_else(|| Failure::new("invalidResponse", "container ID missing"))?;
    match request.words.last().map(String::as_str) {
        Some("inspect") => value(&inspected),
        Some("logs") => {
            let options = LogsOptions {
                follow: request.follow,
                stdout: true,
                stderr: true,
                timestamps: true,
                tail: request.tail.to_string(),
                ..Default::default()
            };
            let mut logs = docker.logs(id, Some(options));
            let mut lines = Vec::new();
            let mut bytes = 0;
            let mut stream = crate::stream::TextStream::default();
            while let Some(line) = logs.try_next().await.map_err(failure)? {
                let text = line.into_bytes();
                bytes += text.len();
                if !request.follow && bytes > 1024 * 1024 {
                    return Err(Failure::new(
                        "responseTooLarge",
                        "log response exceeds 1 MiB; reduce --tail",
                    ));
                }
                if let Some(events) = events {
                    stream.feed(&text, events).await?;
                } else {
                    lines.push(String::from_utf8_lossy(&text).into_owned());
                }
            }
            if let Some(events) = events {
                stream.finish(events).await?;
                return Ok(json!({"ended":true}));
            }
            Ok(json!({"lines":lines}))
        }
        Some("stats") => {
            let stats = docker
                .stats(
                    id,
                    Some(StatsOptions {
                        stream: false,
                        one_shot: true,
                    }),
                )
                .try_next()
                .await
                .map_err(failure)?;
            value(stats)
        }
        Some("start") => {
            docker
                .start_container(id, None)
                .await
                .map_err(mutation_failure)?;
            value(
                docker
                    .inspect_container(id, None)
                    .await
                    .map_err(confirmation_failure)?,
            )
        }
        Some("stop") => {
            docker
                .stop_container(
                    id,
                    Some(StopContainerOptions {
                        t: Some(10),
                        ..Default::default()
                    }),
                )
                .await
                .map_err(mutation_failure)?;
            value(
                docker
                    .inspect_container(id, None)
                    .await
                    .map_err(confirmation_failure)?,
            )
        }
        Some("restart") => {
            docker
                .restart_container(
                    id,
                    Some(RestartContainerOptions {
                        t: Some(10),
                        ..Default::default()
                    }),
                )
                .await
                .map_err(mutation_failure)?;
            value(
                docker
                    .inspect_container(id, None)
                    .await
                    .map_err(confirmation_failure)?,
            )
        }
        Some("delete") => {
            docker
                .remove_container(
                    id,
                    Some(RemoveContainerOptions {
                        force: false,
                        v: false,
                        link: false,
                    }),
                )
                .await
                .map_err(mutation_failure)?;
            Ok(json!({"deleted":true,"id":id}))
        }
        _ => Err(Failure::new(
            "invalidRequest",
            "unsupported Docker operation",
        )),
    }
}
