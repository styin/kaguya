//! Phase 1 Endpoint Service — WebSocket server for dev console
//! ---
//! Handles a single connected browser client at a time.
//! Phase 2 targets OpenPod protocol integration.

use std::borrow::Cow;
use std::sync::Arc;

use axum::{
    extract::{
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::clients::audio_sink::ListenerAudioSink;
use crate::core::types::*;
use crate::lifecycle::{LifecycleSnapshot, LifecycleSupervisor};
use kaguya_protocol::session_scope::SessionScope;

const CLOSE_SUPERSEDED: u16 = 4001;

pub struct EndpointState {
    pub control_tx: mpsc::Sender<ControlSignal>,
    pub p1_tx: mpsc::Sender<InputEvent>,
    pub metadata_tx: tokio::sync::broadcast::Sender<MetadataEvent>,
    pub active_client: std::sync::Mutex<Option<CancellationToken>>,
    pub listener_audio: ListenerAudioSink,
    pub runtime_status: RuntimeStatusState,
}

#[derive(Clone)]
pub struct RuntimeStatusState {
    pub lifecycle: LifecycleSupervisor,
    pub scope: SessionScope,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeStatusResponse {
    pub lifecycle: LifecycleSnapshot,
    pub instance_id: String,
    pub session_id: String,
}

pub fn router(state: Arc<EndpointState>) -> Router {
    Router::new()
        .route("/ws", get(ws_upgrade))
        .route("/health", get(|| async { "OK" }))
        .route("/runtime/status", get(runtime_status))
        .with_state(state)
}

async fn runtime_status(State(state): State<Arc<EndpointState>>) -> Json<RuntimeStatusResponse> {
    Json(runtime_status_response(&state.runtime_status))
}

fn runtime_status_response(state: &RuntimeStatusState) -> RuntimeStatusResponse {
    RuntimeStatusResponse {
        lifecycle: state.lifecycle.snapshot(),
        instance_id: state.scope.instance_id().to_string(),
        session_id: state.scope.session_id().to_string(),
    }
}

fn session_init(scope: SessionScope) -> serde_json::Value {
    serde_json::json!({
        "event_type": "session_init",
        "data": {
            "instance_id": scope.instance_id().to_string(),
            "session_id": scope.session_id().to_string(),
        }
    })
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

    // Subscribe before the bootstrap write so events produced during it are retained.
    let mut metadata_rx = state.metadata_tx.subscribe();
    let bootstrap = session_init(state.runtime_status.scope);
    if socket
        .send(Message::Text(bootstrap.to_string()))
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            biased;

            _ = token.cancelled() => {
                info!("dev console superseded by new client");
                let _ = socket
                    .send(Message::Close(Some(CloseFrame {
                        code: CLOSE_SUPERSEDED,
                        reason: Cow::from("superseded by another dev console client"),
                    })))
                    .await;
                break;
            }

            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(json))) => {
                        handle_text_message(&json, &state).await;
                    }
                    Some(Ok(Message::Binary(data))) => {
                        // G4: forward raw audio bytes to Listener via the raw TCP socket
                        state.listener_audio.send(bytes::Bytes::from(data)).await;
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }

            // G3: metadata egress — sentences, emotions, response lifecycle
            result = metadata_rx.recv() => {
                let meta = match result {
                    Ok(meta) => meta,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "dev console fell behind the live event stream");
                        let _ = socket.send(Message::Close(Some(CloseFrame {
                            code: 1013,
                            reason: Cow::from("live event stream lagged; reconnect"),
                        }))).await;
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                let json = serde_json::to_string(&meta).unwrap_or_default();
                if socket.send(Message::Text(json)).await.is_err() {
                    warn!("dev console send failed");
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
                let _ = state
                    .p1_tx
                    .send(InputEvent::TextCommand {
                        text: c.to_string(),
                    })
                    .await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::Readiness;

    #[test]
    fn runtime_status_response_includes_lifecycle_snapshot() {
        let lifecycle = LifecycleSupervisor::new();
        let talker = lifecycle.register_connection("talker");
        talker.set_readiness(Readiness::Ready);

        let response = runtime_status_response(&RuntimeStatusState {
            lifecycle,
            scope: test_scope(),
        });

        assert_eq!(response.lifecycle.connections.len(), 1);
        assert_eq!(response.session_id, "22222222-2222-4222-8222-222222222222");
        assert_eq!(response.lifecycle.connections[0].name, "talker");
        assert_eq!(
            response.lifecycle.connections[0].readiness,
            Readiness::Ready
        );
    }

    fn test_scope() -> SessionScope {
        SessionScope::parse(
            "11111111-1111-4111-8111-111111111111",
            "22222222-2222-4222-8222-222222222222",
        )
        .unwrap()
    }

    #[test]
    fn session_bootstrap_has_one_canonical_session_reference() {
        let bootstrap = session_init(test_scope());
        assert_eq!(
            bootstrap["data"]["instance_id"],
            "11111111-1111-4111-8111-111111111111"
        );
        assert_eq!(
            bootstrap["data"]["session_id"],
            "22222222-2222-4222-8222-222222222222"
        );
        assert_eq!(bootstrap["data"].as_object().unwrap().len(), 2);
    }
}
