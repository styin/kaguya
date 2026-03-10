//! Phase 1 开发端点 — WebSocket 连接 dev-GUI/TUI。
//!
//! Phase 2 替换为 OpenPod protobuf 协议。

use std::sync::Arc;
use tracing::info; 
use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, State},
    response::IntoResponse,
    routing::get,
    Router,
};
use tokio::sync::mpsc;

use crate::listener::ListenerBridge;
use crate::types::*;

pub struct EndpointState {
    pub control_tx: mpsc::Sender<ControlSignal>,
    pub p1_tx: mpsc::Sender<InputEvent>,
    pub listener: Arc<ListenerBridge>,
    pub audio_out_rx: tokio::sync::Mutex<mpsc::Receiver<bytes::Bytes>>,
    pub metadata_rx: tokio::sync::Mutex<mpsc::Receiver<MetadataEvent>>,
}

pub fn router(state: Arc<EndpointState>) -> Router {
    Router::new()
        .route("/ws", get(ws_upgrade))
        .route("/health", get(|| async { "OK" }))
        .with_state(state)
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<EndpointState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(mut socket: WebSocket, state: Arc<EndpointState>) {
    info!("dev-GUI connected");
    // TODO: 完整的双向流处理
    // 入：Binary → listener.send_audio()
    //     Text JSON {type:"text", content:...} → p1_tx
    //     Text JSON {type:"control", command:"stop"} → control_tx
    // 出：audio_out_rx → Binary 帧
    //     metadata_rx → Text JSON 帧

    while let Some(Ok(msg)) = socket.recv().await {
        match msg {
            Message::Text(json) => {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json) {
                    match parsed.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(content) = parsed.get("content").and_then(|c| c.as_str()) {
                                let _ = state
                                    .p1_tx
                                    .send(InputEvent::TextCommand {
                                        text: content.to_string(),
                                    })
                                    .await;
                            }
                        }
                        Some("control") => {
                            if let Some("stop") =
                                parsed.get("command").and_then(|c| c.as_str())
                            {
                                let _ = state.control_tx.send(ControlSignal::Stop).await;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Message::Binary(_audio) => {
                state
                    .listener
                    .on_final("(audio not yet processed)".into(), 0.0)
                    .await;
                // TODO: 转发音频给 Listener gRPC
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    info!("dev-GUI disconnected");
}