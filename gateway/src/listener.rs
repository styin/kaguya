//! Listener 事件桥接。
//!
//! 接收 Listener 的 VAD/STT 事件，路由到正确的 Input Stream 优先级：
//!   VAD signals → P2
//!   partial_transcript → P2
//!   final_transcript → P1

use tokio::sync::mpsc;
use tracing::debug;
use crate::types::InputEvent;

pub struct ListenerBridge {
    p1_tx: mpsc::Sender<InputEvent>,
    p2_tx: mpsc::Sender<InputEvent>,
}

impl ListenerBridge {
    pub fn new(p1_tx: mpsc::Sender<InputEvent>, p2_tx: mpsc::Sender<InputEvent>) -> Self {
        Self { p1_tx, p2_tx }
    }

    pub async fn on_vad_start(&self) {
        debug!("Listener → P2: vad_speech_start");
        let _ = self.p2_tx.send(InputEvent::VadSpeechStart).await;
    }

    pub async fn on_vad_end(&self) {
        let _ = self.p2_tx.send(InputEvent::VadSpeechEnd).await;
    }

    pub async fn on_partial(&self, text: String) {
        let _ = self
            .p2_tx
            .send(InputEvent::PartialTranscript { text })
            .await;
    }

    pub async fn on_final(&self, text: String, confidence: f32) {
        debug!(text = %text, "Listener → P1: final_transcript");
        let _ = self
            .p1_tx
            .send(InputEvent::FinalTranscript { text, confidence })
            .await;
    }
}