use crate::model::{Failure, Request, Result};
use kube::{
    Client, Config,
    config::{ExecInteractiveMode, KubeConfigOptions, Kubeconfig},
};
use serde_json::{Value, json};
use std::path::PathBuf;

fn retired(config: &Kubeconfig, name: &str) -> bool {
    let profile = if name == "hamn" {
        "default"
    } else if let Some(value) = name.strip_prefix("hamn-") {
        value
    } else {
        return false;
    };
    if profile.is_empty()
        || profile.len() >= 64
        || !profile
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
    {
        return false;
    }
    let Some(context) = config
        .contexts
        .iter()
        .find(|entry| entry.name == name)
        .and_then(|entry| entry.context.as_ref())
    else {
        return false;
    };
    if context.cluster != name || context.user.as_deref() != Some(name) {
        return false;
    }
    let Some(server) = config
        .clusters
        .iter()
        .find(|entry| entry.name == name)
        .and_then(|entry| entry.cluster.as_ref())
        .and_then(|cluster| cluster.server.as_deref())
    else {
        return false;
    };
    if !server
        .strip_prefix("https://127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .is_some_and(|port| (16443..17467).contains(&port))
    {
        return false;
    }
    let Some(home) = std::env::var_os("HOME") else {
        return false;
    };
    let root = PathBuf::from(home).join(".hamn");
    // An unsafe marker still fails closed for this exact legacy local endpoint.
    [".kube-contexts", ".retired-kube-contexts"]
        .iter()
        .any(|directory| std::fs::symlink_metadata(root.join(directory).join(profile)).is_ok())
}

pub fn load(request: &Request) -> Result<Kubeconfig> {
    let environment = std::env::var_os("KUBECONFIG").filter(|p| !p.is_empty());
    let using_default = request.kubeconfig.is_none() && environment.is_none();
    let paths: Vec<PathBuf> = if let Some(path) = &request.kubeconfig {
        vec![path.into()]
    } else if let Some(paths) = environment {
        std::env::split_paths(&paths)
            .filter(|p| !p.as_os_str().is_empty())
            .collect()
    } else {
        vec![
            PathBuf::from(
                std::env::var_os("HOME")
                    .ok_or_else(|| Failure::new("configurationInvalid", "HOME is not set"))?,
            )
            .join(".kube/config"),
        ]
    };
    if paths.len() > 32 {
        return Err(Failure::new(
            "configurationInvalid",
            "too many kubeconfig files",
        ));
    }
    let mut config = Kubeconfig::default();
    let mut bytes = 0;
    for path in paths {
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && using_default => {
                continue;
            }
            Err(_) => {
                return Err(Failure::new(
                    "configurationInvalid",
                    "cannot read kubeconfig file",
                ));
            }
        };
        bytes += metadata.len();
        if !metadata.is_file() || bytes > 4 * 1024 * 1024 {
            return Err(Failure::new(
                "configurationInvalid",
                "kubeconfig exceeds the 4 MiB limit or is not a file",
            ));
        }
        let next = Kubeconfig::read_from(&path).map_err(|_| {
            Failure::new(
                "configurationInvalid",
                "invalid kubeconfig; credentials are omitted from this error",
            )
        })?;
        config = config
            .merge(next)
            .map_err(|_| Failure::new("configurationInvalid", "cannot merge kubeconfig files"))?;
    }
    Ok(config)
}

pub fn contexts(config: &Kubeconfig) -> Value {
    json!(
        config
            .contexts
            .iter()
            .map(|entry| {
                let context = entry.context.as_ref();
                json!({"name":entry.name,"cluster":context.map(|v| &v.cluster),
            "namespace":context.and_then(|v| v.namespace.as_deref()).unwrap_or("default"),
            "current":config.current_context.as_ref() == Some(&entry.name),
            "available":!retired(config, &entry.name),
            "reason":if retired(config, &entry.name) {Some("managedK3sRemoved")} else {None}})
            })
            .collect::<Vec<_>>()
    )
}

pub async fn client(request: &Request, mut config: Kubeconfig) -> Result<(Client, String)> {
    let selected = request
        .context
        .as_ref()
        .ok_or_else(|| Failure::new("invalidRequest", "--context required"))?;
    if retired(&config, selected) {
        return Err(Failure::new(
            "managedK3sRemoved",
            "this legacy Hamn context is unavailable; the source kubeconfig has been preserved",
        ));
    }
    let context = config
        .contexts
        .iter()
        .find(|c| &c.name == selected)
        .and_then(|c| c.context.as_ref())
        .ok_or_else(|| Failure::new("contextNotFound", "selected context is unavailable"))?;
    let user = context.user.clone();
    for info in &mut config.auth_infos {
        if user.as_ref() != Some(&info.name) {
            continue;
        }
        if let Some(exec) = info.auth_info.as_mut().and_then(|i| i.exec.as_mut()) {
            if exec.interactive_mode == Some(ExecInteractiveMode::Always) {
                return Err(Failure::new(
                    "authenticationRequired",
                    "authenticate outside Hamn before using this context",
                ));
            }
            // Neither frontend lets an auth subprocess consume terminal input.
            exec.interactive_mode = Some(ExecInteractiveMode::Never);
        }
    }
    let options = KubeConfigOptions {
        context: Some(selected.clone()),
        ..Default::default()
    };
    let mut config = Config::from_custom_kubeconfig(config, &options)
        .await
        .map_err(|_| {
            Failure::new(
                "configurationInvalid",
                "cannot configure the selected Kubernetes context",
            )
        })?;
    config.connect_timeout = Some(std::time::Duration::from_secs(10));
    config.read_timeout = Some(std::time::Duration::from_secs(30));
    // A transient response must not silently replay a user-approved mutation.
    config.default_retry = false;
    let namespace = request
        .namespace
        .clone()
        .unwrap_or_else(|| config.default_namespace.clone());
    let client = Client::try_from(config).map_err(|_| {
        Failure::new(
            "authenticationFailed",
            "cannot initialize Kubernetes authentication",
        )
    })?;
    Ok((client, namespace))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_listing_omits_credentials_and_preserves_current_context() {
        let config = Kubeconfig::from_yaml("apiVersion: v1\nkind: Config\ncurrent-context: dev\ncontexts:\n- name: dev\n  context:\n    cluster: cluster\n    user: admin\nusers:\n- name: admin\n  user:\n    token: secret-token\n").unwrap();
        let result = contexts(&config);
        assert_eq!(result[0]["name"], "dev");
        assert_eq!(result[0]["current"], true);
        assert!(!result.to_string().contains("secret-token"));
        assert_eq!(config.current_context.as_deref(), Some("dev"));
    }
}
