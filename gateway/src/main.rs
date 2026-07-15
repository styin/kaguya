//! Kaguya Gateway entry point.
//!
//! Boots the lifecycle supervisor, spawns recovery loops for voice-stack
//! connections (Talker, Listener), and runs the priority-ordered main event
//! loop that routes control signals, ASR events, tool results, and silence
//! timeouts through the Talker's bidi Converse stream.

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tonic::transport::Server;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use kaguya_gateway::audio_sink::ListenerAudioSink;
use kaguya_gateway::capabilities::RagCapability;
use kaguya_gateway::config::GatewayConfig;
use kaguya_gateway::control::ControlServiceImpl;
#[cfg(feature = "dev-console")]
use kaguya_gateway::endpoint;
use kaguya_gateway::history::History;
use kaguya_gateway::input_stream;
use kaguya_gateway::lifecycle::{LifecycleSupervisor, Readiness, ShutdownReason};
use kaguya_gateway::narration::NarrationFilter;
use kaguya_gateway::output::OutputManager;
use kaguya_gateway::persona::Persona;
use kaguya_gateway::pipeline::{handlers, PipelineComponents, TurnState};
use kaguya_gateway::proto;
use kaguya_gateway::rag::RagEngine;
use kaguya_gateway::reasoner::ReasonerManager;
use kaguya_gateway::sandbox::SandboxClient;
use kaguya_gateway::silence::SilenceTimers;
use kaguya_gateway::talker::TalkerClient;
use kaguya_gateway::tools::ToolRegistry;
use kaguya_gateway::types::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "kaguya_gateway=info".into()),
        )
        .init();

    info!("Kaguya Gateway starting");
    let mut lifecycle = LifecycleSupervisor::new();

    let config = GatewayConfig::load("gateway.toml").unwrap_or_else(|e| {
        warn!("config load failed ({e}), using defaults");
        GatewayConfig::default()
    });
    let clients = config.resolved_clients();

    // ── Channels ──
    let (control_tx, mut control_rx) = mpsc::channel::<ControlSignal>(64);
    let (input_tx, mut input_rx) = input_stream::create(256);
    let (talker_output_tx, mut talker_output_rx) = mpsc::channel::<proto::TalkerOutput>(256);
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
    let supervisor_url =
        std::env::var("KAGUYA_SUPERVISOR_URL").unwrap_or_else(|_| config.supervisor.url.clone());
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
    );
    let output = OutputManager::new(audio_out_tx, metadata_out_tx);
    let mut narration = NarrationFilter::new(5);

    // ── Start RAG embedder background task (RagEngine-specific, before trait erasure) ──
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
                kaguya_gateway::listener::run_recovery_loop(
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
    let mut turn = TurnState::new(conversation_id.clone(), last_memory_md);
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

    // ══════════════════════════════════════
    //  MAIN EVENT LOOP
    // ══════════════════════════════════════
    //
    // Priority-ordered biased select. P0 bypasses the pipeline (inline
    // control handling). All other branches route through handlers →
    // executor for testability.
    loop {
        tokio::select! {
            biased;

            // ── P0: Control (inline — bypasses pipeline) ──
            Some(ctrl) = control_rx.recv() => {
                match ctrl {
                    ControlSignal::Stop => {
                        info!("P0: STOP");
                        turn.cancel_active_gen();
                        turn.cancel_active_silence();
                        reasoner.cancel_all().await;
                        output.mute_audio();
                        talker.barge_in(&turn.conversation_id).await;
                    }
                    ControlSignal::Shutdown => {
                        info!("P0: SHUTDOWN");
                        turn.cancel_active_gen();
                        turn.cancel_active_silence();
                        reasoner.cancel_all().await;
                        lifecycle.shutdown(ShutdownReason::ControlShutdown).await;
                        break;
                    }
                    ControlSignal::Approval { context } => {
                        info!(ctx = %context, "P0: APPROVAL (Phase 2)");
                    }
                }
            }

            // ── Talker Output ──
            Some(out) = talker_output_rx.recv() => {
                let actions = match out.payload {
                    Some(proto::talker_output::Payload::ResponseStarted(rs)) => {
                        debug!(turn = %rs.turn_id, "response started");
                        handlers::handle_response_started(&mut turn, &rs.turn_id)
                    }
                    Some(proto::talker_output::Payload::Sentence(se)) => {
                        debug!(text = %se.text, "→ [SENTENCE]");
                        handlers::handle_sentence(&mut turn, &se.text)
                    }
                    Some(proto::talker_output::Payload::Emotion(em)) => {
                        handlers::handle_emotion(&em.emotion)
                    }
                    Some(proto::talker_output::Payload::ToolRequest(tr)) => {
                        info!(tool = %tr.tool_name, "→ [TOOL]");
                        let tool_exists = tools.has(&tr.tool_name);
                        if !tool_exists {
                            warn!(tool = %tr.tool_name, "rejecting unknown tool (likely hallucinated)");
                        }
                        handlers::handle_tool_request(
                            &tr.tool_name, &tr.request_id, &tr.args_json,
                            tool_exists, &tools.name_list(),
                        )
                    }
                    Some(proto::talker_output::Payload::DelegateRequest(dr)) => {
                        info!(task = %dr.task_id, "→ [DELEGATE]");
                        handlers::handle_delegate_request(&dr.task_id, &dr.description)
                    }
                    Some(proto::talker_output::Payload::BargeInAck(ack)) => {
                        debug!("← BargeInAck");
                        handlers::handle_barge_in_ack(&ack.spoken_text)
                    }
                    Some(proto::talker_output::Payload::ResponseComplete(rc)) => {
                        debug!(interrupted = rc.was_interrupted, "response complete");
                        let last_user = history.last_user_input().await;
                        handlers::handle_response_complete(
                            &mut turn, &rc.turn_id, rc.was_interrupted, last_user,
                        )
                    }
                    None => continue,
                };
                pipeline.executor(&mut turn).execute_all(actions).await;
            }

            // ── P1: User Intent ──
            Some(event) = input_rx.p1.recv() => {
                let (text, is_voice) = match event {
                    InputEvent::FinalTranscript { text, .. } => (text, true),
                    InputEvent::TextCommand { text } => (text, false),
                    _ => continue,
                };
                info!(text = %text, "P1: user intent");
                let ready = talker.is_ready();
                if !ready {
                    warn!(
                        readiness = ?talker.readiness(),
                        "P1 user intent: Talker not ready"
                    );
                }

                // Skip expensive async fetches when Talker can't accept a dispatch.
                let (retrieval, recent, tool_defs, tasks) = if ready {
                    (
                        rag.retrieve(&text).await,
                        history.recent().await,
                        tools.definitions(),
                        reasoner.active_tasks().await,
                    )
                } else {
                    (vec![], vec![], vec![], vec![])
                };

                let actions = handlers::handle_user_intent(
                    &mut turn, &text, is_voice, ready,
                    retrieval, recent, tool_defs, &tasks,
                );
                pipeline.executor(&mut turn).execute_all(actions).await;
            }

            // ── P2: ASR States ──
            Some(event) = input_rx.p2.recv() => {
                match event {
                    InputEvent::VadSpeechStart => {
                        debug!("P2: vad_speech_start → BARGE-IN");
                        let actions = handlers::handle_vad_speech_start(&mut turn);
                        pipeline.executor(&mut turn).execute_all(actions).await;
                    }
                    InputEvent::PartialTranscript { text } => {
                        debug!(text = %text, "P2: partial");
                    }
                    InputEvent::VadSpeechEnd { .. } => {
                        debug!("P2: vad_speech_end");
                    }
                    _ => {}
                }
            }

            // ── P3: Tool/Reasoner Results ──
            Some(event) = input_rx.p3.recv() => {
                let actions = match event {
                    InputEvent::ToolResult { request_id, tool_name, content } => {
                        info!(id = %request_id, tool = %tool_name, "P3: tool result");
                        let recent = history.recent().await;
                        let tool_defs = tools.definitions();
                        let tasks = reasoner.active_tasks().await;
                        let ready = talker.is_ready();
                        handlers::handle_tool_result(
                            &mut turn, &request_id, &tool_name, &content,
                            ready, recent, tool_defs, &tasks,
                        )
                    }
                    InputEvent::ReasonerStep { task_id: _, description } => {
                        let should_narrate = narration.should_narrate(&description);
                        // Skip the async history fetch when the handler will
                        // short-circuit anyway (narration filtered or already
                        // generating). Avoids unnecessary RwLock contention on
                        // the high-frequency reasoner step path.
                        if !should_narrate || turn.is_generating() {
                            continue;
                        }
                        let recent = history.recent().await;
                        let ready = talker.is_ready();
                        handlers::handle_reasoner_step(
                            &mut turn, &description,
                            should_narrate, ready, recent,
                        )
                    }
                    InputEvent::ReasonerCompleted { task_id, summary } => {
                        info!(task_id = %task_id, "P3: reasoner done");
                        let recent = history.recent().await;
                        let tool_defs = tools.definitions();
                        let tasks = reasoner.active_tasks().await;
                        let ready = talker.is_ready();
                        handlers::handle_reasoner_completed(
                            &mut turn, &task_id, &summary,
                            ready, recent, tool_defs, &tasks,
                        )
                    }
                    InputEvent::ReasonerError { task_id, message, .. } => {
                        warn!(task_id = %task_id, err = %message, "P3: reasoner error");
                        continue;
                    }
                    _ => continue,
                };
                if !actions.is_empty() {
                    pipeline.executor(&mut turn).execute_all(actions).await;
                }
            }

            // ── P4: Silence ──
            Some(event) = input_rx.p4.recv() => {
                if let InputEvent::SilenceExceeded { duration } = event {
                    debug!(secs = duration.as_secs(), "P4: silence");
                    let recent = history.recent().await;
                    let tool_defs = tools.definitions();
                    let ready = talker.is_ready();
                    let actions = handlers::handle_silence(
                        &mut turn, duration, config.silence.enabled,
                        ready, recent, tool_defs,
                    );
                    if !actions.is_empty() {
                        pipeline.executor(&mut turn).execute_all(actions).await;
                    }
                }
            }

            // ── P5: Telemetry ──
            Some(event) = input_rx.p5.recv() => {
                if let InputEvent::Telemetry { data } = event {
                    debug!(?data, "P5: telemetry");
                }
            }
        }
    }

    // Release only the opaque conversation handle. Provider/global teardown is
    // owned by Supervisor and runs after managed processes stop.
    sandbox.release().await;

    info!("Kaguya Gateway shutdown");
    Ok(())
}
