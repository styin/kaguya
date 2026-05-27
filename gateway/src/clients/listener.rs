//! Listener gRPC Client + raw audio socket forwarder.
//! Gateway = client, Listener = server.
//! Audio bypasses gRPC — raw TCP socket with length-prefixed frames.

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tracing::{debug, info, warn};

use crate::lifecycle::{ManagedConnectionHandle, Readiness, ReconnectPolicy, TaskSpawner};
use crate::proto;
use crate::proto::listener_service_client::ListenerServiceClient;
use crate::types::InputEvent;

pub struct ListenerClient {
    grpc_endpoint: String,
    audio_addr: String,
    tasks: TaskSpawner,
    connection: ManagedConnectionHandle,
    reconnect: ReconnectPolicy,
}

impl ListenerClient {
    pub fn new(
        grpc_endpoint: String,
        audio_addr: String,
        tasks: TaskSpawner,
        connection: ManagedConnectionHandle,
    ) -> Self {
        Self::with_reconnect_policy(
            grpc_endpoint,
            audio_addr,
            tasks,
            connection,
            ReconnectPolicy::default(),
        )
    }

    pub fn with_reconnect_policy(
        grpc_endpoint: String,
        audio_addr: String,
        tasks: TaskSpawner,
        connection: ManagedConnectionHandle,
        reconnect: ReconnectPolicy,
    ) -> Self {
        Self {
            grpc_endpoint,
            audio_addr,
            tasks,
            connection,
            reconnect,
        }
    }

    /// Start bidi gRPC stream for ASR events + raw TCP forwarder for audio.
    /// Returns the audio sender — caller (main.rs) passes it to EndpointState.
    pub async fn start(
        &self,
        p1_tx: mpsc::Sender<InputEvent>,
        p2_tx: mpsc::Sender<InputEvent>,
    ) -> anyhow::Result<mpsc::Sender<bytes::Bytes>> {
        // ── gRPC bidi stream for ASR events ──
        let mut inbound = Self::connect_stream_with_policy(
            &self.grpc_endpoint,
            self.connection.clone(),
            self.reconnect,
        )
        .await?;

        info!(addr = %self.grpc_endpoint, "Listener gRPC bidi stream established");

        // Spawn receiver: Listener ASR events → Input Stream
        let asr_connection = self.connection.clone();
        self.tasks.spawn("listener_asr_stream", async move {
            while let Ok(Some(output)) = inbound.message().await {
                match output.event {
                    Some(proto::listener_output::Event::VadSpeechStart(_)) => {
                        debug!("Listener → P2: vad_speech_start");
                        let _ = p2_tx.send(InputEvent::VadSpeechStart).await;
                    }
                    Some(proto::listener_output::Event::VadSpeechEnd(e)) => {
                        let _ = p2_tx
                            .send(InputEvent::VadSpeechEnd {
                                silence_duration_ms: e.silence_duration_ms,
                            })
                            .await;
                    }
                    Some(proto::listener_output::Event::PartialTranscript(t)) => {
                        let _ = p2_tx
                            .send(InputEvent::PartialTranscript { text: t.text })
                            .await;
                    }
                    Some(proto::listener_output::Event::FinalTranscript(t)) => {
                        debug!(text = %t.text, "Listener → P1: final_transcript");
                        let _ = p1_tx
                            .send(InputEvent::FinalTranscript {
                                text: t.text,
                                confidence: t.confidence,
                            })
                            .await;
                    }
                    None => {}
                }
            }
            asr_connection.set_readiness(Readiness::Degraded);
            warn!("Listener bidi stream ended");
        });

        // ── Raw TCP socket forwarder for audio ──
        let (audio_tx, mut audio_rx) = mpsc::channel::<bytes::Bytes>(512);
        let audio_addr = self.audio_addr.clone();
        let audio_connection = self.connection.clone();
        let audio_reconnect = self.reconnect;

        self.tasks.spawn("listener_audio_forwarder", async move {
            loop {
                match Self::connect_audio_with_policy(
                    &audio_addr,
                    audio_connection.clone(),
                    audio_reconnect,
                )
                .await
                {
                    Ok(mut stream) => {
                        audio_connection.set_readiness(Readiness::Ready);
                        info!(addr = %audio_addr, "Audio socket connected to Listener");
                        while let Some(data) = audio_rx.recv().await {
                            let len = (data.len() as u32).to_be_bytes();
                            if stream.write_all(&len).await.is_err()
                                || stream.write_all(&data).await.is_err()
                            {
                                audio_connection.set_readiness(Readiness::Degraded);
                                warn!("Audio socket write failed, reconnecting");
                                break;
                            }
                        }
                        // recv() returned None → sender dropped → shutdown
                        if audio_rx.is_closed() {
                            audio_connection.set_readiness(Readiness::Stopped);
                            debug!("Audio forwarder: sender dropped, exiting");
                            return;
                        }
                    }
                    Err(()) => {
                        if audio_rx.is_closed() {
                            audio_connection.set_readiness(Readiness::Stopped);
                            debug!("Audio forwarder: sender dropped during reconnect, exiting");
                            return;
                        }
                    }
                }
            }
        });

        Ok(audio_tx)
    }

    async fn connect_stream_with_policy(
        endpoint: &str,
        connection: ManagedConnectionHandle,
        reconnect: ReconnectPolicy,
    ) -> anyhow::Result<tonic::Streaming<proto::ListenerOutput>> {
        let retry_delays = reconnect.retry_delays();
        connection.set_readiness(Readiness::Starting);
        for attempt in 1..=reconnect.max_attempts() {
            match tokio::time::timeout(
                reconnect.attempt_timeout(),
                Self::connect_stream_once(endpoint),
            )
            .await
            {
                Ok(Ok(stream)) => {
                    connection.set_readiness(Readiness::Ready);
                    return Ok(stream);
                }
                Ok(Err(e)) => {
                    warn!(
                        attempt,
                        max_attempts = reconnect.max_attempts(),
                        "Listener gRPC connect attempt failed: {e}"
                    );
                }
                Err(_) => {
                    warn!(
                        attempt,
                        max_attempts = reconnect.max_attempts(),
                        timeout_ms = reconnect.attempt_timeout().as_millis(),
                        "Listener gRPC connect attempt timed out"
                    );
                }
            }

            if let Some(delay) = retry_delays.get(attempt - 1) {
                tokio::time::sleep(*delay).await;
            }
        }

        connection.set_readiness(Readiness::Degraded);
        anyhow::bail!("Listener gRPC connect failed after reconnect policy was exhausted")
    }

    async fn connect_stream_once(
        endpoint: &str,
    ) -> anyhow::Result<tonic::Streaming<proto::ListenerOutput>> {
        let channel = Channel::from_shared(endpoint.to_string())?
            .connect()
            .await?;
        let mut client = ListenerServiceClient::new(channel);
        let (_ctrl_tx, ctrl_rx) = mpsc::channel::<proto::ListenerInput>(16);
        let outbound = ReceiverStream::new(ctrl_rx);
        Ok(client.stream(outbound).await?.into_inner())
    }

    async fn connect_audio_with_policy(
        audio_addr: &str,
        connection: ManagedConnectionHandle,
        reconnect: ReconnectPolicy,
    ) -> Result<TcpStream, ()> {
        let retry_delays = reconnect.retry_delays();
        connection.set_readiness(Readiness::Starting);
        for attempt in 1..=reconnect.max_attempts() {
            match tokio::time::timeout(reconnect.attempt_timeout(), TcpStream::connect(audio_addr))
                .await
            {
                Ok(Ok(stream)) => {
                    connection.set_readiness(Readiness::Ready);
                    return Ok(stream);
                }
                Ok(Err(e)) => {
                    warn!(
                        attempt,
                        max_attempts = reconnect.max_attempts(),
                        "Audio socket connect failed: {e}"
                    );
                }
                Err(_) => {
                    warn!(
                        attempt,
                        max_attempts = reconnect.max_attempts(),
                        timeout_ms = reconnect.attempt_timeout().as_millis(),
                        "Audio socket connect attempt timed out"
                    );
                }
            }

            if let Some(delay) = retry_delays.get(attempt - 1) {
                tokio::time::sleep(*delay).await;
            }
        }

        warn!("Audio socket reconnect policy exhausted; retrying policy window");
        connection.set_readiness(Readiness::Degraded);
        Err(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::lifecycle::LifecycleSupervisor;

    #[tokio::test]
    async fn start_returns_error_and_degrades_readiness_after_policy_exhaustion() {
        let lifecycle = LifecycleSupervisor::new();
        let connection = lifecycle.register_connection("listener");
        let listener = ListenerClient::with_reconnect_policy(
            "not a valid uri".into(),
            "127.0.0.1:0".into(),
            lifecycle.spawner(),
            connection.clone(),
            ReconnectPolicy::bounded(
                1,
                Duration::from_millis(1),
                Duration::from_millis(1),
                Duration::from_millis(1),
            ),
        );
        let (p1_tx, _p1_rx) = mpsc::channel(1);
        let (p2_tx, _p2_rx) = mpsc::channel(1);

        let result = listener.start(p1_tx, p2_tx).await;

        assert!(result.is_err());
        assert_eq!(connection.readiness(), Readiness::Degraded);
    }
}
