//! Phase 1 Endpoint Service — WebSocket server for dev console
//! ---
//! Handles a single connected browser client at a time (§2.4).
//! Phase 2 targets OpenPod protocol integration.

use std::sync::Arc;
use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, State},
    response::IntoResponse,
    routing::get,
    Router,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use crate::types::*;

pub struct EndpointState {
    pub control_tx: mpsc::Sender<ControlSignal>,
    pub p1_tx: mpsc::Sender<InputEvent>,
    pub audio_out_rx: tokio::sync::Mutex<mpsc::Receiver<bytes::Bytes>>,
    pub metadata_rx: tokio::sync::Mutex<mpsc::Receiver<MetadataEvent>>,
    pub active_client: std::sync::Mutex<Option<CancellationToken>>,
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
    // G1: single-client — cancel previous connection before proceeding
    let token = CancellationToken::new();
    {
        let mut active = state.active_client.lock().unwrap();
        if let Some(old) = active.take() {
            old.cancel();
        }
        *active = Some(token.clone());
    }

    info!("dev console connected");

    // Lock output receivers — released when this connection ends (cancel or close),
    // allowing the next client to acquire them.
    let mut metadata_rx = state.metadata_rx.lock().await;
    let mut audio_rx = state.audio_out_rx.lock().await;

    loop {
        tokio::select! {
            biased;

            _ = token.cancelled() => {
                info!("dev console superseded by new client");
                break;
            }

            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(json))) => {
                        handle_text_message(&json, &state).await;
                    }
                    Some(Ok(Message::Binary(_data))) => {
                        // G4 (v0.2): forward audio to Listener
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }

            // G3: metadata egress — sentences, emotions, response lifecycle
            Some(meta) = metadata_rx.recv() => {
                let json = serde_json::to_string(&meta).unwrap_or_default();
                if socket.send(Message::Text(json)).await.is_err() {
                    warn!("dev console send failed");
                    break;
                }
            }

            // G2: audio egress — TTS PCM output
            Some(audio) = audio_rx.recv() => {
                if socket.send(Message::Binary(audio.to_vec())).await.is_err() {
                    warn!("dev console audio send failed");
                    break;
                }
            }
        }
    }

    info!("dev console disconnected");
}

async fn handle_text_message(json: &str, state: &EndpointState) {
    let parsed = match serde_json::from_str::<serde_json::Value>(json) {
        Ok(v) => v,
        Err(_) => return,
    };
    match parsed.get("type").and_then(|t| t.as_str()) {
        Some("text") => {
            if let Some(c) = parsed.get("content").and_then(|c| c.as_str()) {
                let _ = state.p1_tx.send(InputEvent::TextCommand {
                    text: c.to_string(),
                }).await;
            }
        }
        Some("control") => match parsed.get("command").and_then(|c| c.as_str()) {
            Some("stop") => {
                let _ = state.control_tx.send(ControlSignal::Stop).await;
            }
            Some("shutdown") => {
                let _ = state.control_tx.send(ControlSignal::Shutdown).await;
            }
            _ => {}
        },
        _ => {}
    }
}