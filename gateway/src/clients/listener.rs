//! Listener gRPC Client + raw audio socket forwarder.
//! Gateway = client, Listener = server.
//! Audio bypasses gRPC — raw TCP socket with length-prefixed frames.

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
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

pub async fn run_recovery_loop(
    grpc_endpoint: String,
    audio_addr: String,
    task_spawner: TaskSpawner,
    connection: ManagedConnectionHandle,
    reconnect: ReconnectPolicy,
    listener_audio: crate::audio_sink::ListenerAudioSink,
    p1_tx: mpsc::Sender<InputEvent>,
    p2_tx: mpsc::Sender<InputEvent>,
    expected_runtime: bool,
    shutdown: CancellationToken,
) {
    use crate::probe::{sleep_probe_interval, wait_for_grpc_endpoint};

    if !expected_runtime && p1_tx.is_closed() {
        connection.set_readiness(Readiness::Stopped);
        listener_audio.clear().await;
        return;
    }

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => return,
            _ = wait_for_grpc_endpoint(
                "listener",
                &grpc_endpoint,
                expected_runtime,
                |readiness| connection.set_readiness(readiness),
            ) => {}
        }

        let listener = ListenerClient::with_reconnect_policy(
            grpc_endpoint.clone(),
            audio_addr.clone(),
            task_spawner.clone(),
            connection.clone(),
            reconnect,
        );
        match listener.start(p1_tx.clone(), p2_tx.clone()).await {
            Ok(audio_tx) => {
                listener_audio.install(audio_tx).await;
                info!("Listener connected (gRPC + audio socket)");

                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => return,
                        _ = sleep_probe_interval() => {}
                    }
                    if connection.readiness() == Readiness::Degraded {
                        info!("Listener connection lost, reconnecting");
                        listener_audio.clear().await;
                        break;
                    }
                }
            }
            Err(e) => {
                warn!("Listener startup failed after endpoint readiness: {e}");
                if expected_runtime {
                    connection.set_readiness(Readiness::Starting);
                }
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => return,
                    _ = sleep_probe_interval() => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::lifecycle::LifecycleSupervisor;

    fn fast_policy() -> ReconnectPolicy {
        ReconnectPolicy::bounded(
            1,
            Duration::from_millis(1),
            Duration::from_millis(1),
            Duration::from_millis(100),
        )
    }

    #[tokio::test]
    async fn start_returns_error_and_degrades_readiness_after_policy_exhaustion() {
        let lifecycle = LifecycleSupervisor::new();
        let connection = lifecycle.register_connection("listener");
        let listener = ListenerClient::with_reconnect_policy(
            "not a valid uri".into(),
            "127.0.0.1:0".into(),
            lifecycle.spawner(),
            connection.clone(),
            fast_policy(),
        );
        let (p1_tx, _p1_rx) = mpsc::channel(1);
        let (p2_tx, _p2_rx) = mpsc::channel(1);

        let result = listener.start(p1_tx, p2_tx).await;

        assert!(result.is_err());
        assert_eq!(connection.readiness(), Readiness::Degraded);
    }

    // BUG-SCOPING: Without a recovery loop, readiness stays Degraded
    // permanently after start() fails. The bare ListenerClient has no
    // built-in reconnect — run_recovery_loop() wraps it with one.
    #[tokio::test]
    async fn degraded_readiness_has_no_automatic_recovery() {
        let lifecycle = LifecycleSupervisor::new();
        let connection = lifecycle.register_connection("listener");
        let listener = ListenerClient::with_reconnect_policy(
            "http://127.0.0.1:1".into(),
            "127.0.0.1:1".into(),
            lifecycle.spawner(),
            connection.clone(),
            fast_policy(),
        );
        let (p1_tx, _p1_rx) = mpsc::channel(1);
        let (p2_tx, _p2_rx) = mpsc::channel(1);

        let _ = listener.start(p1_tx, p2_tx).await;
        assert_eq!(connection.readiness(), Readiness::Degraded);

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            connection.readiness(),
            Readiness::Degraded,
            "readiness should stay Degraded: no recovery loop exists"
        );
    }

    // ── Recovery loop integration tests ──

    use crate::audio_sink::ListenerAudioSink;
    use crate::proto::listener_service_server::{
        ListenerService as ListenerServiceTrait, ListenerServiceServer,
    };
    use tokio_stream::wrappers::TcpListenerStream;

    struct StubListener;

    #[tonic::async_trait]
    impl ListenerServiceTrait for StubListener {
        type StreamStream = ReceiverStream<Result<proto::ListenerOutput, tonic::Status>>;

        async fn stream(
            &self,
            _req: tonic::Request<tonic::Streaming<proto::ListenerInput>>,
        ) -> Result<tonic::Response<Self::StreamStream>, tonic::Status> {
            let (tx, rx) = mpsc::channel(1);
            // Hold stream sender alive so the ASR receiver task doesn't
            // exit immediately. Drops when the server shuts down.
            tokio::spawn(async move {
                std::future::pending::<()>().await;
                drop(tx);
            });
            Ok(tonic::Response::new(ReceiverStream::new(rx)))
        }
    }

    async fn start_stub_listener(listener: tokio::net::TcpListener) -> CancellationToken {
        let shutdown = CancellationToken::new();
        let token = shutdown.clone();
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(ListenerServiceServer::new(StubListener))
                .serve_with_incoming_shutdown(
                    TcpListenerStream::new(listener),
                    token.cancelled(),
                )
                .await
                .unwrap();
        });
        tokio::task::yield_now().await;
        shutdown
    }

    /// Accept TCP connections and hold them open (audio socket stub).
    async fn start_audio_acceptor(listener: tokio::net::TcpListener) -> CancellationToken {
        let shutdown = CancellationToken::new();
        let token = shutdown.clone();
        tokio::spawn(async move {
            let mut conns: Vec<tokio::net::TcpStream> = Vec::new();
            loop {
                tokio::select! {
                    _ = token.cancelled() => return,
                    result = listener.accept() => {
                        match result {
                            Ok((stream, _)) => conns.push(stream),
                            Err(_) => return,
                        }
                    }
                }
            }
        });
        shutdown
    }

    /// Poll until readiness reaches `target`, or panic after ~10s.
    async fn poll_readiness(conn: &ManagedConnectionHandle, target: Readiness) {
        for _ in 0..100 {
            if conn.readiness() == target {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "readiness did not reach {:?} (stuck at {:?})",
            target,
            conn.readiness()
        );
    }

    /// Recovery loop: probe → connect gRPC + audio → Ready.
    /// Set Degraded → recovery loop reconnects → Ready (twice).
    ///
    /// Keeps stub servers alive for the whole test (no port rebinding).
    /// The recovery loop creates a fresh ListenerClient on each cycle,
    /// reconnecting to the same running server.
    #[tokio::test]
    async fn recovery_loop_reconnects_after_stream_loss() {
        // Start stub servers (kept alive for the whole test).
        let grpc_tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let grpc_addr = grpc_tcp.local_addr().unwrap();
        let audio_tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let audio_addr = audio_tcp.local_addr().unwrap();

        let _grpc = start_stub_listener(grpc_tcp).await;
        let _audio = start_audio_acceptor(audio_tcp).await;

        let lifecycle = LifecycleSupervisor::new();
        let connection = lifecycle.register_connection("listener");
        let audio_sink = ListenerAudioSink::new();
        let (p1_tx, _p1_rx) = mpsc::channel(16);
        let (p2_tx, _p2_rx) = mpsc::channel(16);
        let shutdown = CancellationToken::new();

        tokio::spawn(run_recovery_loop(
            format!("http://{grpc_addr}"),
            audio_addr.to_string(),
            lifecycle.spawner(),
            connection.clone(),
            fast_policy(),
            audio_sink.clone(),
            p1_tx,
            p2_tx,
            true,
            shutdown.clone(),
        ));

        // Phase 1: initial connect → Ready.
        poll_readiness(&connection, Readiness::Ready).await;

        // Phase 2: simulate ASR stream loss → recovery loop reconnects.
        connection.set_readiness(Readiness::Degraded);
        poll_readiness(&connection, Readiness::Ready).await;

        // Phase 3: second recovery cycle (proves recovery is repeatable).
        connection.set_readiness(Readiness::Degraded);
        poll_readiness(&connection, Readiness::Ready).await;

        shutdown.cancel();
    }
}
