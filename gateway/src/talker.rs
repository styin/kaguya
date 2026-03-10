//! Talker gRPC 客户端。
//!
//! 封装与 Talker 的 4 个 RPC：
//!   ProcessPrompt — 发送 context package，接收流式输出
//!   Prepare       — 打断信号
//!   PrefillCache  — 投机预填充
//!   UpdatePersona — 推送人格配置

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::types::*;

/// PREPARE 信号的应答
#[derive(Debug, Clone)]
pub struct PrepareAck {
    pub was_speaking: bool,
    pub spoken_text: String,
    pub unspoken_text: String,
}

pub struct TalkerClient {
    _endpoint: String,
    event_tx: mpsc::Sender<TalkerEvent>,
}

impl TalkerClient {
    pub fn new(endpoint: String, event_tx: mpsc::Sender<TalkerEvent>) -> Self {
        Self {
            _endpoint: endpoint,
            event_tx,
        }
    }

    /// 发送 PREPARE 信号。Talker 停止当前生成（如果在说话）。
    ///
    /// Spec §5.1: "gRPC, fire-and-forget" — 但我们需要 PrepareAck
    /// 来获取已说/未说的文本，所以实际上是 unary RPC。
    pub async fn prepare(&self) -> PrepareAck {
        debug!("→ PREPARE to Talker");
        // TODO: proto::talker::talker_service_client::TalkerServiceClient
        //       ::connect(&self._endpoint).await
        //       .prepare(PrepareSignal { ... }).await
        PrepareAck {
            was_speaking: false,
            spoken_text: String::new(),
            unspoken_text: String::new(),
        }
    }

    /// 发送 context package，启动流式推理。
    /// 在后台任务中处理 Talker 的流式返回，通过 event_tx 回传。
    /// 返回 CancellationToken 供 barge-in 取消。
    pub fn dispatch(&self, ctx: ContextPackage) -> CancellationToken {
        let token = CancellationToken::new();
        let child = token.child_token();
        let tx = self.event_tx.clone();
        let _endpoint = self._endpoint.clone();

        tokio::spawn(async move {
            debug!(input = %ctx.user_input, "→ ProcessPrompt to Talker");

            // TODO: 实际 gRPC 流式调用
            // let mut client = TalkerServiceClient::connect(_endpoint).await?;
            // let mut stream = client.process_prompt(ctx_proto).await?.into_inner();
            // loop {
            //     tokio::select! {
            //         _ = child.cancelled() => break,
            //         msg = stream.message() => match msg { ... }
            //     }
            // }

            // ── Stub: 模拟 Talker 回复 ──
            tokio::select! {
                _ = child.cancelled() => {
                    debug!("Talker generation cancelled (barge-in)");
                    return;
                }
                _ = async {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    let text = format!(
                        "[Kaguya] {}",
                        if ctx.user_input.is_empty() { "(continuation)" }
                        else { &ctx.user_input }
                    );
                    let _ = tx.send(TalkerEvent::TextChunk {
                        content: text.clone(), is_final: true,
                    }).await;
                    let _ = tx.send(TalkerEvent::ResponseComplete {
                        full_text: text,
                    }).await;
                } => {}
            }
        });

        token
    }

    /// 投机预填充（n_predict: 0, cache_prompt: true）
    pub async fn prefill_cache(&self, _ctx: ContextPackage) {
        debug!("→ PrefillCache to Talker");
        // TODO: 实际 gRPC 调用
    }

    /// 推送人格配置
    pub async fn update_persona(&self, bundle: PersonaBundle) {
        info!(
            soul = bundle.soul_md.len(),
            identity = bundle.identity_md.len(),
            memory = bundle.memory_md.len(),
            "→ UpdatePersona to Talker"
        );
        // TODO: 实际 gRPC 调用
    }
}