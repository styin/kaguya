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
}

impl TalkerClient {
    pub fn new(endpoint: String, tasks: TaskSpawner, connection: ManagedConnectionHandle) -> Self {
        Self::with_reconnect_policy(endpoint, tasks, connection, ReconnectPolicy::default())
    }

    pub fn with_reconnect_policy(
        endpoint: String,
        tasks: TaskSpawner,
        connection: ManagedConnectionHandle,
        reconnect: ReconnectPolicy,
    ) -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
            endpoint,
            stream_tx: Arc::new(Mutex::new(None)),
            tasks,
            connection,
            reconnect,
        }
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
    /// policy. Returns `true` on success (readiness → Ready), `false` if all
    /// attempts are exhausted (readiness → Degraded).
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
    /// Returns a [`CancellationToken`] that cancels the stream when dropped.
    /// The stream sender is registered *before* the task spawns so that a
    /// [`barge_in`](Self::barge_in) call racing immediately after this returns
    /// finds the live sender.
    pub async fn dispatch(
        &self,
        ctx: proto::TalkerContext,
        output_tx: mpsc::Sender<proto::TalkerOutput>,
    ) -> CancellationToken {
        let token = CancellationToken::new();
        let child = token.child_token();
        let inner = Arc::clone(&self.inner);
        let stream_tx_arc = Arc::clone(&self.stream_tx);
        let endpoint = self.endpoint.clone();
        let connection = self.connection.clone();
        let reconnect = self.reconnect;

        // Create the bidi channel and register the sender BEFORE spawning.
        // Capacity 64 ⇒ try_send for the start payload never blocks.
        let (tx, rx) = mpsc::channel::<proto::TalkerInput>(64);
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
            let mut inbound = match client.converse(outbound).await {
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

            loop {
                tokio::select! {
                    _ = child.cancelled() => {
                        debug!("Talker dispatch cancelled externally");
                        break;
                    }
                    result = inbound.message() => {
                        match result {
                            Ok(Some(output)) => {
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
                    connection.set_readiness(Readiness::Ready);
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
    pub async fn barge_in(&self, conversation_id: &str) {
        let guard = self.stream_tx.lock().await;
        if let Some(tx) = guard.as_ref() {
            let msg = proto::TalkerInput {
                payload: Some(proto::talker_input::Payload::BargeIn(
                    proto::BargeInSignal {
                        conversation_id: conversation_id.into(),
                    },
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
    pub async fn prefill_cache(&self, conversation_id: &str, ctx: proto::TalkerContext) {
        let Some(mut client) = self.inner.read().await.clone() else {
            return;
        };
        debug!("→ PrefillCache");
        if let Err(e) = client
            .prefill_cache(proto::PrefillRequest {
                conversation_id: conversation_id.into(),
                context: Some(ctx),
            })
            .await
        {
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
    ) {
        use crate::probe::{sleep_probe_interval, wait_for_grpc_endpoint};

        loop {
            tokio::select! {
                biased;
                _ = shutdown.cancelled() => return,
                _ = wait_for_grpc_endpoint(
                    "talker",
                    &self.endpoint,
                    expected_runtime,
                    |readiness| self.set_readiness(readiness),
                ) => {}
            }

            loop {
                if shutdown.is_cancelled() {
                    return;
                }
                if self.try_connect().await {
                    let persona_snapshot = persona.read().await.clone();
                    self.update_persona(persona_snapshot).await;
                    break;
                }
                if expected_runtime {
                    self.set_readiness(Readiness::Starting);
                }
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => return,
                    _ = sleep_probe_interval() => {}
                }
            }

            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => return,
                    _ = sleep_probe_interval() => {}
                }
                if self.readiness() == Readiness::Degraded {
                    info!("Talker connection lost, reconnecting");
                    break;
                }
            }
        }
    }

    /// Send an UpdatePersona RPC with the latest soul, identity, and memory.
    pub async fn update_persona(&self, config: proto::PersonaConfig) {
        let guard = self.inner.read().await;
        let Some(mut client) = guard.clone() else {
            if self.connection.readiness() != Readiness::Starting {
                self.connection.set_readiness(Readiness::Degraded);
            }
            warn!("cannot UpdatePersona: Talker not connected");
            return;
        };
        drop(guard);
        info!(
            soul_len = config.soul_md.len(),
            identity_len = config.identity_md.len(),
            memory_len = config.memory_md.len(),
            "→ UpdatePersona"
        );
        if let Err(e) = client.update_persona(config).await {
            self.connection.set_readiness(Readiness::Degraded);
            error!("UpdatePersona failed: {e}");
        } else {
            self.connection.set_readiness(Readiness::Ready);
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
    async fn try_connect_marks_connection_degraded_after_policy_exhaustion() {
        let lifecycle = LifecycleSupervisor::new();
        let connection = lifecycle.register_connection("talker");
        let talker = TalkerClient::with_reconnect_policy(
            "not a valid uri".into(),
            lifecycle.spawner(),
            connection.clone(),
            fast_policy(),
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
            "http://127.0.0.1:1".into(),
            lifecycle.spawner(),
            connection.clone(),
            fast_policy(),
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
            _req: tonic::Request<proto::PersonaConfig>,
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
        );

        let persona = Arc::new(RwLock::new(proto::PersonaConfig::default()));
        let shutdown = CancellationToken::new();

        let t = talker.clone();
        let p = persona.clone();
        let s = shutdown.clone();
        tokio::spawn(async move {
            t.run_recovery_loop(p, true, s).await;
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
}
