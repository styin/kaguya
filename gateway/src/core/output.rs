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
        Self {
            audio_muted: AtomicBool::new(false),
            audio_tx,
            metadata_tx,
        }
    }

    pub fn mute_audio(&self) {
        self.audio_muted.store(true, Ordering::SeqCst);
    }

    pub fn unmute_audio(&self) {
        self.audio_muted.store(false, Ordering::SeqCst);
    }

    pub async fn send_audio(&self, data: bytes::Bytes) {
        if !self.audio_muted.load(Ordering::SeqCst) {
            let _ = self.audio_tx.send(data).await;
        }
    }

    pub async fn send_sentence(&self, generation_id: &str, text: &str) {
        let _ = self
            .metadata_tx
            .send(MetadataEvent {
                event_type: "sentence".into(),
                data: serde_json::json!({ "generation_id": generation_id, "text": text }),
            })
            .await;
    }

    pub async fn send_emotion(&self, generation_id: &str, emotion: &str) {
        let _ = self
            .metadata_tx
            .send(MetadataEvent {
                event_type: "emotion".into(),
                data: serde_json::json!({ "generation_id": generation_id, "emotion": emotion }),
            })
            .await;
    }

    pub async fn send_response_started(&self, generation_id: &str) {
        let _ = self
            .metadata_tx
            .send(MetadataEvent {
                event_type: "response_started".into(),
                data: serde_json::json!({ "generation_id": generation_id }),
            })
            .await;
    }

    pub async fn send_response_complete(&self, generation_id: &str, interrupted: bool) {
        let _ = self
            .metadata_tx
            .send(MetadataEvent {
                event_type: "response_complete".into(),
                data: serde_json::json!({ "generation_id": generation_id, "interrupted": interrupted }),
            })
            .await;
    }

    pub async fn send_user_input(&self, text: &str) {
        let _ = self
            .metadata_tx
            .send(MetadataEvent {
                event_type: "user_input".into(),
                data: serde_json::json!({ "text": text }),
            })
            .await;
    }
}
