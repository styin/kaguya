//! Swappable audio sender for the Listener audio path.
//!
//! The WebSocket endpoint forwards microphone audio through this sink to
//! whichever Listener TCP connection is currently active. When the Listener
//! recovery loop reconnects, it installs a new sender; when the connection
//! drops, it clears the sender so audio is silently discarded until the next
//! connection is established.

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::{mpsc, RwLock};

/// Hot-swappable audio sender shared between the WebSocket endpoint and the
/// Listener recovery loop.
///
/// Audio frames arriving while no sender is installed are silently dropped.
#[derive(Clone, Default)]
pub struct ListenerAudioSink {
    tx: Arc<RwLock<Option<mpsc::Sender<Bytes>>>>,
}

impl ListenerAudioSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the active sender (called by the recovery loop on reconnect).
    pub async fn install(&self, tx: mpsc::Sender<Bytes>) {
        *self.tx.write().await = Some(tx);
    }

    /// Remove the active sender (called on disconnect).
    pub async fn clear(&self) {
        *self.tx.write().await = None;
    }

    /// Forward an audio frame to the Listener, or drop it if no sender is
    /// installed.
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
