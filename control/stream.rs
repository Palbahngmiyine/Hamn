use crate::model::{Failure, Result};
use serde_json::{Value, json};
pub type Events = tokio::sync::mpsc::Sender<Value>;

#[derive(Default)]
pub struct TextStream {
    pending: Vec<u8>,
}

impl TextStream {
    pub async fn feed(&mut self, bytes: &[u8], events: &Events) -> Result<()> {
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            if part.len() > 1024 * 1024 - self.pending.len() {
                return Err(Failure::new("responseTooLarge", "a log line exceeds 1 MiB"));
            }
            self.pending.extend_from_slice(part);
            if part.last() == Some(&b'\n') {
                let line = std::mem::take(&mut self.pending);
                emit(
                    events,
                    json!({"type":"log","text":String::from_utf8_lossy(&line)}),
                )
                .await?;
            }
        }
        Ok(())
    }
    pub async fn finish(self, events: &Events) -> Result<()> {
        if !self.pending.is_empty() {
            emit(
                events,
                json!({"type":"log","text":String::from_utf8_lossy(&self.pending)}),
            )
            .await?;
        }
        Ok(())
    }
}

pub async fn emit(events: &Events, value: Value) -> Result<()> {
    events
        .send(value)
        .await
        .map_err(|_| Failure::new("cancelled", "stream consumer disconnected"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn large_batches_of_short_lines_use_bounded_backpressure() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<Value>(2);
        let consumer = tokio::spawn(async move {
            let mut count = 0;
            while let Some(event) = receiver.recv().await {
                assert_eq!(event["text"], "line\n");
                count += 1;
            }
            count
        });
        let mut text = TextStream::default();
        text.feed(&b"line\n".repeat(220000), &sender).await.unwrap();
        assert!(text.pending.is_empty());
        text.finish(&sender).await.unwrap();
        drop(sender);
        assert_eq!(consumer.await.unwrap(), 220000);
    }

    #[tokio::test]
    async fn rejects_oversized_lines_before_copying_and_reports_closed_receiver() {
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let mut text = TextStream::default();
        text.feed(&vec![b'x'; 1024 * 1024], &sender).await.unwrap();
        assert_eq!(
            text.feed(b"x", &sender).await.unwrap_err().code,
            "responseTooLarge"
        );
        assert_eq!(text.pending.len(), 1024 * 1024);
        drop(receiver);
        assert_eq!(text.finish(&sender).await.unwrap_err().code, "cancelled");
    }

    #[tokio::test]
    async fn preserves_unicode_split_across_network_reads() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        let mut text = TextStream::default();
        let bytes = "한글\n".as_bytes();
        text.feed(&bytes[..2], &sender).await.unwrap();
        assert!(receiver.try_recv().is_err());
        text.feed(&bytes[2..], &sender).await.unwrap();
        assert_eq!(receiver.recv().await.unwrap()["text"], "한글\n");
        assert!(
            text.feed(&vec![b'x'; 1024 * 1024 + 1], &sender)
                .await
                .is_err()
        );
    }
}
