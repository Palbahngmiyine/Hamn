use crate::{
    model::{Failure, Request, envelope},
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
        let (events, mut receiver) = tokio::sync::mpsc::channel(32);
        let execution = service::execute_stream(&request, &cancel, Some(events));
        tokio::pin!(execution);
        let result = loop {
            tokio::select! {
                biased;
                Some(event) = receiver.recv() => {
                    let mut value = envelope(&request, &id, Ok(event.clone()));
                    value["type"] = event["type"].clone();
                    value["sequence"] = sequence.into();
                    sequence += 1;
                    if !write(&value) {
                        cancel.cancel();
                        let _ = execution.await; // wait for lifecycle cleanup even if the reader left
                        listener.abort(); return 1;
                    }
                }
                result = &mut execution => break result,
            }
        };
        // The last poll of execution can enqueue events and complete together.
        while let Ok(event) = receiver.try_recv() {
            let mut value = envelope(&request, &id, Ok(event.clone()));
            value["type"] = event["type"].clone();
            value["sequence"] = sequence.into();
            sequence += 1;
            if !write(&value) {
                listener.abort();
                return 1;
            }
        }
        let failed = result.is_err();
        let mut value = envelope(&request, &id, result);
        if request.watch
            || request.follow
            || request.words.last().is_some_and(|word| word == "logs")
        {
            value["type"] = if request.watch { "snapshot" } else { "result" }.into();
            value["sequence"] = sequence.into();
        }
        if !write(&value) {
            break 1;
        }
        if failed {
            break 1;
        }
        if !request.watch {
            break 0;
        }
        sequence += 1;
        tokio::select! {
            _ = cancel.cancelled() => {
                let mut value = envelope(&request, &id, Err(Failure::new("cancelled", "Watch cancelled")));
                value["type"] = "result".into();
                value["sequence"] = sequence.into();
                break if write(&value) { 130 } else { 1 };
            },
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
        }
    };
    listener.abort();
    exit
}

fn write(value: &serde_json::Value) -> bool {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, value).is_ok() && writeln!(out).is_ok() && out.flush().is_ok()
}
