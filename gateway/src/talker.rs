//! Talker gRPC Client — Encapsulates all 4 RPCs to the Talker:
//!
//! ProcessPrompt: Unary-Stream - Dispatches with context, receives streaming output. Returns CancellationToken for barge-in.
//! Prepare: Unary-Unary - Interrupts current generation, returns spoken/unspoken split for smooth turn-taking.
//! PrefillCache: Unary-Unary - Speculative prefill of Talker's cache based on user input and conversation context.
//! UpdatePersona: Unary-Unary - Pushes updated persona configuration (soul/identity/memory) to Talker.

use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;
use tonic::transport::Channel;
use tracing::{debug, error, info, warn};

use crate::proto;
use crate::proto::talker_service_client::TalkerServiceClient;

#[derive(Clone)]
pub struct TalkerClient {
    inner: Arc<RwLock<Option<TalkerServiceClient<Channel>>>>,
    endpoint: String,
}

impl TalkerClient {
    pub fn new(endpoint: String) -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
            endpoint,
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
        match self.connect().await {
            Ok(()) => {}
            Err(e) => warn!("Talker not ready: {e} (will retry on first RPC)"),
        }
    }

    async fn client(&self) -> Option<TalkerServiceClient<Channel>> {
        self.inner.read().await.clone()
    }

    /// Attempt to get the client, or reconnect if not connected. Returns None if still unavailable after reconnect attempt.
    async fn client_or_reconnect(&self) -> Option<TalkerServiceClient<Channel>> {
        if let Some(c) = self.client().await {
            return Some(c);
        }
        self.try_connect().await;
        self.client().await
    }

    /// PREPARE - Barge-in
    pub async fn prepare(&self, conversation_id: &str) -> proto::PrepareAck {
        let Some(mut client) = self.client_or_reconnect().await else {
            return proto::PrepareAck::default();
        };
        debug!("→ PREPARE");
        match client.prepare(proto::PrepareSignal {
            conversation_id: conversation_id.into(),
        }).await {
            Ok(resp) => resp.into_inner(),
            Err(e) => {
                error!("Prepare failed: {e}");
                proto::PrepareAck::default()
            }
        }
    }

    /// ProcessPrompt Dispatch
    pub fn dispatch(
        &self,
        ctx: proto::TalkerContext,
        output_tx: mpsc::Sender<proto::TalkerOutput>,
    ) -> CancellationToken {
        let token = CancellationToken::new();
        let child = token.child_token();
        let inner = Arc::clone(&self.inner);
        let endpoint = self.endpoint.clone();

        tokio::spawn(async move {
            // Attempt to get client, reconnect if necessary, finally log and exit if still unavailable
            let mut guard = inner.write().await;
            if guard.is_none() {
                match Channel::from_shared(endpoint).and_then(|c| Ok(c)) {
                    Ok(ch) => match ch.connect().await {
                        Ok(channel) => { *guard = Some(TalkerServiceClient::new(channel)); }
                        Err(e) => { error!("reconnect failed: {e}"); return; }
                    },
                    Err(e) => { error!("bad endpoint: {e}"); return; }
                }
            }
            let mut client = guard.clone().unwrap();
            drop(guard);

            debug!(input = %ctx.user_input, "→ ProcessPrompt");

            let mut stream = match client.process_prompt(ctx).await {
                Ok(resp) => resp.into_inner(),
                Err(e) => {
                    error!("ProcessPrompt failed: {e}");
                    return;
                }
            };

            loop {
                tokio::select! {
                    _ = child.cancelled() => {
                        debug!("Talker stream cancelled (barge-in)");
                        break;
                    }
                    result = stream.message() => {
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
        });

        token
    }

    /// Speculative prefill
    pub async fn prefill_cache(&self, conversation_id: &str, ctx: proto::TalkerContext) {
        let Some(mut client) = self.client().await else { return };
        debug!("→ PrefillCache");
        if let Err(e) = client.prefill_cache(proto::PrefillRequest {
            conversation_id: conversation_id.into(),
            context: Some(ctx),
        }).await {
            warn!("PrefillCache failed (non-fatal): {e}");
        }
    }

    /// Update persona configuration
    pub async fn update_persona(&self, config: proto::PersonaConfig) {
        let Some(mut client) = self.client_or_reconnect().await else {
            warn!("cannot UpdatePersona: Talker not connected");
            return;
        };
        info!(
            soul_len = config.soul_md.len(),
            identity_len = config.identity_md.len(),
            memory_len = config.memory_md.len(),
            "→ UpdatePersona"
        );
        if let Err(e) = client.update_persona(config).await {
            error!("UpdatePersona failed: {e}");
        }
    }
}