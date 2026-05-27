//! Talker gRPC Client — bidi Converse stream.
//! Barge-in is inline on the same stream (BargeInSignal → BargeInAck).

use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tracing::{debug, error, info, warn};

use crate::lifecycle::{ManagedConnectionHandle, Readiness, ReconnectPolicy, TaskSpawner};
use crate::proto;
use crate::proto::talker_service_client::TalkerServiceClient;

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

    /// Open a bidi Converse stream, send context, receive output.
    /// Stores stream sender for inline barge-in.
    ///
    /// The channel + sender are created and registered before spawning the
    /// task, so a `barge_in()` call racing in immediately after this returns
    /// finds the live sender instead of silently no-op'ing.
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

    #[tokio::test]
    async fn try_connect_marks_connection_degraded_after_policy_exhaustion() {
        let lifecycle = LifecycleSupervisor::new();
        let connection = lifecycle.register_connection("talker");
        let talker = TalkerClient::with_reconnect_policy(
            "not a valid uri".into(),
            lifecycle.spawner(),
            connection.clone(),
            ReconnectPolicy::bounded(
                1,
                Duration::from_millis(1),
                Duration::from_millis(1),
                Duration::from_millis(1),
            ),
        );

        talker.try_connect().await;

        assert_eq!(connection.readiness(), Readiness::Degraded);
    }
}
