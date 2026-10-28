//! Gateway event-loop orchestration.
//!
//! Receives assembled components and event channels from the application.
//! P0 control remains separate from the input queues and handler/action path.

use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::core::input_stream::InputReceivers;
use crate::core::narration::NarrationFilter;
use crate::core::types::{ControlSignal, InputEvent};
use crate::lifecycle::{LifecycleSupervisor, ShutdownReason};
use crate::proto;
use crate::telemetry::TelemetryClient;

use super::{handlers, PipelineComponents, SessionState};

pub async fn run(
    pipeline: &PipelineComponents<'_>,
    mut session: SessionState,
    mut narration: NarrationFilter,
    mut control_rx: mpsc::Receiver<ControlSignal>,
    mut input_rx: InputReceivers,
    mut talker_output_rx: mpsc::Receiver<proto::TalkerOutput>,
    lifecycle: &mut LifecycleSupervisor,
    telemetry: &TelemetryClient,
    silence_enabled: bool,
) -> anyhow::Result<()> {
    let talker = pipeline.talker;
    let history = pipeline.history;
    let tools = pipeline.tools;
    let reasoner = pipeline.reasoner;
    let rag = pipeline.rag;
    let (expected_instance_id, expected_session_id) =
        crate::instance::validate_session_ref(Some(&session.scope))?;
    let expected_instance_id = expected_instance_id.to_owned();
    let expected_session_id = expected_session_id.to_owned();
    anyhow::ensure!(
        history.matches_session(&session.scope),
        "pipeline history instance/session context does not match"
    );

    loop {
        tokio::select! {
            biased;

            // ── P0: Control (inline — bypasses input queues and handlers) ──
            Some(ctrl) = control_rx.recv() => {
                match ctrl {
                    ControlSignal::Stop => {
                        info!("P0: STOP");
                        session.cancel_active_gen();
                        session.cancel_active_silence();
                        reasoner.cancel_all().await;
                        talker.barge_in().await;
                    }
                    ControlSignal::Shutdown => {
                        info!("P0: SHUTDOWN");
                        session.cancel_active_gen();
                        session.cancel_active_silence();
                        reasoner.cancel_all().await;
                        lifecycle.shutdown(ShutdownReason::ControlShutdown).await;
                        break;
                    }
                }
            }

            // ── Talker Output ──
            Some(out) = talker_output_rx.recv() => {
                let generation_id = &out.generation_id;
                debug!(%generation_id, "processing Talker generation event");
                let actions = match out.payload {
                    Some(proto::talker_output::Payload::ResponseStarted(_)) => {
                        handlers::handle_response_started(&mut session, generation_id)
                    }
                    Some(proto::talker_output::Payload::Sentence(se)) => {
                        debug!(text = %se.text, "→ [SENTENCE]");
                        handlers::handle_sentence(&mut session, generation_id, &se.text)
                    }
                    Some(proto::talker_output::Payload::Emotion(em)) => {
                        handlers::handle_emotion(generation_id, &em.emotion)
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
                            &mut session, generation_id, rc.was_interrupted, last_user,
                        )
                    }
                    None => continue,
                };
                pipeline.executor(&mut session).execute_all(actions).await;
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
                let (retrieval, recent, tool_defs, tasks, rag_elapsed_ms) = if ready {
                    let rag_started = std::time::Instant::now();
                    let retrieval = rag.retrieve(&text).await;
                    let rag_elapsed_ms = rag_started.elapsed().as_millis() as u64;
                    (
                        retrieval,
                        history.recent().await,
                        tools.definitions(),
                        reasoner.active_tasks().await,
                        Some(rag_elapsed_ms),
                    )
                } else {
                    (vec![], vec![], vec![], vec![], None)
                };
                let rag_result_count = retrieval.len();
                let rag_top_score = retrieval
                    .iter()
                    .map(|result| result.score)
                    .fold(None, |best: Option<f32>, score| {
                        Some(best.map_or(score, |current| current.max(score)))
                    });
                let mut rag_source_counts = std::collections::BTreeMap::<String, usize>::new();
                for result in &retrieval {
                    *rag_source_counts.entry(result.source.clone()).or_insert(0) += 1;
                }

                let actions = handlers::handle_user_intent(
                    &mut session, &text, is_voice, ready,
                    retrieval, recent, tool_defs, &tasks,
                );
                if let Some(duration_ms) = rag_elapsed_ms {
                    telemetry.emit(
                        "rag.retrieve.completed",
                        Some(expected_session_id.clone()),
                        Some(session.last_user_generation_id.clone()),
                        None,
                        serde_json::json!({
                            "duration_ms": duration_ms,
                            "fused_hits": rag_result_count,
                            "top_score": rag_top_score,
                            "source_counts": rag_source_counts,
                            "query_chars": text.chars().count(),
                        }),
                    );
                }
                pipeline.executor(&mut session).execute_all(actions).await;
            }

            // ── P2: ASR States ──
            Some(event) = input_rx.p2.recv() => {
                match event {
                    InputEvent::VadSpeechStart => {
                        debug!("P2: vad_speech_start → BARGE-IN");
                        let actions = handlers::handle_vad_speech_start(&mut session);
                        pipeline.executor(&mut session).execute_all(actions).await;
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
                    InputEvent::ToolResult { instance_id, session_id, request_id, tool_name, content } => {
                        if instance_id != expected_instance_id || session_id != expected_session_id {
                            warn!(%instance_id, %session_id, %request_id, "dropping tool callback with mismatched originating instance/session correlation");
                            continue;
                        }
                        info!(id = %request_id, tool = %tool_name, "P3: tool result");
                        let recent = history.recent().await;
                        let tool_defs = tools.definitions();
                        let tasks = reasoner.active_tasks().await;
                        let ready = talker.is_ready();
                        handlers::handle_tool_result(
                            &mut session, &request_id, &tool_name, &content,
                            ready, recent, tool_defs, &tasks,
                        )
                    }
                    InputEvent::ReasonerStep { instance_id, session_id, task_id, description } => {
                        if instance_id != expected_instance_id || session_id != expected_session_id {
                            warn!(%instance_id, %session_id, "dropping Reasoner step with mismatched originating instance/session correlation");
                            continue;
                        }
                        let should_narrate = narration.should_narrate(&description);
                        // Skip the async history fetch when the handler will
                        // short-circuit anyway (narration filtered or already
                        // generating). Avoids unnecessary RwLock contention on
                        // the high-frequency reasoner step path.
                        if !should_narrate || session.is_generating() {
                            continue;
                        }
                        let recent = history.recent().await;
                        let ready = talker.is_ready();
                        handlers::handle_reasoner_step(
                            &mut session, &task_id, &description,
                            should_narrate, ready, recent,
                        )
                    }
                    InputEvent::ReasonerCompleted { instance_id, session_id, task_id, summary } => {
                        if instance_id != expected_instance_id || session_id != expected_session_id {
                            warn!(%instance_id, %session_id, %task_id, "dropping Reasoner completion with mismatched originating instance/session correlation");
                            continue;
                        }
                        info!(task_id = %task_id, "P3: reasoner done");
                        let recent = history.recent().await;
                        let tool_defs = tools.definitions();
                        let tasks = reasoner.active_tasks().await;
                        let ready = talker.is_ready();
                        handlers::handle_reasoner_completed(
                            &mut session, &task_id, &summary,
                            ready, recent, tool_defs, &tasks,
                        )
                    }
                    InputEvent::ReasonerError { instance_id, session_id, task_id, message, code } => {
                        if instance_id != expected_instance_id || session_id != expected_session_id {
                            warn!(%instance_id, %session_id, %task_id, "dropping Reasoner error with mismatched originating instance/session correlation");
                            continue;
                        }
                        warn!(task_id = %task_id, err = %message, "P3: reasoner error");
                        let recent = history.recent().await;
                        let tool_defs = tools.definitions();
                        let tasks = reasoner.active_tasks().await;
                        handlers::handle_reasoner_failed(
                            &mut session, proto::ReasonerFailure { task_id, message, code },
                            talker.is_ready(), recent, tool_defs, &tasks,
                        )
                    }
                    _ => continue,
                };
                if !actions.is_empty() {
                    pipeline.executor(&mut session).execute_all(actions).await;
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
                        &mut session, duration, silence_enabled,
                        ready, recent, tool_defs,
                    );
                    if !actions.is_empty() {
                        pipeline.executor(&mut session).execute_all(actions).await;
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
    Ok(())
}
