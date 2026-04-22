//! Phase 1 Endpoint Service — WebSocket server for dev-GUI
//! ---
//! Phase 2 targets OpenPod protocol integration

use std::sync::Arc;
use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, State},
    response::IntoResponse,
    routing::get,
    Router,
};
use tokio::sync::mpsc;
use tracing::info;
use crate::types::*;

pub struct EndpointState {
    pub control_tx: mpsc::Sender<ControlSignal>,
    pub p1_tx: mpsc::Sender<InputEvent>,
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
    while let Some(Ok(msg)) = socket.recv().await {
        match msg {
            Message::Text(json) => {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&json) {
                    match parsed.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(c) = parsed.get("content").and_then(|c| c.as_str()) {
                                let _ = state.p1_tx.send(InputEvent::TextCommand {
                                    text: c.to_string(),
                                }).await;
                            }
                        }
                        Some("control") => {
                            if let Some("stop") = parsed.get("command").and_then(|c| c.as_str()) {
                                let _ = state.control_tx.send(ControlSignal::Stop).await;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    info!("dev-GUI disconnected");
}