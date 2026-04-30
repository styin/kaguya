//! Listener gRPC Client + raw audio socket forwarder.
//! Gateway = client, Listener = server.
//! Audio bypasses gRPC — raw TCP socket with length-prefixed frames.

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tracing::{debug, error, info, warn};

use crate::proto;
use crate::proto::listener_service_client::ListenerServiceClient;
use crate::types::InputEvent;

pub struct ListenerClient {
    grpc_endpoint: String,
    audio_addr: String,
}

impl ListenerClient {
    pub fn new(grpc_endpoint: String, audio_addr: String) -> Self {
        Self { grpc_endpoint, audio_addr }
    }

    /// Start bidi gRPC stream for ASR events + raw TCP forwarder for audio.
    /// Returns the audio sender — caller (main.rs) passes it to EndpointState.
    pub async fn start(
        &self,
        p1_tx: mpsc::Sender<InputEvent>,
        p2_tx: mpsc::Sender<InputEvent>,
    ) -> anyhow::Result<mpsc::Sender<bytes::Bytes>> {
        // ── gRPC bidi stream for ASR events ──
        let channel = Channel::from_shared(self.grpc_endpoint.clone())?
            .connect()
            .await?;
        let mut client = ListenerServiceClient::new(channel);

        let (_ctrl_tx, ctrl_rx) = mpsc::channel::<proto::ListenerInput>(16);
        let outbound = ReceiverStream::new(ctrl_rx);

        let response = client.stream(outbound).await?;
        let mut inbound = response.into_inner();

        info!(addr = %self.grpc_endpoint, "Listener gRPC bidi stream established");

        // Spawn receiver: Listener ASR events → Input Stream
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

        // ── Raw TCP socket forwarder for audio ──
        let (audio_tx, mut audio_rx) = mpsc::channel::<bytes::Bytes>(512);
        let audio_addr = self.audio_addr.clone();

        tokio::spawn(async move {
            loop {
                match TcpStream::connect(&audio_addr).await {
                    Ok(mut stream) => {
                        info!(addr = %audio_addr, "Audio socket connected to Listener");
                        while let Some(data) = audio_rx.recv().await {
                            let len = (data.len() as u32).to_be_bytes();
                            if stream.write_all(&len).await.is_err()
                                || stream.write_all(&data).await.is_err()
                            {
                                warn!("Audio socket write failed, reconnecting");
                                break;
                            }
                        }
                        // recv() returned None → sender dropped → shutdown
                        if audio_rx.is_closed() {
                            debug!("Audio forwarder: sender dropped, exiting");
                            return;
                        }
                    }
                    Err(e) => {
                        // Check if sender is still alive before retrying
                        if audio_rx.is_closed() {
                            debug!("Audio forwarder: sender dropped during reconnect, exiting");
                            return;
                        }
                        warn!("Audio socket connect failed: {e}, retrying in 2s");
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                }
            }
        });

        Ok(audio_tx)
    }
}