use std::convert::Infallible;
use std::net::SocketAddr;

use async_stream::stream;
use axum::{
    extract::{Path, Query, State},
    response::{
        sse::{Event, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

use crate::app::SupervisorApp;

#[derive(Debug, Deserialize)]
struct LogsQuery {
    since: Option<u64>,
}

pub async fn serve(app: SupervisorApp, addr: SocketAddr) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "Kaguya Supervisor listening");
    axum::serve(listener, router(app)).await?;
    Ok(())
}

pub fn router(app: SupervisorApp) -> Router {
    Router::new()
        .route("/api/app/status", get(app_status))
        .route("/api/app/start", post(app_start))
        .route("/api/app/shutdown", post(app_shutdown))
        .route("/api/process/status", get(process_status))
        .route("/api/process/:name/start", post(process_start))
        .route("/api/process/:name/stop", post(process_stop))
        .route("/api/process/:name/restart", post(process_restart))
        .route("/api/logs", get(logs_since))
        .route("/api/logs/stream", get(logs_stream))
        .with_state(app)
}

async fn app_status(State(app): State<SupervisorApp>) -> impl IntoResponse {
    Json(app.status().await)
}

async fn app_start(State(app): State<SupervisorApp>) -> impl IntoResponse {
    Json(action_result(app.start_app().await))
}

async fn app_shutdown(State(app): State<SupervisorApp>) -> impl IntoResponse {
    Json(action_result(app.shutdown_app().await))
}

async fn process_status(State(app): State<SupervisorApp>) -> impl IntoResponse {
    Json(app.process_status().await)
}

async fn process_start(
    State(app): State<SupervisorApp>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    Json(action_result(app.start_process(&name).await))
}

async fn process_stop(
    State(app): State<SupervisorApp>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    Json(action_result(app.stop_process(&name).await))
}

async fn process_restart(
    State(app): State<SupervisorApp>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    Json(action_result(app.restart_process(&name).await))
}

async fn logs_since(
    State(app): State<SupervisorApp>,
    Query(query): Query<LogsQuery>,
) -> impl IntoResponse {
    Json(app.logs().since(query.since.unwrap_or(0)))
}

async fn logs_stream(
    State(app): State<SupervisorApp>,
) -> Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>> {
    let logs = app.logs();
    let backlog = logs.since(0);
    let mut rx = logs.subscribe();

    let stream = stream! {
        for entry in backlog {
            if let Ok(data) = serde_json::to_string(&entry) {
                yield Ok(Event::default().data(data));
            }
        }

        loop {
            match rx.recv().await {
                Ok(entry) => {
                    if let Ok(data) = serde_json::to_string(&entry) {
                        yield Ok(Event::default().data(data));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Sse::new(stream)
}

#[derive(serde::Serialize)]
struct ActionResult {
    ok: bool,
    error: Option<String>,
}

fn action_result(result: anyhow::Result<()>) -> ActionResult {
    match result {
        Ok(()) => ActionResult {
            ok: true,
            error: None,
        },
        Err(error) => ActionResult {
            ok: false,
            error: Some(error.to_string()),
        },
    }
}
