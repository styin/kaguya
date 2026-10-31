//! gRPC client and raw audio forwarder for the Listener runtime.
//!
//! The Listener exposes two server endpoints that the Gateway connects to as
//! a client:
//!
//! - **gRPC bidirectional stream** — initializes scope and carries a control
//!   extension point; ASR events enter the Gateway's priority input stream.
//! - **Raw TCP socket** — receives length-prefixed audio frames forwarded
//!   from the WebSocket endpoint. Audio bypasses gRPC to avoid protobuf
//!   serialization overhead at 50 fps.
//!
//! Unlike [`TalkerClient`](super::talker::TalkerClient), there is no
//! persistent client instance — each recovery cycle creates a fresh
//! [`ListenerClient`], establishes new streams, and installs a new audio
//! sender into the shared [`ListenerAudioSink`](super::audio_sink::ListenerAudioSink).
//!
//! [`run_recovery_loop`] runs as a background task and handles endpoint
//! probing, connection setup, and automatic reconnection on stream loss.

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tracing::{debug, info, warn};

use crate::core::types::InputEvent;
use crate::lifecycle::{ManagedConnectionHandle, Readiness, ReconnectPolicy, TaskSpawner};
use kaguya_protocol::proto;
use kaguya_protocol::proto::listener_service_client::ListenerServiceClient;
use kaguya_protocol::session_scope::SessionScope;

/// Single-use gRPC + audio client for one Listener connection lifecycle.
///
/// Created fresh by [`run_recovery_loop`] on each reconnection cycle.
/// [`start`](Self::start) establishes both the gRPC event stream and the
/// audio TCP socket, spawning background tasks for each.
pub struct ListenerClient {
    grpc_endpoint: String,
    audio_addr: String,
    tasks: TaskSpawner,
    connection: ManagedConnectionHandle,
    reconnect: ReconnectPolicy,
    scope: SessionScope,
}

impl ListenerClient {
    pub fn new(
        grpc_endpoint: String,
        audio_addr: String,
        tasks: TaskSpawner,
        connection: ManagedConnectionHandle,
        scope: SessionScope,
    ) -> Self {
        Self::with_reconnect_policy(
            grpc_endpoint,
            audio_addr,
            tasks,
            connection,
            ReconnectPolicy::default(),
            scope,
        )
    }

    pub fn with_reconnect_policy(
        grpc_endpoint: String,
        audio_addr: String,
        tasks: TaskSpawner,
        connection: ManagedConnectionHandle,
        reconnect: ReconnectPolicy,
        scope: SessionScope,
    ) -> Self {
        Self {
            grpc_endpoint,
            audio_addr,
            tasks,
            connection,
            reconnect,
            scope,
        }
    }

    /// Establish the gRPC event stream and audio TCP socket, spawning
    /// background receiver/forwarder tasks. Returns the audio sender for
    /// installation into [`ListenerAudioSink`](super::audio_sink::ListenerAudioSink).
    pub async fn start(
        &self,
        p1_tx: mpsc::Sender<InputEvent>,
        p2_tx: mpsc::Sender<InputEvent>,
    ) -> anyhow::Result<mpsc::Sender<bytes::Bytes>> {
        // ── gRPC event stream for ASR events ──
        let mut inbound = Self::connect_stream_with_policy(
            &self.grpc_endpoint,
            self.connection.clone(),
            self.reconnect,
            self.scope,
        )
        .await?;

        info!(addr = %self.grpc_endpoint, "Listener gRPC event stream established");

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
            warn!("Listener event stream ended");
        });

        // ── Raw TCP socket forwarder for audio ──
        let (audio_tx, mut audio_rx) = mpsc::channel::<bytes::Bytes>(512);
        let audio_addr = self.audio_addr.clone();
        let audio_connection = self.connection.clone();
        let audio_reconnect = self.reconnect;
        let audio_header = audio_header(self.scope);

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
                        if stream.write_all(&audio_header).await.is_err() {
                            audio_connection.set_readiness(Readiness::Degraded);
                            continue;
                        }
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

    /// Open a gRPC event stream with bounded retries and exponential backoff.
    async fn connect_stream_with_policy(
        endpoint: &str,
        connection: ManagedConnectionHandle,
        reconnect: ReconnectPolicy,
        scope: SessionScope,
    ) -> anyhow::Result<tonic::Streaming<proto::ListenerOutput>> {
        let retry_delays = reconnect.retry_delays();
        connection.set_readiness(Readiness::Starting);
        for attempt in 1..=reconnect.max_attempts() {
            match tokio::time::timeout(
                reconnect.attempt_timeout(),
                Self::connect_stream_once(endpoint, scope),
            )
            .await
            {
                Ok(Ok(stream)) => {
                    connection.set_readiness(Readiness::Ready);
                    return Ok(stream);
                }
                Ok(Err(e)) if is_scope_mismatch(&e) => {
                    connection.set_readiness(Readiness::Degraded);
                    return Err(e);
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
        scope: SessionScope,
    ) -> anyhow::Result<tonic::Streaming<proto::ListenerOutput>> {
        let channel = Channel::from_shared(endpoint.to_string())?
            .connect()
            .await?;
        let mut client = ListenerServiceClient::new(channel);
        // The request carries only ListenerInit, queued before response headers
        // are awaited. Listener keeps the subscription after the half-close.
        let init = proto::ListenerInput {
            payload: Some(proto::listener_input::Payload::Init(proto::ListenerInit {
                session: Some(scope.to_proto()),
            })),
        };
        Ok(client.stream(tokio_stream::once(init)).await?.into_inner())
    }

    /// Open the raw audio TCP socket with bounded retries.
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

/// Raw transport framing: 7-byte protocol marker, then two 16-byte UUIDs.
/// Constructed once per audio forwarder; never applied to individual frames.
fn audio_header(scope: SessionScope) -> [u8; 39] {
    let mut header = [0u8; 39];
    header[..7].copy_from_slice(b"KAGUYA1");
    header[7..23].copy_from_slice(scope.instance_id().as_bytes());
    header[23..].copy_from_slice(scope.session_id().as_bytes());
    header
}

/// Listener answers FAILED_PRECONDITION when it serves a different scope.
fn is_scope_mismatch(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<tonic::Status>()
        .is_some_and(|status| status.code() == tonic::Code::FailedPrecondition)
}

/// Background loop: probe endpoint → create [`ListenerClient`] → start
/// streams → install audio sink → watch for Degraded → clear sink →
/// reconnect. Runs until `shutdown` is cancelled.
///
/// Each cycle creates a fresh client because the Listener has no persistent
/// channel — `start()` establishes new gRPC and TCP connections, and the
/// previous cycle's spawned tasks exit naturally when their streams end.
/// Returns an error only when the Listener serves a different scope.
pub async fn run_recovery_loop(
    grpc_endpoint: String,
    audio_addr: String,
    task_spawner: TaskSpawner,
    connection: ManagedConnectionHandle,
    reconnect: ReconnectPolicy,
    listener_audio: crate::clients::audio_sink::ListenerAudioSink,
    p1_tx: mpsc::Sender<InputEvent>,
    p2_tx: mpsc::Sender<InputEvent>,
    expected_runtime: bool,
    scope: SessionScope,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    use crate::clients::probe::{sleep_probe_interval, wait_for_grpc_endpoint};

    if !expected_runtime && p1_tx.is_closed() {
        connection.set_readiness(Readiness::Stopped);
        listener_audio.clear().await;
        return Ok(());
    }

    loop {
        tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Ok(()),
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
            scope,
        );
        match listener.start(p1_tx.clone(), p2_tx.clone()).await {
            Ok(audio_tx) => {
                listener_audio.install(audio_tx).await;
                info!("Listener connected (gRPC + audio socket)");

                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => return Ok(()),
                        _ = sleep_probe_interval() => {}
                    }
                    if connection.readiness() == Readiness::Degraded {
                        info!("Listener connection lost, reconnecting");
                        listener_audio.clear().await;
                        break;
                    }
                }
            }
            Err(e) if is_scope_mismatch(&e) => {
                listener_audio.clear().await;
                return Err(e.context("Listener serves a different instance/session"));
            }
            Err(e) => {
                warn!("Listener startup failed after endpoint readiness: {e}");
                if expected_runtime {
                    connection.set_readiness(Readiness::Starting);
                }
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => return Ok(()),
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

    fn test_scope() -> SessionScope {
        SessionScope::parse(
            "11111111-1111-4111-8111-111111111111",
            "22222222-2222-4222-8222-222222222222",
        )
        .unwrap()
    }

    #[test]
    fn raw_header_contains_fixed_session_once_without_text_uuid_encoding() {
        let scope = test_scope();
        let header = audio_header(scope);
        assert_eq!(header.len(), 39);
        assert_eq!(&header[..7], b"KAGUYA1");
        assert_eq!(&header[7..23], scope.instance_id().as_bytes());
        assert_eq!(&header[23..], scope.session_id().as_bytes());
    }

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
            test_scope(),
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
            "not a valid uri".into(),
            "127.0.0.1:1".into(),
            lifecycle.spawner(),
            connection.clone(),
            fast_policy(),
            test_scope(),
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

    use crate::clients::audio_sink::ListenerAudioSink;
    use kaguya_protocol::proto::listener_service_server::{
        ListenerService as ListenerServiceTrait, ListenerServiceServer,
    };
    use tokio_stream::wrappers::{ReceiverStream, TcpListenerStream};

    /// `Some(code)` rejects initialization with that status.
    struct StubListener(Option<tonic::Code>);

    #[tonic::async_trait]
    impl ListenerServiceTrait for StubListener {
        type StreamStream = ReceiverStream<Result<proto::ListenerOutput, tonic::Status>>;

        async fn stream(
            &self,
            request: tonic::Request<tonic::Streaming<proto::ListenerInput>>,
        ) -> Result<tonic::Response<Self::StreamStream>, tonic::Status> {
            if let Some(code) = self.0 {
                return Err(tonic::Status::new(code, "rejected"));
            }
            let mut input = request.into_inner();
            let first = input
                .message()
                .await?
                .expect("initialization must precede response headers");
            let Some(proto::listener_input::Payload::Init(init)) = first.payload else {
                return Err(tonic::Status::invalid_argument(
                    "expected Listener init first",
                ));
            };
            assert_eq!(init.session, Some(test_scope().to_proto()));
            // The client sends only ListenerInit and half-closes; like the real
            // Listener, keep the subscription open afterwards.
            assert!(input.message().await?.is_none());
            let (tx, rx) = mpsc::channel(1);
            tokio::spawn(async move {
                let _tx = tx;
                std::future::pending::<()>().await;
            });
            Ok(tonic::Response::new(ReceiverStream::new(rx)))
        }
    }

    async fn start_stub_listener(
        listener: tokio::net::TcpListener,
        rejection: Option<tonic::Code>,
    ) -> CancellationToken {
        let shutdown = CancellationToken::new();
        let token = shutdown.clone();
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(ListenerServiceServer::new(StubListener(rejection)))
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), token.cancelled())
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
                            Ok((mut stream, _)) => {
                                use tokio::io::AsyncReadExt;
                                let mut header = [0u8; 39];
                                stream.read_exact(&mut header).await.unwrap();
                                assert_eq!(header, audio_header(test_scope()));
                                conns.push(stream);
                            },
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

    /// Proves repeatable recovery: connect → Degraded → reconnect → Ready,
    /// twice in a row against the same running stub servers.
    #[tokio::test]
    async fn recovery_loop_reconnects_after_stream_loss() {
        // Start stub servers (kept alive for the whole test).
        let grpc_tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let grpc_addr = grpc_tcp.local_addr().unwrap();
        let audio_tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let audio_addr = audio_tcp.local_addr().unwrap();

        let _grpc = start_stub_listener(grpc_tcp, None).await;
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
            test_scope(),
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

    #[tokio::test]
    async fn recovery_stops_when_listener_serves_another_scope() {
        let grpc_tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let grpc_addr = grpc_tcp.local_addr().unwrap();
        let _grpc = start_stub_listener(grpc_tcp, Some(tonic::Code::FailedPrecondition)).await;
        let lifecycle = LifecycleSupervisor::new();
        let (p1_tx, _p1_rx) = mpsc::channel(16);
        let (p2_tx, _p2_rx) = mpsc::channel(16);
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            run_recovery_loop(
                format!("http://{grpc_addr}"),
                "127.0.0.1:1".into(),
                lifecycle.spawner(),
                lifecycle.register_connection("listener"),
                fast_policy(),
                ListenerAudioSink::new(),
                p1_tx,
                p2_tx,
                true,
                test_scope(),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("a scope mismatch stops recovery");
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("different instance/session"));
    }
}
