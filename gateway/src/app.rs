//! Gateway application assembly, startup wiring, and cleanup.
//!
//! Constructs components and supervises local background tasks, then awaits the
//! conversation pipeline. Managed processes and sandbox resources belong to Supervisor.

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tonic::transport::Server;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::audio_sink::ListenerAudioSink;
use crate::capabilities::RagCapability;
use crate::config::GatewayConfig;
use crate::control::ControlServiceImpl;
#[cfg(feature = "dev-console")]
use crate::endpoint;
use crate::history::History;
use crate::input_stream;
use crate::lifecycle::{LifecycleSupervisor, Readiness};
use crate::narration::NarrationFilter;
use crate::output::OutputManager;
use crate::persona::Persona;
use crate::pipeline::{self, PipelineComponents, TurnState};
use crate::proto;
use crate::rag::RagEngine;
use crate::reasoner::ReasonerManager;
use crate::sandbox::SandboxClient;
use crate::silence::SilenceTimers;
use crate::talker::TalkerClient;
use crate::telemetry::TelemetryClient;
use crate::tools::ToolRegistry;
use crate::types::{ControlSignal, MetadataEvent};

pub async fn run() -> anyhow::Result<()> {
    info!("Kaguya Gateway starting");
    let mut lifecycle = LifecycleSupervisor::new();

    let config = GatewayConfig::load("gateway.toml").unwrap_or_else(|e| {
        warn!("config load failed ({e}), using defaults");
        GatewayConfig::default()
    });
    let clients = config.resolved_clients();

    // ── Channels ──
    let (control_tx, control_rx) = mpsc::channel::<ControlSignal>(64);
    let (input_tx, input_rx) = input_stream::create(256);
    let (talker_output_tx, talker_output_rx) = mpsc::channel::<proto::TalkerOutput>(256);
    let (audio_out_tx, _audio_out_rx) = mpsc::channel::<bytes::Bytes>(512);
    let (metadata_out_tx, _metadata_out_rx) = mpsc::channel::<MetadataEvent>(256);

    let signal_tx = control_tx.clone();
    lifecycle.spawn("os_signal_shutdown", async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                info!("OS shutdown signal received");
                let _ = signal_tx.send(ControlSignal::Shutdown).await;
            }
            Err(e) => warn!("failed to listen for OS shutdown signal: {e}"),
        }
    });

    // ── Components ──
    let conversation_id = Uuid::new_v4().to_string();
    let history = History::new(config.history.max_recent_turns);
    let persona = Persona::load(&config.files.soul_path, &config.files.identity_path).await?;
    let rag_engine = RagEngine::new(&config.rag, config.files.workspace_root.clone())?;
    let rag_embedder = rag_engine.embedder.clone();
    let rag: Arc<dyn RagCapability> = Arc::new(rag_engine);
    let task_spawner = lifecycle.spawner();
    let talker_connection = lifecycle.register_connection("talker");
    let listener_connection = lifecycle.register_connection("listener");
    let reasoner_connection = lifecycle.register_connection("reasoner");
    reasoner_connection.set_readiness(Readiness::Stopped);

    // ── Supervisor-owned Sandbox Provider ──
    // Gateway holds only the client; it acquires an opaque handle lazily on the
    // first tool call. Backend policy and resource lifecycle stay in Supervisor.
    let supervisor_url = config.supervisor.resolved_url();
    let telemetry = TelemetryClient::new(&supervisor_url, "gateway");
    let sandbox = match SandboxClient::connect(&supervisor_url).await {
        Ok(client) => {
            info!(
                backend = client.backend().unwrap_or("unknown"),
                "connected to Supervisor Sandbox Provider"
            );
            Arc::new(client)
        }
        Err(error) => {
            warn!(%error, %supervisor_url, "Supervisor sandbox unavailable; tool disabled");
            Arc::new(SandboxClient::disabled())
        }
    };

    let tools = ToolRegistry::new(
        config.files.workspace_root.clone(),
        task_spawner.clone(),
        Some(Arc::clone(&sandbox)),
    );
    let reasoner = ReasonerManager::new(
        clients.reasoner_addr.clone(),
        task_spawner.clone(),
        reasoner_connection,
    );
    let silence = SilenceTimers::new(
        config.silence.soft_prompt_secs,
        config.silence.follow_up_secs,
        config.silence.context_shift_secs,
        input_tx.p4.clone(),
        task_spawner.clone(),
    );
    let talker = TalkerClient::new(
        clients.talker_addr.clone(),
        task_spawner.clone(),
        talker_connection,
    )
    .with_telemetry(telemetry.clone());
    let output = OutputManager::new(audio_out_tx, metadata_out_tx);
    let narration = NarrationFilter::new(5);

    // ── Start optional RAG embedder using the handle retained during construction ──
    if let Some(ref embedder) = rag_embedder {
        let emb = embedder.clone();
        lifecycle.spawn("rag_embedder", async move { emb.run().await });
    }

    // ── gRPC server (RouterControlService only) ──
    let grpc_addr = config.server.grpc_addr.parse()?;
    let control_svc = ControlServiceImpl::new(control_tx.clone());
    lifecycle.spawn("grpc_control_server", async move {
        info!(addr = %grpc_addr, "gRPC control server listening");
        if let Err(e) = Server::builder()
            .add_service(
                proto::router_control_service_server::RouterControlServiceServer::new(control_svc),
            )
            .serve(grpc_addr)
            .await
        {
            error!("gRPC server failed: {e}");
        }
    });

    // ── Voice-stack connections (Talker + Listener recovery loops) ──
    let last_memory_md = rag.export_memory_md().await;
    let listener_audio = ListenerAudioSink::new();
    let listener_enabled = config.listener_enabled();
    if !listener_enabled {
        listener_connection.set_readiness(Readiness::Stopped);
        info!("Listener disabled by runtime profile (text-only mode)");
    }

    let shared_persona: Arc<RwLock<proto::PersonaConfig>> =
        Arc::new(RwLock::new(proto::PersonaConfig {
            soul_md: persona.soul().await,
            identity_md: persona.identity().await,
            memory_md: last_memory_md.clone(),
        }));
    let expected_voice_stack = config.runtime.runtime_expected("voice_stack");
    let shutdown_token = lifecycle.shutdown_token();

    {
        let talker_recovery = talker.clone();
        let persona_recovery = shared_persona.clone();
        let shutdown = shutdown_token.clone();
        lifecycle.spawn("talker_recovery", async move {
            talker_recovery
                .run_recovery_loop(persona_recovery, expected_voice_stack, shutdown)
                .await;
        });
    }

    if listener_enabled {
        let shutdown = shutdown_token.clone();
        lifecycle.spawn("listener_recovery", {
            let grpc_endpoint = clients.listener_grpc_addr.clone();
            let audio_addr = clients.listener_audio_addr.clone();
            let spawner = task_spawner.clone();
            let conn = listener_connection.clone();
            let audio = listener_audio.clone();
            let p1 = input_tx.p1.clone();
            let p2 = input_tx.p2.clone();
            async move {
                crate::listener::run_recovery_loop(
                    grpc_endpoint,
                    audio_addr,
                    spawner,
                    conn,
                    Default::default(),
                    audio,
                    p1,
                    p2,
                    expected_voice_stack,
                    shutdown,
                )
                .await;
            }
        });
    }

    // ── WebSocket endpoint (dev-console feature) ──
    #[cfg(feature = "dev-console")]
    {
        let endpoint_state = Arc::new(endpoint::EndpointState {
            control_tx: control_tx.clone(),
            p1_tx: input_tx.p1.clone(),
            audio_out_rx: tokio::sync::Mutex::new(_audio_out_rx),
            metadata_rx: tokio::sync::Mutex::new(_metadata_out_rx),
            active_client: std::sync::Mutex::new(None),
            listener_audio: listener_audio.clone(),
            runtime_status: endpoint::RuntimeStatusState {
                lifecycle: lifecycle.clone(),
            },
        });
        let ws_addr = config.server.ws_addr.clone();
        lifecycle.spawn("websocket_endpoint", async move {
            let app = endpoint::router(endpoint_state);
            let listener = match tokio::net::TcpListener::bind(&ws_addr).await {
                Ok(l) => l,
                Err(e) => {
                    error!(addr = %ws_addr, "WebSocket bind failed: {e}");
                    return;
                }
            };
            info!(addr = %ws_addr, "WebSocket endpoint listening");
            if let Err(e) = axum::serve(listener, app).await {
                error!(addr = %ws_addr, "WebSocket endpoint failed: {e}");
            }
        });
    }

    // ── File watcher (SOUL.md + IDENTITY.md only — memory is in SQLite) ──
    {
        use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

        let persona_w = persona.clone();
        let talker_w = talker.clone();
        let rag_w = rag.clone();
        let shared_persona_w = shared_persona.clone();
        let soul_path = config.files.soul_path.clone();
        let identity_path = config.files.identity_path.clone();

        let (watch_tx, mut watch_rx) = mpsc::channel::<PathBuf>(16);
        let mut watcher = RecommendedWatcher::new(
            move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                        for path in event.paths {
                            let _ = watch_tx.blocking_send(path);
                        }
                    }
                }
            },
            Config::default(),
        )?;
        for p in [&soul_path, &identity_path] {
            if let Some(parent) = p.parent() {
                if parent.exists() {
                    watcher
                        .watch(parent, RecursiveMode::NonRecursive)
                        .unwrap_or_else(|e| warn!("watch failed for {:?}: {e}", parent));
                }
            }
        }
        lifecycle.spawn("persona_file_watcher", async move {
            let _watcher = watcher;
            while let Some(changed) = watch_rx.recv().await {
                info!(file = ?changed, "config file changed");
                if changed == soul_path {
                    if let Err(e) = persona_w.reload_soul(&soul_path).await {
                        error!("reload SOUL: {e}");
                        continue;
                    }
                } else if changed == identity_path {
                    if let Err(e) = persona_w.reload_identity(&identity_path).await {
                        error!("reload IDENTITY: {e}");
                        continue;
                    }
                } else {
                    continue;
                }

                let new_persona = proto::PersonaConfig {
                    soul_md: persona_w.soul().await,
                    identity_md: persona_w.identity().await,
                    memory_md: rag_w.export_memory_md().await,
                };
                *shared_persona_w.write().await = new_persona.clone();
                talker_w.update_persona(new_persona).await;
                info!("persona pushed to Talker");
            }
        });
    }

    info!("Kaguya Gateway ready");

    // ── Event Loop State ──
    let turn = TurnState::new(conversation_id.clone(), last_memory_md);
    let pipeline = PipelineComponents {
        talker: &talker,
        history: &history,
        output: &output,
        tools: &tools,
        reasoner: &reasoner,
        rag: &rag,
        silence: &silence,
        persona: &persona,
        shared_persona: &shared_persona,
        talker_output_tx: talker_output_tx.clone(),
        p3_tx: input_tx.p3.clone(),
    };

    pipeline::run(
        &pipeline,
        turn,
        narration,
        control_rx,
        input_rx,
        talker_output_rx,
        &mut lifecycle,
        &telemetry,
        config.silence.enabled,
    )
    .await;

    // Release only the opaque conversation handle. Provider/global teardown is
    // owned by Supervisor and runs after managed processes stop.
    sandbox.release().await;

    info!("Kaguya Gateway shutdown");
    Ok(())
}
