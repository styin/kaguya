use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::{mpsc, RwLock};

#[derive(Clone, Default)]
pub struct ListenerAudioSink {
    tx: Arc<RwLock<Option<mpsc::Sender<Bytes>>>>,
}

impl ListenerAudioSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn install(&self, tx: mpsc::Sender<Bytes>) {
        *self.tx.write().await = Some(tx);
    }

    pub async fn clear(&self) {
        *self.tx.write().await = None;
    }

    pub async fn send(&self, data: Bytes) {
        let tx = self.tx.read().await.clone();
        if let Some(tx) = tx {
            let _ = tx.send(data).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sink_drops_until_sender_is_installed() {
        let sink = ListenerAudioSink::new();
        sink.send(Bytes::from_static(b"before")).await;

        let (tx, mut rx) = mpsc::channel(1);
        sink.install(tx).await;
        sink.send(Bytes::from_static(b"after")).await;

        assert_eq!(rx.recv().await, Some(Bytes::from_static(b"after")));
    }
}
