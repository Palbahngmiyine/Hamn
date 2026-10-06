//! Explicit Docker contexts use the Docker CLI's authenticated raw transport.
//! A private per-request Unix socket keeps the existing Engine API/JSON contract
//! intact for TLS and SSH contexts. Every bridge is polled in this future (no
//! detached tasks); completion, timeout or cancellation drops/reaps its owned
//! CLI process groups and removes only the socket directory we created.
//!
//! A CLI that exits unsuccessfully fails the request with its exit status and
//! stderr, for example an unknown context. The code is `outcomeUnknown` only
//! for a mutation whose transport had already relayed Engine response bytes;
//! before that, version negotiation cannot have finished and nothing was sent.
use crate::model::{Failure, Request, Result};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{
    io::{self, Read},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};

struct PrivateSocket {
    directory: PathBuf,
    path: PathBuf,
}
impl PrivateSocket {
    fn create() -> io::Result<(Self, UnixListener)> {
        let mut random = [0u8; 16];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
        let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
        // Keep the address below Darwin's 104-byte sockaddr_un bound.
        let directory = PathBuf::from(format!("/tmp/hamn-docker-{token}"));
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let owner = Self {
            path: directory.join("engine.sock"),
            directory,
        };
        let listener = UnixListener::bind(&owner.path)?;
        std::fs::set_permissions(&owner.path, std::fs::Permissions::from_mode(0o600))?;
        Ok((owner, listener))
    }
}
impl Drop for PrivateSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

/// Relays one Engine client connection through one CLI transport process and
/// sets `responded` once any byte from the CLI reaches the client.
async fn bridge(
    mut stream: UnixStream,
    context: &str,
    config: Option<&str>,
    responded: &AtomicBool,
) -> io::Result<()> {
    let mut command = tokio::process::Command::new("docker");
    if let Some(config) = config {
        command.args(["--config", config]);
    }
    command
        .args(["--context", context, "system", "dial-stdio"])
        .env_remove("DOCKER_HOST")
        .env_remove("DOCKER_CONTEXT");
    let (mut process, mut input, mut output, error) =
        crate::query_process::QueryProcess::spawn_piped(command)?;
    let (mut read, mut write) = stream.split();
    {
        let to_daemon = async {
            tokio::io::copy(&mut read, &mut input).await?;
            input.shutdown().await
        };
        let from_daemon = async {
            let mut buffer = vec![0u8; 8192];
            loop {
                let count = output.read(&mut buffer).await?;
                if count == 0 {
                    return Ok::<_, io::Error>(());
                }
                responded.store(true, Ordering::Relaxed);
                write.write_all(&buffer[..count]).await?;
            }
        };
        let drain_error = async {
            let mut bytes = Vec::new();
            error.take(65537).read_to_end(&mut bytes).await?;
            if bytes.len() > 65536 {
                return Err(io::Error::other("Docker transport stderr exceeds 64 KiB"));
            }
            Ok::<_, io::Error>(bytes)
        };
        let completion = async {
            let (status, stderr) = tokio::try_join!(process.wait(), drain_error)?;
            if !status.success() {
                return Err(io::Error::other(format!(
                    "Docker context transport exited {status}: {}",
                    String::from_utf8_lossy(&stderr).trim_end()
                )));
            }
            Ok(())
        };
        tokio::pin!(completion, to_daemon, from_daemon);
        let mut input_done = false;
        loop {
            tokio::select! {
                // A normally exited CLI may still have a finite stdout tail in its
                // pipe. Reaping the leader must not discard those response bytes.
                result = &mut completion => { result?; from_daemon.await?; break; },
                // End of output does not say why the CLI stopped. Its exit status
                // must be known before the client sees the stream close, or a
                // failed transport surfaces as the client's bare connection error.
                // `dial-stdio` closes stdout only as it exits; a CLI that lingered
                // would be bounded by the request deadline, which drops this future.
                result = &mut from_daemon => { result?; completion.await?; break; },
                result = &mut to_daemon, if !input_done => { result?; input_done = true; },
            }
        }
    }
    match write.shutdown().await {
        // The Engine client may already have consumed a Connection: close
        // response and closed its end. That is a completed half-close.
        Err(error) if error.kind() == io::ErrorKind::NotConnected => Ok(()),
        result => result,
    }
}

pub async fn execute(
    request: &Request,
    events: Option<&crate::stream::Events>,
) -> Result<serde_json::Value> {
    let context = request
        .context
        .as_deref()
        .ok_or_else(|| Failure::new("invalidRequest", "explicit Docker context required"))?;
    let (socket, listener) =
        PrivateSocket::create().map_err(|e| Failure::new("dockerUnavailable", e))?;
    let socket_path = socket.path.to_str().expect("ASCII private socket path");
    let responded = AtomicBool::new(false);
    let server = async {
        let mut connections = FuturesUnordered::new();
        loop {
            tokio::select! {
                accepted = listener.accept(), if connections.len() < 16 => {
                    let (stream, _) = accepted?;
                    connections.push(bridge(stream, context, request.docker_config.as_deref(), &responded));
                }
                Some(result) = connections.next(), if !connections.is_empty() => result?,
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), io::Error>(())
    };
    tokio::select! {
        result = crate::docker::execute(request, socket_path, events) => result,
        result = server => {
            // A mutation is written only after version negotiation and name
            // resolution, which both need an Engine response.
            let possibly_sent = request.mutates() && responded.load(Ordering::Relaxed);
            Err(Failure::new(if possibly_sent { "outcomeUnknown" } else { "dockerUnavailable" },
                result.err().map_or_else(|| "Docker transport ended".into(), |e| e.to_string())))
        }
    }
}
