//! gRPC client for the Talker runtime.
//!
//! Wraps a persistent [`TalkerServiceClient`] channel with lifecycle-aware
//! reconnection. The main event loop calls [`TalkerClient::dispatch`] to open
//! a bidi Converse stream and [`TalkerClient::barge_in`] to inject inline
//! interrupts on the active stream.
//!
//! [`TalkerClient::run_recovery_loop`] runs as a background task: it probes
//! the endpoint, connects, sends the current persona, and watches for
//! [`Readiness::Degraded`] to trigger reconnection.

use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tracing::{debug, error, info, warn};

use crate::lifecycle::{ManagedConnectionHandle, Readiness, ReconnectPolicy, TaskSpawner};
use crate::proto;
use crate::proto::talker_service_client::TalkerServiceClient;
use crate::session_scope::SessionScope;
use crate::telemetry::TelemetryClient;

fn trigger_name(trigger: Option<&proto::generation_start::Trigger>) -> &'static str {
    match trigger {
        Some(proto::generation_start::Trigger::UserInput(_)) => "user_input",
        Some(proto::generation_start::Trigger::ToolResult(_)) => "tool_result",
        Some(proto::generation_start::Trigger::ReasonerResult(_)) => "reasoner_result",
        Some(proto::generation_start::Trigger::ReasonerProgress(_)) => "reasoner_progress",
        Some(proto::generation_start::Trigger::ReasonerFailure(_)) => "reasoner_failure",
        Some(proto::generation_start::Trigger::Silence(_)) => "silence",
        None => "missing",
    }
}

/// Persistent gRPC client for the Talker process.
///
/// Holds a reusable channel and the sender half of the active Converse bidi
/// stream. Cloneable — the main event loop and the recovery loop share the
/// same instance.
#[derive(Clone)]
pub struct TalkerClient {
    inner: Arc<RwLock<Option<TalkerServiceClient<Channel>>>>,
    endpoint: String,
    stream_tx: Arc<Mutex<Option<mpsc::Sender<proto::TalkerInput>>>>,
    tasks: TaskSpawner,
    connection: ManagedConnectionHandle,
    reconnect: ReconnectPolicy,
    telemetry: Option<TelemetryClient>,
    scope: SessionScope,
}

impl TalkerClient {
    pub fn new(
        endpoint: String,
        tasks: TaskSpawner,
        connection: ManagedConnectionHandle,
        scope: SessionScope,
    ) -> Self {
        Self::with_reconnect_policy(
            endpoint,
            tasks,
            connection,
            ReconnectPolicy::default(),
            scope,
        )
    }

    pub fn with_reconnect_policy(
        endpoint: String,
        tasks: TaskSpawner,
        connection: ManagedConnectionHandle,
        reconnect: ReconnectPolicy,
        scope: SessionScope,
    ) -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
            endpoint,
            stream_tx: Arc::new(Mutex::new(None)),
            tasks,
            connection,
            reconnect,
            telemetry: None,
            scope,
        }
    }

    pub fn with_telemetry(mut self, telemetry: TelemetryClient) -> Self {
        self.telemetry = Some(telemetry);
        self
    }

    pub fn readiness(&self) -> Readiness {
        self.connection.readiness()
    }

    pub fn set_readiness(&self, readiness: Readiness) {
        self.connection.set_readiness(readiness);
    }

    pub fn is_ready(&self) -> bool {
        self.readiness() == Readiness::Ready
    }

    /// Attempt to establish a gRPC channel using the configured reconnect
    /// policy. A connected channel stays Starting until UpdatePersona accepts
    /// the configured scope. Exhaustion marks the connection Degraded.
    pub async fn try_connect(&self) -> bool {
        match Self::connect_with_policy(&self.endpoint, self.connection.clone(), self.reconnect)
            .await
        {
            Some(client) => {
                *self.inner.write().await = Some(client);
                info!(addr = %self.endpoint, "connected to Talker");
                true
            }
            None => {
                warn!("Talker not ready after reconnect policy was exhausted");
                false
            }
        }
    }

    /// Open a bidi Converse stream, send `ctx` as the start payload, and
    /// forward Talker outputs to `output_tx`.
    ///
    /// Returns a [`CancellationToken`] that the caller can cancel to stop the stream.
    /// The stream sender is registered *before* the task spawns so that a
    /// [`barge_in`](Self::barge_in) call racing immediately after this returns
    /// finds the live sender.
    pub async fn dispatch(
        &self,
        mut ctx: proto::GenerationStart,
        output_tx: mpsc::Sender<proto::TalkerOutput>,
    ) -> CancellationToken {
        ctx.session = Some(self.scope.to_proto());
        let token = CancellationToken::new();
        let child = token.child_token();
        let inner = Arc::clone(&self.inner);
        let stream_tx_arc = Arc::clone(&self.stream_tx);
        let endpoint = self.endpoint.clone();
        let connection = self.connection.clone();
        let reconnect = self.reconnect;
        let telemetry = self.telemetry.clone();
        let generation_id = ctx.generation_id.clone();
        let dispatch_started = std::time::Instant::now();

        // Create the bidi channel and register the sender BEFORE spawning.
        // Capacity 64 ⇒ try_send for the start payload never blocks.
        let (tx, rx) = mpsc::channel::<proto::TalkerInput>(64);
        if let Some(telemetry) = &telemetry {
            telemetry.emit(
                "talker.dispatch.started",
                Some(generation_id.clone()),
                None,
                serde_json::json!({
                    "history_count": ctx.prompt_context.as_ref().map_or(0, |prompt| prompt.history.len()),
                    "retrieval_count": ctx.prompt_context.as_ref().map_or(0, |prompt| prompt.retrieval_results.len()),
                    "memory_chars": ctx.prompt_context.as_ref().map_or(0, |prompt| prompt.memory_contents.chars().count()),
                    "tool_count": ctx.prompt_context.as_ref().map_or(0, |prompt| prompt.tools.len()),
                    "user_input_chars": match ctx.trigger.as_ref() {
                        Some(proto::generation_start::Trigger::UserInput(input)) => input.text.chars().count(),
                        _ => 0,
                    },
                    "trigger": trigger_name(ctx.trigger.as_ref()),
                }),
            );
        }
        let _ = tx.try_send(proto::TalkerInput {
            payload: Some(proto::talker_input::Payload::Start(ctx)),
        });
        *stream_tx_arc.lock().await = Some(tx);

        self.tasks.spawn("talker_dispatch", async move {
            let maybe_client = inner.read().await.clone();
            let mut client = match maybe_client {
                Some(client) => client,
                None => {
                    let Some(client) =
                        Self::connect_with_policy(&endpoint, connection.clone(), reconnect).await
                    else {
                        error!("Talker reconnect failed after policy was exhausted");
                        *stream_tx_arc.lock().await = None;
                        return;
                    };
                    *inner.write().await = Some(client.clone());
                    client
                }
            };

            let outbound = ReceiverStream::new(rx);
            let request = tonic::Request::new(outbound);
            let mut inbound = match client.converse(request).await {
                Ok(resp) => {
                    connection.set_readiness(Readiness::Ready);
                    resp.into_inner()
                }
                Err(e) => {
                    connection.set_readiness(Readiness::Degraded);
                    error!("Converse failed: {e}");
                    *stream_tx_arc.lock().await = None;
                    return;
                }
            };

            let mut first_output_sent = false;
            let mut first_sentence_sent = false;
            loop {
                tokio::select! {
                    _ = child.cancelled() => {
                        debug!("Talker dispatch cancelled externally");
                        break;
                    }
                    result = inbound.message() => {
                        match result {
                            Ok(Some(output)) => {
                                if output.generation_id.is_empty() || output.generation_id != generation_id {
                                    connection.set_readiness(Readiness::Degraded);
                                    error!(%generation_id, response_generation = %output.generation_id, "Talker response generation is missing or differs from its stream");
                                    break;
                                }
                                if let Some(telemetry) = &telemetry {
                                    if !first_output_sent {
                                        first_output_sent = true;
                                        telemetry.emit(
                                            "talker.first_output",
                                            Some(generation_id.clone()),
                                            None,
                                            serde_json::json!({
                                                "duration_ms": dispatch_started.elapsed().as_millis() as u64,
                                            }),
                                        );
                                    }
                                    if !first_sentence_sent
                                        && matches!(
                                            output.payload.as_ref(),
                                            Some(proto::talker_output::Payload::Sentence(_))
                                        )
                                    {
                                        first_sentence_sent = true;
                                        telemetry.emit(
                                            "talker.first_sentence",
                                            Some(generation_id.clone()),
                                            None,
                                            serde_json::json!({
                                                "duration_ms": dispatch_started.elapsed().as_millis() as u64,
                                            }),
                                        );
                                    }
                                }
                                if output_tx.send(output).await.is_err() { break; }
                            }
                            Ok(None) => break,
                            Err(e) => {
                                connection.set_readiness(Readiness::Degraded);
                                error!("Talker stream error: {e}");
                                break;
                            }
                        }
                    }
                }
            }

            *stream_tx_arc.lock().await = None;
        });

        token
    }

    /// Attempt connection with bounded retries and exponential backoff.
    async fn connect_with_policy(
        endpoint: &str,
        connection: ManagedConnectionHandle,
        reconnect: ReconnectPolicy,
    ) -> Option<TalkerServiceClient<Channel>> {
        let retry_delays = reconnect.retry_delays();
        connection.set_readiness(Readiness::Starting);
        for attempt in 1..=reconnect.max_attempts() {
            match tokio::time::timeout(reconnect.attempt_timeout(), Self::connect_once(endpoint))
                .await
            {
                Ok(Ok(client)) => {
                    // Transport connectivity does not establish persona/session readiness.
                    // Recovery marks Ready only after UpdatePersona succeeds.
                    return Some(client);
                }
                Ok(Err(e)) => {
                    warn!(
                        attempt,
                        max_attempts = reconnect.max_attempts(),
                        "Talker connect attempt failed: {e}"
                    );
                    if let Some(delay) = retry_delays.get(attempt - 1) {
                        tokio::time::sleep(*delay).await;
                    }
                }
                Err(_) => {
                    warn!(
                        attempt,
                        max_attempts = reconnect.max_attempts(),
                        timeout_ms = reconnect.attempt_timeout().as_millis(),
                        "Talker connect attempt timed out"
                    );
                    if let Some(delay) = retry_delays.get(attempt - 1) {
                        tokio::time::sleep(*delay).await;
                    }
                }
            }
        }
        connection.set_readiness(Readiness::Degraded);
        None
    }

    async fn connect_once(endpoint: &str) -> Result<TalkerServiceClient<Channel>, String> {
        let channel = Channel::from_shared(endpoint.to_string())
            .map_err(|e| format!("bad Talker endpoint: {e}"))?
            .connect()
            .await
            .map_err(|e| e.to_string())?;
        Ok(TalkerServiceClient::new(channel))
    }

    /// Send inline barge-in on the active Converse stream.
    pub async fn barge_in(&self) {
        let guard = self.stream_tx.lock().await;
        if let Some(tx) = guard.as_ref() {
            let msg = proto::TalkerInput {
                payload: Some(proto::talker_input::Payload::BargeIn(
                    proto::BargeInSignal {},
                )),
            };
            if tx.send(msg).await.is_err() {
                debug!("barge-in: stream already closed");
            } else {
                debug!("→ BargeIn (inline)");
            }
        } else {
            debug!("barge-in: no active stream (Talker idle)");
        }
    }

    /// Hint the Talker to prefill its KV cache for the next turn.
    pub async fn prefill_cache(&self, context: proto::PromptContext) {
        let Some(mut client) = self.inner.read().await.clone() else {
            return;
        };
        debug!("→ PrefillCache");
        let payload = proto::PrefillRequest {
            session: Some(self.scope.to_proto()),
            prompt_context: Some(context),
        };
        let request = tonic::Request::new(payload);
        if let Err(e) = client.prefill_cache(request).await {
            warn!("PrefillCache failed: {e}");
        }
    }

    /// Background loop: probe endpoint → connect → send persona → watch for
    /// Degraded → reconnect. Runs until `shutdown` is cancelled.
    ///
    /// Each reconnection re-reads the shared `persona` snapshot so any
    /// file-watcher or memory updates are picked up automatically.
    pub async fn run_recovery_loop(
        &self,
        persona: Arc<RwLock<proto::PersonaConfig>>,
        expected_runtime: bool,
        shutdown: CancellationToken,
    ) -> Result<(), tonic::Status> {
        use crate::clients::probe::{sleep_probe_interval, wait_for_grpc_endpoint};

        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => return Ok(()),
                _ = wait_for_grpc_endpoint(
                    "talker",
                    &self.endpoint,
                    expected_runtime,
                    |readiness| self.set_readiness(readiness),
                ) => {}
            }

            loop {
                if shutdown.is_cancelled() {
                    return Ok(());
                }
                if self.try_connect().await {
                    let persona_snapshot = persona.read().await.clone();
                    match self.update_persona(persona_snapshot).await {
                        Ok(()) => break,
                        Err(error) if error.code() == tonic::Code::FailedPrecondition => {
                            return Err(error)
                        }
                        Err(_) => {}
                    }
                }
                if expected_runtime {
                    self.set_readiness(Readiness::Starting);
                }
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => return Ok(()),
                    _ = sleep_probe_interval() => {}
                }
            }

            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => return Ok(()),
                    _ = sleep_probe_interval() => {}
                }
                if self.readiness() == Readiness::Degraded {
                    info!("Talker connection lost, reconnecting");
                    break;
                }
            }
        }
    }

    /// Send an UpdatePersona RPC with the latest soul, persona identity content, and memory.
    pub async fn update_persona(&self, config: proto::PersonaConfig) -> Result<(), tonic::Status> {
        let guard = self.inner.read().await;
        let Some(mut client) = guard.clone() else {
            if self.connection.readiness() != Readiness::Starting {
                self.connection.set_readiness(Readiness::Degraded);
            }
            warn!("cannot UpdatePersona: Talker not connected");
            return Err(tonic::Status::unavailable("Talker not connected"));
        };
        drop(guard);
        info!(
            soul_len = config.soul_md.len(),
            identity_len = config.identity_md.len(),
            memory_len = config.memory_md.len(),
            "→ UpdatePersona"
        );
        let request = tonic::Request::new(proto::UpdatePersonaRequest {
            session: Some(self.scope.to_proto()),
            persona: Some(config),
        });
        if let Err(e) = client.update_persona(request).await {
            self.connection.set_readiness(Readiness::Degraded);
            error!("UpdatePersona failed: {e}");
            Err(e)
        } else {
            self.connection.set_readiness(Readiness::Ready);
            Ok(())
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
            "00000000-0000-4000-8000-000000000001",
            "00000000-0000-4000-8000-000000000002",
        )
        .unwrap()
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
    async fn try_connect_marks_connection_degraded_after_policy_exhaustion() {
        let lifecycle = LifecycleSupervisor::new();
        let connection = lifecycle.register_connection("talker");
        let talker = TalkerClient::with_reconnect_policy(
            "not a valid uri".into(),
            lifecycle.spawner(),
            connection.clone(),
            fast_policy(),
            test_scope(),
        );

        talker.try_connect().await;

        assert_eq!(connection.readiness(), Readiness::Degraded);
    }

    // BUG-SCOPING: Without a recovery loop, readiness stays Degraded
    // permanently after connection loss. The bare TalkerClient has no
    // built-in reconnect — run_recovery_loop() wraps it with one.
    #[tokio::test]
    async fn degraded_readiness_has_no_automatic_recovery() {
        let lifecycle = LifecycleSupervisor::new();
        let connection = lifecycle.register_connection("talker");
        let talker = TalkerClient::with_reconnect_policy(
            "not a valid uri".into(),
            lifecycle.spawner(),
            connection.clone(),
            fast_policy(),
            test_scope(),
        );

        talker.try_connect().await;
        assert_eq!(connection.readiness(), Readiness::Degraded);

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            connection.readiness(),
            Readiness::Degraded,
            "readiness should stay Degraded: no recovery loop exists"
        );
    }

    // ── Recovery loop integration tests ──

    use crate::proto::talker_service_server::{
        TalkerService as TalkerServiceTrait, TalkerServiceServer,
    };
    use tokio_stream::wrappers::TcpListenerStream;

    struct StubTalker;

    /// Answers every RPC with one status code.
    struct RejectingTalker(tonic::Code);

    #[tonic::async_trait]
    impl TalkerServiceTrait for RejectingTalker {
        type ConverseStream = ReceiverStream<Result<proto::TalkerOutput, tonic::Status>>;

        async fn converse(
            &self,
            _req: tonic::Request<tonic::Streaming<proto::TalkerInput>>,
        ) -> Result<tonic::Response<Self::ConverseStream>, tonic::Status> {
            Err(tonic::Status::new(self.0, "rejected"))
        }

        async fn prefill_cache(
            &self,
            _req: tonic::Request<proto::PrefillRequest>,
        ) -> Result<tonic::Response<proto::PrefillAck>, tonic::Status> {
            Err(tonic::Status::new(self.0, "rejected"))
        }

        async fn update_persona(
            &self,
            _req: tonic::Request<proto::UpdatePersonaRequest>,
        ) -> Result<tonic::Response<proto::PersonaAck>, tonic::Status> {
            Err(tonic::Status::new(self.0, "rejected"))
        }
    }

    #[tonic::async_trait]
    impl TalkerServiceTrait for StubTalker {
        type ConverseStream = ReceiverStream<Result<proto::TalkerOutput, tonic::Status>>;

        async fn converse(
            &self,
            _req: tonic::Request<tonic::Streaming<proto::TalkerInput>>,
        ) -> Result<tonic::Response<Self::ConverseStream>, tonic::Status> {
            let (_tx, rx) = mpsc::channel(1);
            Ok(tonic::Response::new(ReceiverStream::new(rx)))
        }

        async fn prefill_cache(
            &self,
            _req: tonic::Request<proto::PrefillRequest>,
        ) -> Result<tonic::Response<proto::PrefillAck>, tonic::Status> {
            Ok(tonic::Response::new(proto::PrefillAck {}))
        }

        async fn update_persona(
            &self,
            _req: tonic::Request<proto::UpdatePersonaRequest>,
        ) -> Result<tonic::Response<proto::PersonaAck>, tonic::Status> {
            Ok(tonic::Response::new(proto::PersonaAck {}))
        }
    }

    async fn start_stub_talker(listener: tokio::net::TcpListener) -> CancellationToken {
        let shutdown = CancellationToken::new();
        let token = shutdown.clone();
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(TalkerServiceServer::new(StubTalker))
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), token.cancelled())
                .await
                .unwrap();
        });
        tokio::task::yield_now().await;
        shutdown
    }

    async fn run_recovery_against(code: tonic::Code) -> Option<Result<(), tonic::Status>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(TalkerServiceServer::new(RejectingTalker(code)))
                .serve_with_incoming(TcpListenerStream::new(listener))
                .await
                .unwrap();
        });
        let lifecycle = LifecycleSupervisor::new();
        let client = TalkerClient::with_reconnect_policy(
            format!("http://{address}"),
            lifecycle.spawner(),
            lifecycle.register_connection("talker"),
            fast_policy(),
            test_scope(),
        );
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            client.run_recovery_loop(
                Arc::new(RwLock::new(proto::PersonaConfig::default())),
                true,
                CancellationToken::new(),
            ),
        )
        .await
        .ok();
        server.abort();
        result
    }

    #[tokio::test]
    async fn recovery_stops_only_on_scope_mismatch() {
        let mismatch = run_recovery_against(tonic::Code::FailedPrecondition).await;
        assert_eq!(
            mismatch
                .expect("mismatch stops recovery")
                .unwrap_err()
                .code(),
            tonic::Code::FailedPrecondition
        );
        // A malformed request is a Gateway bug, not a binding failure: keep retrying.
        assert!(run_recovery_against(tonic::Code::InvalidArgument)
            .await
            .is_none());
    }

    /// Bind to `addr`, retrying for up to 2s if the port is still in
    /// TIME_WAIT from the previous server (common on Windows).
    async fn bind_with_retry(addr: std::net::SocketAddr) -> tokio::net::TcpListener {
        for _ in 0..20 {
            if let Ok(listener) = tokio::net::TcpListener::bind(addr).await {
                return listener;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("failed to bind {addr} after retries (port still in use)");
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

    /// Proves the full recovery cycle: start server → Ready → kill server →
    /// Degraded → restart server → Ready again.
    #[tokio::test]
    async fn recovery_loop_reconnects_after_degraded() {
        // Start stub gRPC server on a random port.
        let tcp1 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp1.local_addr().unwrap();
        let server1 = start_stub_talker(tcp1).await;

        let lifecycle = LifecycleSupervisor::new();
        let connection = lifecycle.register_connection("talker");
        let talker = TalkerClient::with_reconnect_policy(
            format!("http://{addr}"),
            lifecycle.spawner(),
            connection.clone(),
            fast_policy(),
            test_scope(),
        );

        let persona = Arc::new(RwLock::new(proto::PersonaConfig::default()));
        let shutdown = CancellationToken::new();

        let t = talker.clone();
        let p = persona.clone();
        let s = shutdown.clone();
        tokio::spawn(async move {
            let _ = t.run_recovery_loop(p, true, s).await;
        });

        // Phase 1: recovery loop should probe, connect, send persona → Ready.
        poll_readiness(&connection, Readiness::Ready).await;

        // Phase 2: kill server, simulate dispatch failure setting Degraded.
        server1.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;
        connection.set_readiness(Readiness::Degraded);

        // Phase 3: start a new server on the same port (retries for port release).
        let tcp2 = bind_with_retry(addr).await;
        let _server2 = start_stub_talker(tcp2).await;

        // Phase 4: recovery loop should detect Degraded, re-probe, reconnect.
        poll_readiness(&connection, Readiness::Ready).await;

        shutdown.cancel();
    }

    fn start_for() -> proto::GenerationStart {
        crate::core::context::assemble_from_data(
            "generation-1",
            "hello",
            vec![],
            "",
            vec![],
            vec![],
            &[],
        )
    }

    struct HandshakeTalker {
        mismatch: Option<(u32, &'static str)>,
        received: mpsc::UnboundedSender<(SessionScope, String)>,
    }

    #[tonic::async_trait]
    impl TalkerServiceTrait for HandshakeTalker {
        type ConverseStream = ReceiverStream<Result<proto::TalkerOutput, tonic::Status>>;

        async fn converse(
            &self,
            req: tonic::Request<tonic::Streaming<proto::TalkerInput>>,
        ) -> Result<tonic::Response<Self::ConverseStream>, tonic::Status> {
            // Scope is an explicit first-message contract, with no competing
            // custom metadata representation.

            let mut input = req.into_inner();
            let first = input
                .message()
                .await?
                .ok_or_else(|| tonic::Status::invalid_argument("missing start"))?;
            let Some(proto::talker_input::Payload::Start(start)) = first.payload else {
                return Err(tonic::Status::invalid_argument("start must be first"));
            };
            let scope = SessionScope::from_proto(start.session.as_ref())
                .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
            assert_eq!(scope, test_scope());
            self.received
                .send((scope, start.generation_id.clone()))
                .unwrap();
            // Reading the first message before response headers is deliberate:
            // this would deadlock if the client waited for headers before Start.
            use proto::talker_output::Payload;
            let payloads = [
                Payload::ResponseStarted(proto::ResponseStarted {}),
                Payload::Sentence(proto::SentenceEvent {
                    text: "hello".into(),
                }),
                Payload::Emotion(proto::EmotionEvent {
                    emotion: "joy".into(),
                }),
                Payload::ToolRequest(proto::ToolRequest {
                    request_id: "request-1".into(),
                    tool_name: "read_file".into(),
                    args_json: "{}".into(),
                }),
                Payload::DelegateRequest(proto::DelegateRequest {
                    task_id: "task-1".into(),
                    description: "research".into(),
                }),
                Payload::BargeInAck(proto::BargeInAck {
                    spoken_text: "hello".into(),
                    unspoken_text: String::new(),
                }),
                Payload::ResponseComplete(proto::ResponseComplete {
                    was_interrupted: false,
                }),
            ];
            let (tx, rx) = mpsc::channel(payloads.len());
            for (index, payload) in payloads.into_iter().enumerate() {
                let seq = index as u32 + 1;
                let generation_id = match self.mismatch {
                    Some((mismatched_seq, value)) if mismatched_seq == seq => value.to_owned(),
                    _ => start.generation_id.clone(),
                };
                tx.send(Ok(proto::TalkerOutput {
                    seq,
                    generation_id,
                    payload: Some(payload),
                }))
                .await
                .unwrap();
            }
            Ok(tonic::Response::new(ReceiverStream::new(rx)))
        }

        async fn prefill_cache(
            &self,
            req: tonic::Request<proto::PrefillRequest>,
        ) -> Result<tonic::Response<proto::PrefillAck>, tonic::Status> {
            assert_eq!(req.get_ref().session, Some(test_scope().to_proto()));
            assert!(req.get_ref().prompt_context.is_some());
            Ok(tonic::Response::new(proto::PrefillAck {}))
        }

        async fn update_persona(
            &self,
            req: tonic::Request<proto::UpdatePersonaRequest>,
        ) -> Result<tonic::Response<proto::PersonaAck>, tonic::Status> {
            assert_eq!(req.get_ref().session, Some(test_scope().to_proto()));
            assert!(req.get_ref().persona.is_some());
            Ok(tonic::Response::new(proto::PersonaAck {}))
        }
    }

    async fn start_handshake_talker(
        mismatch: Option<(u32, &'static str)>,
    ) -> (
        String,
        mpsc::UnboundedReceiver<(SessionScope, String)>,
        CancellationToken,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let shutdown = CancellationToken::new();
        let token = shutdown.clone();
        let (received, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(TalkerServiceServer::new(HandshakeTalker {
                    mismatch,
                    received,
                }))
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), token.cancelled())
                .await
                .unwrap();
        });
        (endpoint, rx, shutdown)
    }

    #[tokio::test]
    async fn first_start_handshake_carries_scope_and_preserves_generation_provenance() {
        let (endpoint, mut received, shutdown) = start_handshake_talker(None).await;
        let lifecycle = LifecycleSupervisor::new();
        let connection = lifecycle.register_connection("talker");
        let client = TalkerClient::with_reconnect_policy(
            endpoint,
            lifecycle.spawner(),
            connection.clone(),
            fast_policy(),
            test_scope(),
        );
        assert!(client.try_connect().await);
        assert_eq!(connection.readiness(), Readiness::Starting);
        client
            .update_persona(proto::PersonaConfig::default())
            .await
            .unwrap();
        assert_eq!(connection.readiness(), Readiness::Ready);
        client.prefill_cache(proto::PromptContext::default()).await;
        let (tx, mut rx) = mpsc::channel(4);
        client.dispatch(start_for(), tx).await;
        let observed = tokio::time::timeout(Duration::from_secs(3), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(observed, (test_scope(), "generation-1".into()));
        let mut outputs = Vec::new();
        while let Some(event) = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
        {
            assert_eq!(event.generation_id, "generation-1");
            outputs.push(event);
        }
        assert_eq!(outputs.len(), 7);
        assert!(matches!(
            outputs[1].payload,
            Some(proto::talker_output::Payload::Sentence(_))
        ));
        shutdown.cancel();
    }

    #[tokio::test]
    async fn missing_or_mismatched_generation_on_any_payload_never_enters_the_shared_queue() {
        for mismatch in (1..=7).flat_map(|seq| [(seq, "wrong-generation"), (seq, "")]) {
            let expected_outputs = mismatch.0 as usize - 1;
            let (endpoint, _received, shutdown) = start_handshake_talker(Some(mismatch)).await;
            let lifecycle = LifecycleSupervisor::new();
            let connection = lifecycle.register_connection("talker");
            let client = TalkerClient::with_reconnect_policy(
                endpoint,
                lifecycle.spawner(),
                connection.clone(),
                fast_policy(),
                test_scope(),
            );
            let (tx, mut rx) = mpsc::channel(4);
            client.dispatch(start_for(), tx).await;
            let mut count = 0;
            while tokio::time::timeout(Duration::from_secs(3), rx.recv())
                .await
                .unwrap()
                .is_some()
            {
                count += 1;
            }
            assert_eq!(count, expected_outputs);
            assert_eq!(connection.readiness(), Readiness::Degraded);
            shutdown.cancel();
        }
    }
}
