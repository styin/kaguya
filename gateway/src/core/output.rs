//! Output Management
//!
//! Projects Talker semantic events into endpoint metadata.
//! Talker owns local audio playback; Gateway does not receive TTS bytes.

use tokio::sync::broadcast;

use crate::core::types::MetadataEvent;

pub struct OutputManager {
    metadata_tx: broadcast::Sender<MetadataEvent>,
}

impl OutputManager {
    pub fn new(metadata_tx: broadcast::Sender<MetadataEvent>) -> Self {
        Self { metadata_tx }
    }

    pub fn send_sentence(&self, generation_id: &str, text: &str) {
        let _ = self.metadata_tx.send(MetadataEvent {
            event_type: "sentence".into(),
            data: serde_json::json!({ "generation_id": generation_id, "text": text }),
        });
    }

    pub fn send_emotion(&self, generation_id: &str, emotion: &str) {
        let _ = self.metadata_tx.send(MetadataEvent {
            event_type: "emotion".into(),
            data: serde_json::json!({ "generation_id": generation_id, "emotion": emotion }),
        });
    }

    pub fn send_response_started(&self, generation_id: &str) {
        let _ = self.metadata_tx.send(MetadataEvent {
            event_type: "response_started".into(),
            data: serde_json::json!({ "generation_id": generation_id }),
        });
    }

    pub fn send_response_complete(&self, generation_id: &str, interrupted: bool) {
        let _ = self.metadata_tx.send(MetadataEvent {
            event_type: "response_complete".into(),
            data: serde_json::json!({ "generation_id": generation_id, "interrupted": interrupted }),
        });
    }

    pub fn send_user_input(&self, text: &str) {
        let _ = self.metadata_tx.send(MetadataEvent {
            event_type: "user_input".into(),
            data: serde_json::json!({ "text": text }),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_without_or_with_a_slow_console_never_backpressures_output() {
        let (tx, _) = broadcast::channel(2);
        let output = OutputManager::new(tx.clone());
        for _ in 0..1024 {
            output.send_sentence("generation-1", "no console connected");
        }
        let mut rx = tx.subscribe();
        for _ in 0..4 {
            output.send_sentence("generation-2", "slow console");
        }
        assert!(matches!(
            rx.try_recv(),
            Err(broadcast::error::TryRecvError::Lagged(2))
        ));
        let event = rx.try_recv().unwrap();
        assert_eq!(event.data["generation_id"], "generation-2");
    }
}
