//! Output Management
//!
//! Controls audio and metadata forwarding of the Talker to the Endpoint.
//! Sends audio data and metadata events (like sentences and emotions) via WebSocket for rendering.

use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use crate::types::MetadataEvent;

pub struct OutputManager {
    audio_muted: AtomicBool,
    audio_tx: mpsc::Sender<bytes::Bytes>,
    metadata_tx: mpsc::Sender<MetadataEvent>,
}

impl OutputManager {
    pub fn new(
        audio_tx: mpsc::Sender<bytes::Bytes>,
        metadata_tx: mpsc::Sender<MetadataEvent>,
    ) -> Self {
        Self { audio_muted: AtomicBool::new(false), audio_tx, metadata_tx }
    }

    pub fn mute_audio(&self)   { self.audio_muted.store(true, Ordering::SeqCst); }
    pub fn unmute_audio(&self) { self.audio_muted.store(false, Ordering::SeqCst); }

    pub async fn send_audio(&self, data: bytes::Bytes) {
        if !self.audio_muted.load(Ordering::SeqCst) {
            let _ = self.audio_tx.send(data).await;
        }
    }

    pub async fn send_sentence(&self, text: &str) {
        let _ = self.metadata_tx.send(MetadataEvent {
            event_type: "sentence".into(),
            data: serde_json::json!({ "text": text }),
        }).await;
    }

    pub async fn send_emotion(&self, emotion: &str) {
        let _ = self.metadata_tx.send(MetadataEvent {
            event_type: "emotion".into(),
            data: serde_json::json!({ "emotion": emotion }),
        }).await;
    }
}