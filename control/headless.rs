use crate::{
    model::{Request, envelope},
    service,
};
use std::io::{self, Write};
use tokio_util::sync::CancellationToken;

pub async fn run(request: Request) -> i32 {
    let cancel = CancellationToken::new();
    let signal = cancel.clone();
    let listener = tokio::spawn(async move {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("signal handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        signal.cancel();
    });
    let id = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let mut sequence = 0u64;
    let exit = loop {
        let result = service::execute(&request, &cancel).await;
        let failed = result.is_err();
        let mut value = envelope(&request, &id, result);
        if request.watch {
            value["type"] = "snapshot".into();
            value["sequence"] = sequence.into();
        }
        let mut out = io::stdout().lock();
        if serde_json::to_writer(&mut out, &value).is_err()
            || writeln!(out).is_err()
            || out.flush().is_err()
        {
            break 1;
        }
        drop(out);
        if failed {
            break 1;
        }
        if !request.watch {
            break 0;
        }
        sequence += 1;
        tokio::select! {
            _ = cancel.cancelled() => break 130,
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
        }
    };
    listener.abort();
    exit
}
