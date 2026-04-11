//! Listener gRPC Client — Bidi streaming
//! Gateway = client, Listener = server
//! Gateway streams audio chunks → Listener streams back ASR events

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tracing::{debug, error, info, warn};

use crate::proto;
use crate::proto::listener_service_client::ListenerServiceClient;
use crate::types::InputEvent;

pub struct ListenerClient {
    endpoint: String,
    audio_tx: Option<mpsc::Sender<proto::ListenerInput>>,
}

impl ListenerClient {
    pub fn new(endpoint: String) -> Self {
        Self { endpoint, audio_tx: None }
    }

    /// Start bidi stream. Returns handle to feed audio and spawns a task
    /// to receive ASR events into the input stream.
    pub async fn start(
        &mut self,
        p1_tx: mpsc::Sender<InputEvent>,
        p2_tx: mpsc::Sender<InputEvent>,
    ) -> anyhow::Result<mpsc::Sender<proto::ListenerInput>> {
        let channel = Channel::from_shared(self.endpoint.clone())?
            .connect()
            .await?;
        let mut client = ListenerServiceClient::new(channel);

        // Outbound: audio chunks from Gateway → Listener
        let (audio_tx, audio_rx) = mpsc::channel::<proto::ListenerInput>(512);
        let outbound = ReceiverStream::new(audio_rx);

        // Start bidi stream
        let response = client.stream(outbound).await?;
        let mut inbound = response.into_inner();

        info!(addr = %self.endpoint, "Listener bidi stream established");

        // Spawn receiver task: Listener events → Input Stream
        tokio::spawn(async move {
            while let Ok(Some(output)) = inbound.message().await {
                match output.event {
                    Some(proto::listener_output::Event::VadSpeechStart(_)) => {
                        debug!("Listener → P2: vad_speech_start");
                        let _ = p2_tx.send(InputEvent::VadSpeechStart).await;
                    }
                    Some(proto::listener_output::Event::VadSpeechEnd(e)) => {
                        let _ = p2_tx.send(InputEvent::VadSpeechEnd {
                            silence_duration_ms: e.silence_duration_ms,
                        }).await;
                    }
                    Some(proto::listener_output::Event::PartialTranscript(t)) => {
                        let _ = p2_tx.send(InputEvent::PartialTranscript {
                            text: t.text,
                        }).await;
                    }
                    Some(proto::listener_output::Event::FinalTranscript(t)) => {
                        debug!(text = %t.text, "Listener → P1: final_transcript");
                        let _ = p1_tx.send(InputEvent::FinalTranscript {
                            text: t.text,
                            confidence: t.confidence,
                        }).await;
                    }
                    None => {}
                }
            }
            warn!("Listener bidi stream ended");
        });

        self.audio_tx = Some(audio_tx.clone());
        Ok(audio_tx)
    }

    /// Forward audio from endpoint to Listener
    pub async fn feed_audio(&self, data: bytes::Bytes, encoding: &str) {
        if let Some(tx) = &self.audio_tx {
            let _ = tx.send(proto::ListenerInput {
                payload: Some(proto::listener_input::Payload::Audio(
                    proto::AudioChunk {
                        data: data.to_vec(),
                        timestamp_ms: chrono::Utc::now().timestamp_millis(),
                        encoding: encoding.into(),
                    }
                )),
            }).await;
        }
    }
}