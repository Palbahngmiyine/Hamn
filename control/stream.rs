use crate::model::{Failure, Result};
use serde_json::{Value, json};
pub type Events = tokio::sync::mpsc::Sender<Value>;

#[derive(Default)]
pub struct TextStream {
    pending: Vec<u8>,
}

impl TextStream {
    pub async fn feed(&mut self, bytes: &[u8], events: &Events) -> Result<()> {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > 1024 * 1024 {
            return Err(Failure::new("responseTooLarge", "a log line exceeds 1 MiB"));
        }
        while let Some(index) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = self.pending.drain(..=index).collect();
            emit(
                events,
                json!({"type":"log","text":String::from_utf8_lossy(&line)}),
            )
            .await?;
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
