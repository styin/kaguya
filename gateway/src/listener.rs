//! Listener Service
//! TODO: Refactor - Flip server-client roles: Gateway connects to Listener as a gRPC client

use tokio::sync::mpsc;
use tonic::{Request, Response, Status, Streaming};
use tracing::debug;

use crate::proto;
use crate::proto::listener_service_server::ListenerService;
use crate::types::InputEvent;

pub struct ListenerServiceImpl {
    p1_tx: mpsc::Sender<InputEvent>,
    p2_tx: mpsc::Sender<InputEvent>,
}

impl ListenerServiceImpl {
    pub fn new(p1_tx: mpsc::Sender<InputEvent>, p2_tx: mpsc::Sender<InputEvent>) -> Self {
        Self { p1_tx, p2_tx }
    }
}

#[tonic::async_trait]
impl ListenerService for ListenerServiceImpl {
    async fn stream_events(
        &self,
        request: Request<Streaming<proto::ListenerEvent>>,
    ) -> Result<Response<proto::ListenerAck>, Status> {
        let mut stream = request.into_inner();
        debug!("Listener connected");

        while let Some(event) = stream
            .message()
            .await
            .map_err(|e| Status::internal(format!("stream error: {e}")))?
        {
            match event.event {
                Some(proto::listener_event::Event::VadSpeechStart(_)) => {
                    debug!("Listener → P2: vad_speech_start");
                    let _ = self.p2_tx.send(InputEvent::VadSpeechStart).await;
                }
                Some(proto::listener_event::Event::VadSpeechEnd(e)) => {
                    let _ = self.p2_tx.send(InputEvent::VadSpeechEnd {
                        silence_duration_ms: e.silence_duration_ms,
                    }).await;
                }
                Some(proto::listener_event::Event::PartialTranscript(t)) => {
                    let _ = self.p2_tx.send(InputEvent::PartialTranscript {
                        text: t.text,
                    }).await;
                }
                Some(proto::listener_event::Event::FinalTranscript(t)) => {
                    debug!(text = %t.text, "Listener → P1: final_transcript");
                    let _ = self.p1_tx.send(InputEvent::FinalTranscript {
                        text: t.text,
                        confidence: t.confidence,
                    }).await;
                }
                None => {}
            }
        }

        Ok(Response::new(proto::ListenerAck {}))
    }
}