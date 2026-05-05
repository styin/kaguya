//! Kaguya Gateway — main event loop.
//! Uses RagEngine (not Memory), bidi Listener/Talker clients.

use std::sync::Arc;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use kaguya_gateway::config::GatewayConfig;
use kaguya_gateway::context;
use kaguya_gateway::control::ControlServiceImpl;
use kaguya_gateway::endpoint;
use kaguya_gateway::history::History;
use kaguya_gateway::input_stream;
use kaguya_gateway::listener::ListenerClient;
use kaguya_gateway::narration::NarrationFilter;
use kaguya_gateway::output::OutputManager;
use kaguya_gateway::persona::Persona;
use kaguya_gateway::proto;
use kaguya_gateway::rag::RagEngine;
use kaguya_gateway::reasoner::ReasonerManager;
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

    let config = GatewayConfig::load("../config/gateway.toml").unwrap_or_else(|e| {
        warn!("config load failed ({e}), using defaults");
        GatewayConfig::default()
    });

    // ── Channels ──
    let (control_tx, mut control_rx) = mpsc::channel::<ControlSignal>(64);
    let (input_tx, mut input_rx) = input_stream::create(256);
    let (talker_output_tx, mut talker_output_rx) = mpsc::channel::<proto::TalkerOutput>(256);
    let (audio_out_tx, audio_out_rx) = mpsc::channel::<bytes::Bytes>(512);
    let (metadata_out_tx, metadata_out_rx) = mpsc::channel::<MetadataEvent>(256);

    // ── Components ──
    let conversation_id = Uuid::new_v4().to_string();
    let history = History::new(config.history.max_recent_turns);
    let persona = Persona::load(&config.files.soul_path, &config.files.identity_path).await?;
    let rag = Arc::new(RagEngine::new(
        &config.rag,
        config.files.workspace_root.clone(),
    )?);
    let tools = ToolRegistry::new(config.files.workspace_root.clone());
    let reasoner = ReasonerManager::new(config.clients.reasoner_addr.clone());
    let silence = SilenceTimers::new(
        config.silence.soft_prompt_secs,
        config.silence.follow_up_secs,
        config.silence.context_shift_secs,
        input_tx.p4.clone(),
    );
    let talker = TalkerClient::new(config.clients.talker_addr.clone());
    let output = OutputManager::new(audio_out_tx, metadata_out_tx);
    let mut narration = NarrationFilter::new(5);

    // ── Start RAG embedder background task ──
    if let Some(ref embedder) = rag.embedder {
        let emb = embedder.clone();
        tokio::spawn(async move { emb.run().await });
    }

    // ── gRPC server (RouterControlService only) ──
    let grpc_addr = config.server.grpc_addr.parse()?;
    let control_svc = ControlServiceImpl::new(control_tx.clone());
    tokio::spawn(async move {
        info!(addr = %grpc_addr, "gRPC control server listening");
        if let Err(e) = Server::builder()
            .add_service(proto::router_control_service_server::RouterControlServiceServer::new(control_svc))
            .serve(grpc_addr)
            .await
        {
            error!("gRPC server failed: {e}");
        }
    });

    // ── Connect Listener (Gateway is client) ──
    // FIX #1: start() returns the audio_tx, which we wire into EndpointState
    let listener_client = ListenerClient::new(
        config.clients.listener_grpc_addr.clone(),
        config.clients.listener_audio_addr.clone(),
    );
    let listener_audio_tx = match listener_client
        .start(input_tx.p1.clone(), input_tx.p2.clone())
        .await
    {
        Ok(tx) => {
            info!("Listener connected (gRPC + audio socket)");
            tx
        }
        Err(e) => {
            warn!("Listener not available at startup: {e} (text-only mode)");
            // Dummy channel — audio frames go nowhere, text input still works
            let (tx, _rx) = mpsc::channel(1);
            tx
        }
    };

    // ── Connect Talker ──
    talker.try_connect().await;
    let mut last_memory_md = rag.export_memory_md().await;
    talker.update_persona(proto::PersonaConfig {
        soul_md: persona.soul().await,
        identity_md: persona.identity().await,
        memory_md: last_memory_md.clone(),
    }).await;

    // ── WebSocket endpoint ──
    // FIX #1: listener_audio_tx comes from ListenerClient, not a disconnected channel
    let endpoint_state = Arc::new(endpoint::EndpointState {
        control_tx: control_tx.clone(),
        p1_tx: input_tx.p1.clone(),
        audio_out_rx: tokio::sync::Mutex::new(audio_out_rx),
        metadata_rx: tokio::sync::Mutex::new(metadata_out_rx),
        listener_audio_tx,
    });
    let ws_addr = config.server.ws_addr.clone();
    tokio::spawn(async move {
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

    // ── File watcher (SOUL.md + IDENTITY.md only — memory is in SQLite) ──
    {
        use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher, EventKind};

        let persona_w = persona.clone();
        let talker_w = talker.clone();
        let rag_w = rag.clone();
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
                    watcher.watch(parent, RecursiveMode::NonRecursive)
                        .unwrap_or_else(|e| warn!("watch failed for {:?}: {e}", parent));
                }
            }
        }
        tokio::spawn(async move {
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

                talker_w.update_persona(proto::PersonaConfig {
                    soul_md: persona_w.soul().await,
                    identity_md: persona_w.identity().await,
                    memory_md: rag_w.export_memory_md().await,
                }).await;
                info!("persona pushed to Talker");
            }
        });
    }

    info!("Kaguya Gateway ready");

    // ── Event Loop State ──
    let mut active_gen: Option<CancellationToken> = None;
    let mut active_silence: Option<CancellationToken> = None;
    let mut current_response = String::new();
    let mut last_turn_id = String::new();

    // ══════════════════════════════════════
    //  MAIN EVENT LOOP
    // ══════════════════════════════════════
    loop {
        tokio::select! {
            biased;

            // ── P0: Control ──
            Some(ctrl) = control_rx.recv() => {
                match ctrl {
                    ControlSignal::Stop => {
                        info!("P0: STOP");
                        if let Some(t) = active_gen.take() { t.cancel(); }
                        if let Some(t) = active_silence.take() { t.cancel(); }
                        reasoner.cancel_all().await;
                        output.mute_audio();
                        talker.barge_in(&conversation_id).await;
                    }
                    ControlSignal::Shutdown => {
                        info!("P0: SHUTDOWN");
                        if let Some(t) = active_gen.take() { t.cancel(); }
                        if let Some(t) = active_silence.take() { t.cancel(); }
                        reasoner.cancel_all().await;
                        break;
                    }
                    ControlSignal::Approval { context } => {
                        info!(ctx = %context, "P0: APPROVAL (Phase 2)");
                    }
                }
            }

            // ── Talker Output ──
            Some(out) = talker_output_rx.recv() => {
                match out.payload {
                    Some(proto::talker_output::Payload::ResponseStarted(rs)) => {
                        debug!(turn = %rs.turn_id, "response started");
                        current_response.clear();
                    }
                    Some(proto::talker_output::Payload::Sentence(se)) => {
                        debug!(text = %se.text, "→ [SENTENCE]");
                        current_response.push_str(&se.text);
                        current_response.push(' ');
                        output.send_sentence(&se.text).await;
                    }
                    Some(proto::talker_output::Payload::Emotion(em)) => {
                        output.send_emotion(&em.emotion).await;
                    }
                    Some(proto::talker_output::Payload::ToolRequest(tr)) => {
                        info!(tool = %tr.tool_name, "→ [TOOL]");
                        tools.dispatch(tr.request_id, tr.tool_name, tr.args_json, input_tx.p3.clone());
                    }
                    Some(proto::talker_output::Payload::DelegateRequest(dr)) => {
                        info!(task = %dr.task_id, "→ [DELEGATE]");
                        reasoner.start(dr.task_id, dr.description, input_tx.p3.clone()).await;
                    }
                    Some(proto::talker_output::Payload::BargeInAck(ack)) => {
                        debug!("← BargeInAck");
                        if !ack.spoken_text.is_empty() {
                            history.append_assistant_partial(&ack.spoken_text).await;
                        }
                        output.unmute_audio();
                    }
                    Some(proto::talker_output::Payload::ResponseComplete(rc)) => {
                        debug!(interrupted = rc.was_interrupted, "response complete");

                        if !rc.was_interrupted {
                            let text = current_response.trim().to_string();
                            if !text.is_empty() {
                                history.append_assistant(&text).await;
                            }
                            if let Some(ui) = history.last_user_input().await {
                                rag.evaluate_and_store(&ui, &text, &last_turn_id).await;
                            }

                            // FIX #3: only push UpdatePersona when memory actually changed
                            let new_memory_md = rag.export_memory_md().await;
                            if new_memory_md != last_memory_md {
                                last_memory_md = new_memory_md;
                                talker.update_persona(proto::PersonaConfig {
                                    soul_md: persona.soul().await,
                                    identity_md: persona.identity().await,
                                    memory_md: last_memory_md.clone(),
                                }).await;
                            }

                            let tasks = reasoner.active_tasks().await;
                            let pctx = context::for_prefill(
                                &conversation_id, &history, &last_memory_md, &tools, &tasks,
                            ).await;
                            talker.prefill_cache(&conversation_id, pctx).await;
                        }

                        if let Some(t) = active_silence.take() { t.cancel(); }
                        active_silence = Some(silence.start());
                        output.unmute_audio();
                        active_gen = None;
                        current_response.clear();
                    }
                    None => {}
                }
            }

            // ── P1: User Intent ──
            Some(event) = input_rx.p1.recv() => {
                let text = match event {
                    InputEvent::FinalTranscript { text, .. }
                    | InputEvent::TextCommand { text } => text,
                    _ => continue,
                };
                info!(text = %text, "P1: user intent");
                if let Some(t) = active_silence.take() { t.cancel(); }
                history.append_user(&text).await;

                let turn_id = Uuid::new_v4().to_string();
                last_turn_id = turn_id.clone();
                let tasks = reasoner.active_tasks().await;
                let retrieval = rag.retrieve(&text).await;
                let ctx = context::assemble(
                    &conversation_id, &turn_id, &text,
                    &history, &last_memory_md, retrieval, &tools, &tasks,
                ).await;

                if let Some(t) = active_gen.take() { t.cancel(); }
                output.unmute_audio();
                active_gen = Some(talker.dispatch(ctx, talker_output_tx.clone()).await);
            }

            // ── P2: ASR States ──
            Some(event) = input_rx.p2.recv() => {
                match event {
                    InputEvent::VadSpeechStart => {
                        debug!("P2: vad_speech_start → BARGE-IN (inline)");
                        talker.barge_in(&conversation_id).await;
                        output.mute_audio();
                        if let Some(t) = active_silence.take() { t.cancel(); }
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
                match event {
                    InputEvent::ToolResult { request_id, tool_name, content } => {
                        info!(id = %request_id, tool = %tool_name, "P3: tool result");
                        history.append_tool_result(&tool_name, &content).await;
                        let turn_id = Uuid::new_v4().to_string();
                        let tasks = reasoner.active_tasks().await;
                        let ctx = context::with_tool_result(
                            &conversation_id, &turn_id, &request_id, &content,
                            &history, &last_memory_md, &tools, &tasks,
                        ).await;
                        if let Some(t) = active_gen.take() { t.cancel(); }
                        output.unmute_audio();
                        active_gen = Some(talker.dispatch(ctx, talker_output_tx.clone()).await);
                    }
                    InputEvent::ReasonerStep { task_id: _, description } => {
                        if narration.should_narrate(&description) {
                            let turn_id = Uuid::new_v4().to_string();
                            let ctx = context::for_narration(
                                &conversation_id, &turn_id,
                                &description, &history, &last_memory_md,
                            ).await;
                            if active_gen.is_none() {
                                active_gen = Some(talker.dispatch(ctx, talker_output_tx.clone()).await);
                            }
                        }
                    }
                    InputEvent::ReasonerCompleted { task_id, summary } => {
                        info!(task_id = %task_id, "P3: reasoner done");
                        history.append_tool_result(&task_id, &summary).await;
                        let turn_id = Uuid::new_v4().to_string();
                        let tasks = reasoner.active_tasks().await;
                        let ctx = context::with_reasoner_result(
                            &conversation_id, &turn_id, &task_id, &summary,
                            &history, &last_memory_md, &tools, &tasks,
                        ).await;
                        if let Some(t) = active_gen.take() { t.cancel(); }
                        output.unmute_audio();
                        active_gen = Some(talker.dispatch(ctx, talker_output_tx.clone()).await);
                    }
                    InputEvent::ReasonerError { task_id, message, .. } => {
                        warn!(task_id = %task_id, err = %message, "P3: reasoner error");
                    }
                    _ => {}
                }
            }

            // ── P4: Silence ──
            Some(event) = input_rx.p4.recv() => {
                if let InputEvent::SilenceExceeded { duration } = event {
                    debug!(secs = duration.as_secs(), "P4: silence");
                    if active_gen.is_none() {
                        let turn_id = Uuid::new_v4().to_string();
                        let ctx = context::for_silence(
                            &conversation_id, &turn_id,
                            duration, &history, &last_memory_md, &tools,
                        ).await;
                        active_gen = Some(talker.dispatch(ctx, talker_output_tx.clone()).await);
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

    info!("Kaguya Gateway shutdown");
    Ok(())
}
