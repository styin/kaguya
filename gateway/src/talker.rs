//! Talker gRPC Client — bidi Converse stream.
//! Barge-in is inline on the same stream (BargeInSignal → BargeInAck).

use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
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

    pub async fn try_connect(&self) {
        match Channel::from_shared(self.endpoint.clone()) {
            Ok(ch) => match ch.connect().await {
                Ok(channel) => {
                    *self.inner.write().await = Some(TalkerServiceClient::new(channel));
                    info!(addr = %self.endpoint, "connected to Talker");
                }
                Err(e) => warn!("Talker not ready: {e}"),
            },
            Err(e) => warn!("bad talker endpoint: {e}"),
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

        // Create the bidi channel and register the sender BEFORE spawning.
        // Capacity 64 ⇒ try_send for the start payload never blocks.
        let (tx, rx) = mpsc::channel::<proto::TalkerInput>(64);
        let _ = tx.try_send(proto::TalkerInput {
            payload: Some(proto::talker_input::Payload::Start(ctx)),
        });
        *stream_tx_arc.lock().await = Some(tx);

        tokio::spawn(async move {
            // Ensure client connected
            let mut guard = inner.write().await;
            if guard.is_none() {
                if let Ok(ch) = Channel::from_shared(endpoint) {
                    if let Ok(channel) = ch.connect().await {
                        *guard = Some(TalkerServiceClient::new(channel));
                    } else {
                        error!("Talker reconnect failed");
                        *stream_tx_arc.lock().await = None;
                        return;
                    }
                } else {
                    error!("bad Talker endpoint");
                    *stream_tx_arc.lock().await = None;
                    return;
                }
            }
            let mut client = guard.clone().unwrap();
            drop(guard);

            let outbound = ReceiverStream::new(rx);
            let mut inbound = match client.converse(outbound).await {
                Ok(resp) => resp.into_inner(),
                Err(e) => {
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
                            Err(e) => { error!("Talker stream error: {e}"); break; }
                        }
                    }
                }
            }

            *stream_tx_arc.lock().await = None;
        });

        token
    }

    /// Send inline barge-in on the active Converse stream.
    pub async fn barge_in(&self, conversation_id: &str) {
        let guard = self.stream_tx.lock().await;
        if let Some(tx) = guard.as_ref() {
            let msg = proto::TalkerInput {
                payload: Some(proto::talker_input::Payload::BargeIn(
                    proto::BargeInSignal { conversation_id: conversation_id.into() }
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
        let Some(mut client) = self.inner.read().await.clone() else { return };
        debug!("→ PrefillCache");
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
        info!(soul_len = config.soul_md.len(), identity_len = config.identity_md.len(),
              memory_len = config.memory_md.len(), "→ UpdatePersona");
        if let Err(e) = client.update_persona(config).await {
            error!("UpdatePersona failed: {e}");
        }
    }
}
