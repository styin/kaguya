use std::convert::Infallible;
use std::net::SocketAddr;

use async_stream::stream;
use axum::{
    extract::{Path, Query, State},
    response::{
        sse::{Event, Sse},
        IntoResponse,
    },
    routing::{delete, get, post},
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
    serve_on(app, listener).await
}

pub async fn serve_on(app: SupervisorApp, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
    let addr = listener.local_addr()?;
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
        .route("/api/sandbox/status", get(sandbox_status))
        .route("/api/sandbox/acquire", post(sandbox_acquire))
        .route("/api/sandbox/:handle/execute", post(sandbox_execute))
        .route("/api/sandbox/:handle", delete(sandbox_release))
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

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SandboxStatus {
    enabled: bool,
    backend: String,
}

async fn sandbox_status(State(app): State<SupervisorApp>) -> Json<SandboxStatus> {
    Json(SandboxStatus {
        enabled: app.sandbox_enabled(),
        backend: app.sandbox_backend().to_string(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxAcquireRequest {
    session_id: String,
}

#[derive(serde::Serialize)]
struct SandboxAcquireResponse {
    handle: Option<String>,
    error: Option<String>,
}

async fn sandbox_acquire(
    State(app): State<SupervisorApp>,
    Json(request): Json<SandboxAcquireRequest>,
) -> Json<SandboxAcquireResponse> {
    match app.acquire_sandbox(&request.session_id).await {
        Ok(handle) => Json(SandboxAcquireResponse {
            handle: Some(handle),
            error: None,
        }),
        Err(error) => Json(SandboxAcquireResponse {
            handle: None,
            error: Some(error.to_string()),
        }),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SandboxExecuteRequest {
    args_json: String,
}

#[derive(serde::Serialize)]
struct SandboxExecuteResponse {
    content: String,
}

async fn sandbox_execute(
    State(app): State<SupervisorApp>,
    Path(handle): Path<String>,
    Json(request): Json<SandboxExecuteRequest>,
) -> Json<SandboxExecuteResponse> {
    Json(SandboxExecuteResponse {
        content: app.execute_in_sandbox(&handle, &request.args_json).await,
    })
}

async fn sandbox_release(
    State(app): State<SupervisorApp>,
    Path(handle): Path<String>,
) -> Json<ActionResult> {
    Json(action_result(app.release_sandbox(&handle).await))
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::{ResolvedRuntimeConfig, RuntimeConfig, SandboxConfig};

    fn test_app() -> SupervisorApp {
        SupervisorApp::new(ResolvedRuntimeConfig {
            config: RuntimeConfig {
                profile: Some("test".into()),
                supervisor_addr: "127.0.0.1:0".into(),
                sandbox: SandboxConfig::default(),
                processes: BTreeMap::new(),
            },
            base_dir: ".".into(),
        })
    }

    #[tokio::test]
    async fn sandbox_control_plane_acquires_executes_and_releases_handle() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router(test_app())).await.unwrap();
        });
        let client = reqwest::Client::new();
        let base = format!("http://{addr}");

        let status: serde_json::Value = client
            .get(format!("{base}/api/sandbox/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status["enabled"], true);

        let acquired: serde_json::Value = client
            .post(format!("{base}/api/sandbox/acquire"))
            .json(&serde_json::json!({"sessionId": "http-contract"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let handle = acquired["handle"].as_str().unwrap();

        let executed: serde_json::Value = client
            .post(format!("{base}/api/sandbox/{handle}/execute"))
            .json(&serde_json::json!({
                "argsJson": r#"{"language":"python","code":"print('control-plane')"}"#
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let content: serde_json::Value =
            serde_json::from_str(executed["content"].as_str().unwrap()).unwrap();
        assert_eq!(content["exit_code"], 0);
        assert!(content["stdout"]
            .as_str()
            .unwrap()
            .contains("control-plane"));

        let released: serde_json::Value = client
            .delete(format!("{base}/api/sandbox/{handle}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(released["ok"], true);
        server.abort();
    }
}
