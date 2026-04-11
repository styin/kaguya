//! Talker gRPC Client — Bidi Converse stream
//! Gateway = client, Talker = server
//! Barge-in is inline on the same stream (not a separate Prepare RPC)

use std::sync::Arc;
use tokio::sync::{mpsc, RwLock, Mutex};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tracing::{debug, error, info, warn};

use crate::proto;
use crate::proto::talker_service_client::TalkerServiceClient;

#[derive(Clone)]
pub struct TalkerClient {
    inner: Arc<RwLock<Option<TalkerServiceClient<Channel>>>>,
    endpoint: String,
    /// Sender for the active bidi stream (None if no stream is open)
    stream_tx: Arc<Mutex<Option<mpsc::Sender<proto::TalkerInput>>>>,
}

impl TalkerClient {
    pub fn new(endpoint: String) -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
            endpoint,
            stream_tx: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn connect(&self) -> anyhow::Result<()> {
        let channel = Channel::from_shared(self.endpoint.clone())?
            .connect()
            .await?;
        *self.inner.write().await = Some(TalkerServiceClient::new(channel));
        info!(addr = %self.endpoint, "connected to Talker");
        Ok(())
    }

    pub async fn try_connect(&self) {
        if let Err(e) = self.connect().await {
            warn!("Talker not ready: {e} (will retry)");
        }
    }

    /// Start a bidi Converse stream and dispatch a context.
    /// Returns CancellationToken for the stream lifetime.
    pub fn dispatch(
        &self,
        ctx: proto::TalkerContext,
        output_tx: mpsc::Sender<proto::TalkerOutput>,
    ) -> CancellationToken {
        let token = CancellationToken::new();
        let child = token.child_token();
        let inner = Arc::clone(&self.inner);
        let stream_tx_arc = Arc::clone(&self.stream_tx);
        let endpoint = self.endpoint.clone();

        tokio::spawn(async move {
            // Get or reconnect client
            let mut guard = inner.write().await;
            if guard.is_none() {
                match Channel::from_shared(endpoint) {
                    Ok(ch) => match ch.connect().await {
                        Ok(channel) => {
                            *guard = Some(TalkerServiceClient::new(channel));
                        }
                        Err(e) => { error!("reconnect failed: {e}"); return; }
                    },
                    Err(e) => { error!("bad endpoint: {e}"); return; }
                }
            }
            let mut client = guard.clone().unwrap();
            drop(guard);

            // Create bidi stream channels
            let (tx, rx) = mpsc::channel::<proto::TalkerInput>(64);
            let outbound = ReceiverStream::new(rx);

            // Send initial context
            if tx.send(proto::TalkerInput {
                payload: Some(proto::talker_input::Payload::Start(ctx)),
            }).await.is_err() {
                return;
            }

            // Store tx for barge-in
            *stream_tx_arc.lock().await = Some(tx);

            // Open bidi stream
            let mut inbound = match client.converse(outbound).await {
                Ok(resp) => resp.into_inner(),
                Err(e) => {
                    error!("Converse failed: {e}");
                    *stream_tx_arc.lock().await = None;
                    return;
                }
            };

            // Receive loop
            loop {
                tokio::select! {
                    _ = child.cancelled() => {
                        debug!("Talker stream cancelled externally");
                        break;
                    }
                    result = inbound.message() => {
                        match result {
                            Ok(Some(output)) => {
                                if output_tx.send(output).await.is_err() { break; }
                            }
                            Ok(None) => break,
                            Err(e) => { error!("stream error: {e}"); break; }
                        }
                    }
                }
            }

            *stream_tx_arc.lock().await = None;
        });

        token
    }

    /// Send inline barge-in on the active bidi stream.
    /// Returns BargeInAck via the output_tx channel (handled in main loop).
    pub async fn barge_in(&self, conversation_id: &str) {
        let guard = self.stream_tx.lock().await;
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(proto::TalkerInput {
                payload: Some(proto::talker_input::Payload::BargeIn(
                    proto::BargeInSignal {
                        conversation_id: conversation_id.into(),
                    }
                )),
            }).await;
            debug!("→ BargeIn (inline)");
        } else {
            debug!("barge-in: no active stream (Talker idle)");
        }
    }

    // PrefillCache 和 UpdatePersona 保持 unary 不变
    pub async fn prefill_cache(&self, conversation_id: &str, ctx: proto::TalkerContext) {
        let Some(mut client) = self.inner.read().await.clone() else { return };
        if let Err(e) = client.prefill_cache(proto::PrefillRequest {
            conversation_id: conversation_id.into(),
            context: Some(ctx),
        }).await {
            warn!("PrefillCache failed: {e}");
        }
    }

    pub async fn update_persona(&self, config: proto::PersonaConfig) {
        let guard = self.inner.read().await;
        let Some(mut client) = guard.clone() else {
            warn!("cannot UpdatePersona: Talker not connected");
            return;
        };
        drop(guard);
        if let Err(e) = client.update_persona(config).await {
            error!("UpdatePersona failed: {e}");
        }
    }
}