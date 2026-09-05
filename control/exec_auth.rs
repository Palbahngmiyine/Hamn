//! Resolve exec credentials asynchronously so operation cancellation remains effective.
use crate::model::{Failure, Result};
use base64::Engine;
use kube::Config;
use serde_json::{Value, json};
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt};

struct ProcessGroup(i32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if self.0 > 0 {
            unsafe {
                libc::kill(-self.0, libc::SIGKILL);
                // Reap this owned child before reporting cancellation. Tokio's
                // background reaper may otherwise outlive runtime shutdown.
                loop {
                    if libc::waitpid(self.0, std::ptr::null_mut(), 0) >= 0
                        || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
                    {
                        break;
                    }
                }
            }
        }
    }
}

fn failure(message: &str) -> Failure {
    Failure::new("authenticationFailed", message)
}

async fn bounded(reader: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| failure("cannot read authentication plugin output"))?;
    if bytes.len() > limit {
        return Err(failure("authentication plugin output is too large"));
    }
    Ok(bytes)
}

pub async fn resolve(config: &mut Config) -> Result<()> {
    if config.auth_info.auth_provider.is_some() {
        return Err(failure(
            "legacy auth-provider configuration is unsupported; use exec credentials",
        ));
    }
    let Some(exec) = config.auth_info.exec.take() else {
        return Ok(());
    };
    let version = exec
        .api_version
        .as_deref()
        .filter(|value| {
            matches!(
                *value,
                "client.authentication.k8s.io/v1" | "client.authentication.k8s.io/v1beta1"
            )
        })
        .ok_or_else(|| failure("unsupported exec credential API version"))?;
    let mut spec = json!({"interactive":false});
    if exec.provide_cluster_info {
        spec["cluster"] = serde_json::to_value(
            exec.cluster
                .as_ref()
                .ok_or_else(|| failure("authentication cluster information is missing"))?,
        )
        .map_err(|_| failure("cannot encode authentication cluster information"))?;
    }
    let mut command = tokio::process::Command::new(
        exec.command
            .as_deref()
            .ok_or_else(|| failure("authentication plugin command is missing"))?,
    );
    command.args(exec.args.as_deref().unwrap_or_default());
    for variable in exec.env.as_deref().unwrap_or_default() {
        if let (Some(name), Some(value)) = (variable.get("name"), variable.get("value")) {
            command.env(name, value);
        }
    }
    for name in exec.drop_env.as_deref().unwrap_or_default() {
        command.env_remove(name);
    }
    command.env(
        "KUBERNETES_EXEC_INFO",
        json!({"apiVersion":version,"kind":"ExecCredential","spec":spec}).to_string(),
    );
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false); // ProcessGroup owns cancellation and reaping.
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|_| failure("cannot start authentication plugin"))?;
    let mut group = ProcessGroup(child.id().unwrap() as i32);
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (stdout, _, status) =
        tokio::try_join!(bounded(stdout, 1024 * 1024), bounded(stderr, 8192), async {
            child
                .wait()
                .await
                .map_err(|_| failure("cannot wait for authentication plugin"))
        })?;
    if !status.success() {
        return Err(failure(
            "authentication plugin failed; authenticate outside Hamn",
        ));
    }
    let value: Value =
        serde_json::from_slice(&stdout).map_err(|_| failure("invalid exec credential response"))?;
    if value["apiVersion"] != version || value["kind"] != "ExecCredential" {
        return Err(failure(
            "exec credential response identity does not match the request",
        ));
    }
    let status = value["status"]
        .as_object()
        .ok_or_else(|| failure("exec credential status is missing"))?;
    if let Some(expiry) = status.get("expirationTimestamp") {
        let expiry = expiry
            .as_str()
            .and_then(|text| text.parse::<jiff::Timestamp>().ok())
            .ok_or_else(|| failure("invalid exec credential expiration"))?;
        if expiry <= jiff::Timestamp::now() {
            return Err(failure(
                "authentication plugin returned expired credentials",
            ));
        }
    }
    let token = status
        .get("token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let certificate = status
        .get("clientCertificateData")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let key = status
        .get("clientKeyData")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    if certificate.is_some() != key.is_some() || (token.is_none() && certificate.is_none()) {
        return Err(failure(
            "exec credentials require a token or a complete certificate and key",
        ));
    }
    config.auth_info.token = token.map(|value| value.to_owned().into());
    config.auth_info.token_file = None;
    if let (Some(certificate), Some(key)) = (certificate, key) {
        config.auth_info.client_certificate = None;
        config.auth_info.client_key = None;
        config.auth_info.client_certificate_data =
            Some(base64::engine::general_purpose::STANDARD.encode(certificate));
        config.auth_info.client_key_data =
            Some(base64::engine::general_purpose::STANDARD.encode(key).into());
    }
    group.0 = 0;
    // Each operation (including each watch snapshot) resolves fresh credentials.
    // Never hand a blocking exec subprocess back to the async HTTP client.
    Ok(())
}
