//! Kaguya Gateway 入口 — 事件循环。
//!
//! 这个 loop 就是 Spec 第 3/5/6/7/8/9/10 节描述的全部行为。
//! biased select! 保证 P0 > Talker > P1 > P2 > P3 > P4 > P5。

use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use kaguya_gateway::config::GatewayConfig;
use kaguya_gateway::context;
use kaguya_gateway::endpoint;
use kaguya_gateway::history::History;
use kaguya_gateway::input_stream;
use kaguya_gateway::listener::ListenerBridge;
use kaguya_gateway::memory::Memory;
use kaguya_gateway::narration::NarrationFilter;
use kaguya_gateway::output::OutputManager;
use kaguya_gateway::persona::Persona;
use kaguya_gateway::reasoner::ReasonerManager;
use kaguya_gateway::silence::SilenceTimers;
use kaguya_gateway::talker::TalkerClient;
use kaguya_gateway::tools::ToolRegistry;
use kaguya_gateway::types::*;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "kaguya_gateway=debug".into()),
        )
        .init();

    info!("🌙 Kaguya Gateway starting…");

    let config = GatewayConfig::load("config/gateway.toml").unwrap_or_else(|e| {
        warn!("config load failed ({e}), using defaults");
        GatewayConfig::default()
    });

    // ═══════════════════════════════════════════
    //  创建通道
    // ═══════════════════════════════════════════

    // P0 控制信号 — 绕过 Input Stream
    let (control_tx, mut control_rx) = mpsc::channel::<ControlSignal>(64);

    // Input Stream P1-P5
    let (input_tx, mut input_rx) = input_stream::create(256);

    // Talker 输出事件
    let (talker_event_tx, mut talker_event_rx) = mpsc::channel::<TalkerEvent>(256);

    // 输出通道 → endpoint
    let (audio_out_tx, audio_out_rx) = mpsc::channel::<bytes::Bytes>(512);
    let (metadata_out_tx, metadata_out_rx) = mpsc::channel::<MetadataEvent>(256);

    // ═══════════════════════════════════════════
    //  初始化组件
    // ═══════════════════════════════════════════

    let history = History::new(config.history.max_recent_turns);
    let persona = Persona::load(
        config.files.soul_path.clone(),
        config.files.identity_path.clone(),
    )
    .await?;
    let memory = Memory::load(config.files.memory_path.clone()).await?;
    let tools = ToolRegistry::new();
    let reasoner = ReasonerManager::new(config.services.reasoner_addr.clone());
    let silence = SilenceTimers::new(
        config.silence.soft_prompt_secs,
        config.silence.follow_up_secs,
        config.silence.context_shift_secs,
        input_tx.p4.clone(),
    );
    let talker = TalkerClient::new(config.services.talker_addr.clone(), talker_event_tx);
    let output = OutputManager::new(audio_out_tx, metadata_out_tx);
    let mut narration = NarrationFilter::new(5);

    // ── Startup: 发送人格配置给 Talker ──
    talker
        .update_persona(PersonaBundle {
            soul_md: persona.soul().await,
            identity_md: persona.identity().await,
            memory_md: memory.contents().await,
        })
        .await;

    // ── Listener bridge ──
    let listener_bridge = Arc::new(ListenerBridge::new(
        input_tx.p1.clone(),
        input_tx.p2.clone(),
    ));

    // ── 启动 WebSocket endpoint ──
    let endpoint_state = Arc::new(endpoint::EndpointState {
        control_tx: control_tx.clone(),
        p1_tx: input_tx.p1.clone(),
        listener: listener_bridge.clone(),
        audio_out_rx: tokio::sync::Mutex::new(audio_out_rx),
        metadata_rx: tokio::sync::Mutex::new(metadata_out_rx),
    });
    let ws_addr = config.server.ws_addr.clone();
    tokio::spawn(async move {
        let app = endpoint::router(endpoint_state);
        let listener = tokio::net::TcpListener::bind(&ws_addr).await.unwrap();
        info!(addr = %ws_addr, "WebSocket endpoint listening");
        axum::serve(listener, app).await.unwrap();
    });

    // TODO: 启动文件 watcher（notify crate）
    //   SOUL.md / IDENTITY.md 变化 → persona.reload() → talker.update_persona()
    //   MEMORY.md 变化 → memory.reload() → talker.update_persona()

    // TODO: 启动 Listener gRPC bridge
    //   音频帧 → Listener → VAD/STT 事件 → listener_bridge.on_*()

    info!("🌙 Kaguya Gateway ready");

    // ═══════════════════════════════════════════
    //  事件循环状态
    // ═══════════════════════════════════════════

    /// 当前活跃的 Talker 生成（barge-in 时取消）
    let mut active_gen: Option<CancellationToken> = None;
    /// 当前活跃的静默计时器
    let mut active_silence: Option<CancellationToken> = None;

    // ═══════════════════════════════════════════
    //  MAIN EVENT LOOP
    //
    //  biased select! 保证检查顺序 = 优先级顺序。
    //  Spec §3.1: "Within a priority level, events are FIFO.
    //  Across levels, higher priority preempts lower."
    // ═══════════════════════════════════════════

    loop {
        tokio::select! {
            biased;

            // ──────────────────────────────────────
            //  P0: 控制信号 — 绕过 Input Stream
            //  "No event may delay a STOP."
            // ──────────────────────────────────────
            Some(ctrl) = control_rx.recv() => {
                match ctrl {
                    ControlSignal::Stop => {
                        info!("P0: STOP");
                        if let Some(t) = active_gen.take() { t.cancel(); }
                        if let Some(t) = active_silence.take() { t.cancel(); }
                        reasoner.cancel_all().await;
                        output.mute_audio();
                        talker.prepare().await;
                    }
                    ControlSignal::Shutdown => {
                        info!("P0: SHUTDOWN");
                        if let Some(t) = active_gen.take() { t.cancel(); }
                        if let Some(t) = active_silence.take() { t.cancel(); }
                        reasoner.cancel_all().await;
                        break;
                    }
                    ControlSignal::Approval { request_id } => {
                        info!(request_id = %request_id, "P0: APPROVAL");
                        // TODO: 批准待审批操作
                    }
                }
            }

            // ──────────────────────────────────────
            //  Talker 输出（活跃生成的流式返回）
            //  优先级仅次于 P0 — 音频转发需要低延迟
            // ──────────────────────────────────────
            Some(event) = talker_event_rx.recv() => {
                match event {
                    TalkerEvent::TextChunk { content, is_final } => {
                        output.send_text(&content, is_final).await;
                    }
                    TalkerEvent::AudioChunk { data, .. } => {
                        output.send_audio(data).await;
                    }
                    TalkerEvent::EmotionTag { emotion, intensity } => {
                        output.send_emotion(&emotion, intensity).await;
                    }

                    // ── Talker 决定调用工具 ──
                    TalkerEvent::ToolCall { call_id, tool_name, params } => {
                        info!(tool = %tool_name, "Talker → [TOOL]");
                        tools.dispatch(
                            call_id, tool_name, params,
                            input_tx.p3.clone(),
                        );
                    }

                    // ── Talker 决定委派推理 ──
                    TalkerEvent::Delegate { task_description, context } => {
                        info!("Talker → [DELEGATE]");
                        reasoner.start(
                            task_description, context,
                            input_tx.p3.clone(),
                        ).await;
                    }

                    // ── 回复完成 ──
                    //  Spec §5.1 最后一步:
                    //  → history append → memory eval → prefill → silence timer
                    TalkerEvent::ResponseComplete { full_text } => {
                        debug!("Talker: response complete");

                        // 1. 追加到对话历史
                        history.append_assistant(&full_text).await;

                        // 2. 回合后记忆评估
                        if let Some(user_input) = history.last_user_input().await {
                            memory.evaluate_and_update(&user_input, &full_text).await;
                        }

                        // 3. 如果记忆变了，推送 UpdatePersona
                        if memory.take_dirty().await {
                            talker.update_persona(PersonaBundle {
                                soul_md: persona.soul().await,
                                identity_md: persona.identity().await,
                                memory_md: memory.contents().await,
                            }).await;
                        }

                        // 4. 投机预填充
                        let tasks = reasoner.active_tasks().await;
                        let prefill = context::for_prefill(
                            &history, &memory, &tools, &tasks,
                        ).await;
                        talker.prefill_cache(prefill).await;

                        // 5. 启动静默计时器
                        if let Some(t) = active_silence.take() { t.cancel(); }
                        active_silence = Some(silence.start());

                        // 6. 恢复音频转发
                        output.unmute_audio();

                        active_gen = None;
                    }

                    TalkerEvent::Error { message } => {
                        error!("Talker error: {message}");
                        active_gen = None;
                    }
                }
            }

            // ──────────────────────────────────────
            //  P1: 完整用户意图
            //  "Trigger context package assembly + Talker dispatch.
            //   Highest normal priority."
            // ──────────────────────────────────────
            Some(event) = input_rx.p1.recv() => {
                let text = match event {
                    InputEvent::FinalTranscript { text, .. }
                    | InputEvent::TextCommand { text } => text,
                    _ => continue,
                };
                info!(text = %text, "P1: user intent");

                // 取消静默计时器
                if let Some(t) = active_silence.take() { t.cancel(); }

                // 追加用户输入到历史
                history.append_user(&text).await;

                // 组装 context package
                let tasks = reasoner.active_tasks().await;
                let ctx = context::assemble(
                    &text, &history, &memory, &tools, &tasks, &[],
                ).await;

                // Barge-in: 取消当前生成
                if let Some(t) = active_gen.take() { t.cancel(); }

                // 恢复音频、分发给 Talker
                output.unmute_audio();
                active_gen = Some(talker.dispatch(ctx));
            }

            // ──────────────────────────────────────
            //  P2: 部分用户信号
            //  "vad_speech_start triggers PREPARE signal immediately."
            // ──────────────────────────────────────
            Some(event) = input_rx.p2.recv() => {
                match event {
                    InputEvent::VadSpeechStart => {
                        debug!("P2: vad_speech_start → PREPARE");

                        // 1. PREPARE 信号
                        let ack = talker.prepare().await;
                        if ack.was_speaking {
                            // 只保留 spoken 部分，丢弃 unspoken
                            history.append_assistant_partial(&ack.spoken_text).await;
                        }

                        // 2. 停止音频转发
                        output.mute_audio();

                        // 3. 取消静默计时器和活跃生成
                        if let Some(t) = active_silence.take() { t.cancel(); }
                        if let Some(t) = active_gen.take() { t.cancel(); }
                    }
                    InputEvent::PartialTranscript { text } => {
                        debug!(text = %text, "P2: partial → (Phase 2: incremental prefill)");
                        // Phase 2: talker.incremental_prefill(text).await;
                    }
                    InputEvent::VadSpeechEnd => {
                        debug!("P2: vad_speech_end");
                        // 信息性事件，等待 final_transcript
                    }
                    _ => {}
                }
            }

            // ──────────────────────────────────────
            //  P3: 异步结果
            //  "Results from work Kaguya initiated. Never preempt the user."
            //  (biased select 自动保证 P1/P2 优先)
            // ──────────────────────────────────────
            Some(event) = input_rx.p3.recv() => {
                match event {
                    InputEvent::ToolResult { call_id, tool_name, result, success, .. } => {
                        info!(tool = %tool_name, id = %call_id, "P3: tool result");

                        history.append_tool_result(&tool_name, &result.to_string()).await;

                        let tasks = reasoner.active_tasks().await;
                        let ctx = context::with_tool_result(
                            &tool_name, &result, success,
                            &history, &memory, &tools, &tasks,
                        ).await;

                        if let Some(t) = active_gen.take() { t.cancel(); }
                        output.unmute_audio();
                        active_gen = Some(talker.dispatch(ctx));
                    }

                    InputEvent::ReasonerStep { task_id, description, progress } => {
                        debug!(task_id = %task_id, progress, "P3: reasoner step");

                        // 叙事过滤 — 只有有意义的状态转换才叙述
                        if narration.should_narrate(&description, progress) {
                            let ctx = context::for_narration(
                                &description, &history, &memory,
                            ).await;
                            if active_gen.is_none() {
                                active_gen = Some(talker.dispatch(ctx));
                            }
                        }
                    }

                    InputEvent::ReasonerOutput { task_id, result } => {
                        info!(task_id = %task_id, "P3: reasoner complete");
                        history.append_reasoner_result(&task_id, &result).await;

                        let tasks = reasoner.active_tasks().await;
                        let ctx = context::with_reasoner_result(
                            &task_id, &result,
                            &history, &memory, &tools, &tasks,
                        ).await;

                        if let Some(t) = active_gen.take() { t.cancel(); }
                        output.unmute_audio();
                        active_gen = Some(talker.dispatch(ctx));
                    }

                    InputEvent::ReasonerError { task_id, error } => {
                        warn!(task_id = %task_id, error = %error, "P3: reasoner error");
                    }
                    _ => {}
                }
            }

            // ──────────────────────────────────────
            //  P4: 定时事件
            //  "Speculative — moot if user is speaking or tool
            //   result is pending. Only act when queue is quiet."
            // ──────────────────────────────────────
            Some(event) = input_rx.p4.recv() => {
                if let InputEvent::SilenceExceeded { duration } = event {
                    let secs = duration.as_secs();
                    debug!(secs, "P4: silence exceeded");

                    if active_gen.is_none() {
                        let ctx = context::for_silence(
                            duration, &history, &memory, &tools,
                        ).await;
                        active_gen = Some(talker.dispatch(ctx));
                    }
                }
            }

            // ──────────────────────────────────────
            //  P5: 环境
            //  "Background context. Never trigger immediate action."
            // ──────────────────────────────────────
            Some(event) = input_rx.p5.recv() => {
                if let InputEvent::Telemetry { data } = event {
                    debug!(?data, "P5: telemetry");
                    // 记录但不触发动作
                }
            }
        }
    }

    info!("🌙 Kaguya Gateway shutdown complete");
    Ok(())
}