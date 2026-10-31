#![cfg(feature = "dev-console")]

//! Exercise the real Gateway route through Supervisor's status aggregation.

use std::sync::Arc;
use std::time::Duration;

use kaguya_gateway::clients::audio_sink::ListenerAudioSink;
use kaguya_gateway::lifecycle::{LifecycleSupervisor, Readiness};
use kaguya_gateway::services::endpoint::{self, EndpointState, RuntimeStatusState};
use kaguya_protocol::session_scope::SessionScope;
use kaguya_supervisor::app::SupervisorApp;
use kaguya_supervisor::config::{ResolvedRuntimeConfig, RuntimeConfig};
use kaguya_supervisor::server;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};

#[tokio::test]
async fn supervisor_exposes_gateway_snapshot_and_capability_children() {
    let lifecycle = LifecycleSupervisor::new();
    lifecycle
        .register_connection("talker")
        .set_readiness(Readiness::Ready);
    lifecycle
        .register_connection("listener")
        .set_readiness(Readiness::Degraded);
    let scope = SessionScope::parse(
        &uuid::Uuid::new_v4().to_string(),
        &uuid::Uuid::new_v4().to_string(),
    )
    .unwrap();
    let (control_tx, _control_rx) = mpsc::channel(1);
    let (p1_tx, _p1_rx) = mpsc::channel(1);
    let (metadata_tx, _) = broadcast::channel(1);
    let state = Arc::new(EndpointState {
        control_tx,
        p1_tx,
        metadata_tx,
        active_client: std::sync::Mutex::new(None),
        listener_audio: ListenerAudioSink::new(),
        runtime_status: RuntimeStatusState { lifecycle, scope },
    });
    let gateway_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_url = format!("http://{}", gateway_listener.local_addr().unwrap());
    let gateway_task = tokio::spawn(async move {
        axum::serve(gateway_listener, endpoint::router(state))
            .await
            .unwrap();
    });

    // Reuse the live health endpoint to mark both fixture processes available;
    // capability readiness comes from the real Gateway lifecycle snapshot.
    let config: RuntimeConfig = toml::from_str(&format!(
        r#"
        [sandbox]
        enabled = false
        [processes.gateway]
        launch = "external"
        health_url = "{gateway_url}/health"
        [processes.gateway.endpoints]
        websocket = "{gateway_url}"
        [processes.voice_stack]
        launch = "external"
        health_url = "{gateway_url}/health"
        provides = ["talker", "listener"]
    "#
    ))
    .unwrap();
    let app = SupervisorApp::new(
        ResolvedRuntimeConfig {
            config,
            base_dir: ".".into(),
        },
        scope,
    )
    .unwrap();
    let supervisor_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let supervisor_url = format!("http://{}", supervisor_listener.local_addr().unwrap());
    let supervisor_task = tokio::spawn(async move {
        server::serve_on(app, supervisor_listener).await.unwrap();
    });

    let status: Value = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap()
        .get(format!("{supervisor_url}/api/app/status"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        status["gateway"]["lifecycle"]["connections"],
        serde_json::json!([
            { "name": "listener", "readiness": "degraded" },
            { "name": "talker", "readiness": "ready" },
        ])
    );
    let voice = status["processes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|process| process["name"] == "voice_stack")
        .unwrap();
    assert_eq!(voice["status"], "running");
    let children = voice["children"].as_array().unwrap();
    assert_eq!(children.len(), 2);
    let talker = children
        .iter()
        .find(|child| child["name"] == "talker")
        .unwrap();
    assert_eq!(talker["status"], "running");
    assert_eq!(talker["readiness"], "ready");
    let listener = children
        .iter()
        .find(|child| child["name"] == "listener")
        .unwrap();
    assert_eq!(listener["readiness"], "degraded");

    supervisor_task.abort();
    gateway_task.abort();
    let _ = tokio::join!(supervisor_task, gateway_task);
}
