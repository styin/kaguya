//! Pipeline handlers — pure decision functions for each event variant.
//!
//! Each handler takes `&mut TurnState` plus pre-fetched data and returns
//! `Vec<PipelineAction>`. The orchestrator in `main.rs` pre-fetches
//! async data (history, tools, retrieval), calls the handler, then
//! passes the returned actions to the executor.

use std::time::Duration;

use uuid::Uuid;

use crate::context;
use crate::proto;
use crate::types::{ActiveTask, DispatchKind};

use super::types::{PipelineAction, TurnState};

/// Reset the response buffer and notify clients. Called on `ResponseStarted`.
pub fn handle_response_started(state: &mut TurnState, turn_id: &str) -> Vec<PipelineAction> {
    state.current_response.clear();
    vec![PipelineAction::SendResponseStarted {
        turn_id: turn_id.to_string(),
    }]
}

/// Accumulate sentence text and forward to output. Called on `Sentence`.
pub fn handle_sentence(state: &mut TurnState, text: &str) -> Vec<PipelineAction> {
    state.current_response.push_str(text);
    state.current_response.push(' ');
    vec![PipelineAction::SendSentence {
        text: text.to_string(),
    }]
}

/// Forward emotion tag to output. Called on `Emotion`.
pub fn handle_emotion(emotion: &str) -> Vec<PipelineAction> {
    vec![PipelineAction::SendEmotion {
        emotion: emotion.to_string(),
    }]
}

/// Dispatch a known tool or reject an unknown one inline. Rejecting
/// appends an error tool result to history instead of round-tripping
/// through P3, which would cause a spurious continuation dispatch.
pub fn handle_tool_request(
    tool_name: &str,
    request_id: &str,
    args_json: &str,
    tool_exists: bool,
    available_tools: &str,
) -> Vec<PipelineAction> {
    if tool_exists {
        vec![PipelineAction::DispatchTool {
            request_id: request_id.to_string(),
            tool_name: tool_name.to_string(),
            args_json: args_json.to_string(),
        }]
    } else {
        let err = serde_json::json!({
            "error": format!(
                "Unknown tool '{}'. Available tools: {}",
                tool_name, available_tools,
            ),
        })
        .to_string();
        vec![PipelineAction::AppendToolResultHistory {
            tool_name: tool_name.to_string(),
            content: err,
        }]
    }
}

/// Start a Reasoner task. Called on `DelegateRequest`.
pub fn handle_delegate_request(task_id: &str, description: &str) -> Vec<PipelineAction> {
    vec![PipelineAction::StartReasoner {
        task_id: task_id.to_string(),
        description: description.to_string(),
    }]
}

/// Record partial spoken text (if any) and unmute output. Called on `BargeInAck`.
pub fn handle_barge_in_ack(spoken_text: &str) -> Vec<PipelineAction> {
    let mut actions = Vec::new();
    if !spoken_text.is_empty() {
        actions.push(PipelineAction::AppendAssistantPartialHistory {
            spoken_text: spoken_text.to_string(),
        });
    }
    actions.push(PipelineAction::UnmuteOutput);
    actions
}

/// Finalize a Talker response. On non-interrupted completions: appends
/// assistant history, conditionally persists a RAG memory (only for
/// `UserIntent` rounds), updates persona, and prefills cache. Always
/// resets turn state and restarts silence timers.
pub fn handle_response_complete(
    state: &mut TurnState,
    turn_id: &str,
    was_interrupted: bool,
    last_user_input: Option<String>,
) -> Vec<PipelineAction> {
    let mut actions = Vec::new();

    if !was_interrupted {
        let text = state.current_response.trim().to_string();
        if !text.is_empty() {
            actions.push(PipelineAction::AppendAssistantHistory { text: text.clone() });

            // Only UserIntent rounds carry a fresh (user, assistant) pair
            // worth persisting. See `DispatchKind::should_persist_memory`.
            let persist = state
                .current_dispatch_kind
                .map(|k| k.should_persist_memory())
                .unwrap_or(false);
            if persist {
                if let Some(ui) = last_user_input {
                    actions.push(PipelineAction::EvaluateAndStoreMemory {
                        user_input: ui,
                        assistant_response: text,
                        turn_id: state.last_turn_id.clone(),
                    });
                }
            }
        }

        actions.push(PipelineAction::UpdatePersonaIfChanged);
        actions.push(PipelineAction::PrefillCache);
    }

    // Reset turn state regardless of interruption.
    state.cancel_active_silence();
    actions.push(PipelineAction::StartSilenceTimers);
    actions.push(PipelineAction::UnmuteOutput);
    state.active_gen = None;
    state.current_dispatch_kind = None;
    state.current_response.clear();

    actions.push(PipelineAction::SendResponseComplete {
        turn_id: turn_id.to_string(),
        was_interrupted,
    });

    actions
}

/// P1: Process a user intent (voice transcript or text command).
/// Cancels silence, appends to history, builds context with RAG
/// retrieval, and dispatches to Talker. No-ops if Talker is not ready.
pub fn handle_user_intent(
    state: &mut TurnState,
    text: &str,
    is_voice: bool,
    talker_ready: bool,
    retrieval: Vec<proto::RetrievalResult>,
    history: Vec<proto::ChatMessage>,
    tools: Vec<proto::ToolDefinition>,
    active_tasks: &[ActiveTask],
) -> Vec<PipelineAction> {
    let mut actions = Vec::new();

    // Voice transcripts are echoed to WS for dev-console display.
    if is_voice {
        actions.push(PipelineAction::SendUserInput {
            text: text.to_string(),
        });
    }

    if !talker_ready {
        return actions;
    }

    state.cancel_active_silence();
    actions.push(PipelineAction::AppendUserHistory {
        text: text.to_string(),
    });

    let turn_id = Uuid::new_v4().to_string();
    state.last_turn_id = turn_id.clone();

    let ctx = context::assemble_from_data(
        &state.conversation_id,
        &turn_id,
        text,
        history,
        &state.last_memory_md,
        retrieval,
        tools,
        active_tasks,
    );

    state.cancel_active_gen();
    actions.push(PipelineAction::UnmuteOutput);
    state.current_dispatch_kind = Some(DispatchKind::UserIntent);
    actions.push(PipelineAction::DispatchTalker {
        context: ctx,
        kind: DispatchKind::UserIntent,
    });

    actions
}

/// P2: Barge-in on speech start — cancel silence, interrupt Talker, mute output.
pub fn handle_vad_speech_start(state: &mut TurnState) -> Vec<PipelineAction> {
    state.cancel_active_silence();
    vec![PipelineAction::BargeIn, PipelineAction::MuteOutput]
}

/// P3: Record a tool result in history and dispatch a continuation to Talker.
pub fn handle_tool_result(
    state: &mut TurnState,
    request_id: &str,
    tool_name: &str,
    content: &str,
    talker_ready: bool,
    history: Vec<proto::ChatMessage>,
    tools: Vec<proto::ToolDefinition>,
    active_tasks: &[ActiveTask],
) -> Vec<PipelineAction> {
    let mut actions = Vec::new();

    actions.push(PipelineAction::AppendToolResultHistory {
        tool_name: tool_name.to_string(),
        content: content.to_string(),
    });

    let turn_id = Uuid::new_v4().to_string();
    let ctx = context::with_tool_result_from_data(
        &state.conversation_id,
        &turn_id,
        request_id,
        content,
        history,
        &state.last_memory_md,
        tools,
        active_tasks,
    );

    state.cancel_active_gen();
    actions.push(PipelineAction::UnmuteOutput);
    state.current_dispatch_kind = Some(DispatchKind::ToolResult);

    if talker_ready {
        actions.push(PipelineAction::DispatchTalker {
            context: ctx,
            kind: DispatchKind::ToolResult,
        });
    } else {
        state.current_dispatch_kind = None;
    }

    actions
}

/// P3: Narrate a reasoner step to the user. Suppressed if the narration
/// filter rejects it or a generation is already active.
pub fn handle_reasoner_step(
    state: &mut TurnState,
    description: &str,
    should_narrate: bool,
    talker_ready: bool,
    history: Vec<proto::ChatMessage>,
) -> Vec<PipelineAction> {
    if !should_narrate {
        return vec![];
    }

    if state.is_generating() {
        return vec![];
    }

    let turn_id = Uuid::new_v4().to_string();
    let ctx = context::for_narration_from_data(
        &state.conversation_id,
        &turn_id,
        description,
        history,
        &state.last_memory_md,
    );

    state.current_dispatch_kind = Some(DispatchKind::ReasonerNarration);
    if talker_ready {
        vec![PipelineAction::DispatchTalker {
            context: ctx,
            kind: DispatchKind::ReasonerNarration,
        }]
    } else {
        state.current_dispatch_kind = None;
        vec![]
    }
}

/// P3: Record a completed reasoner summary and dispatch continuation to Talker.
pub fn handle_reasoner_completed(
    state: &mut TurnState,
    task_id: &str,
    summary: &str,
    talker_ready: bool,
    history: Vec<proto::ChatMessage>,
    tools: Vec<proto::ToolDefinition>,
    active_tasks: &[ActiveTask],
) -> Vec<PipelineAction> {
    let mut actions = Vec::new();

    actions.push(PipelineAction::AppendToolResultHistory {
        tool_name: task_id.to_string(),
        content: summary.to_string(),
    });

    let turn_id = Uuid::new_v4().to_string();
    let ctx = context::with_reasoner_result_from_data(
        &state.conversation_id,
        &turn_id,
        task_id,
        summary,
        history,
        &state.last_memory_md,
        tools,
        active_tasks,
    );

    state.cancel_active_gen();
    actions.push(PipelineAction::UnmuteOutput);
    state.current_dispatch_kind = Some(DispatchKind::ReasonerResult);

    if talker_ready {
        actions.push(PipelineAction::DispatchTalker {
            context: ctx,
            kind: DispatchKind::ReasonerResult,
        });
    } else {
        state.current_dispatch_kind = None;
    }

    actions
}

/// P4: Proactive re-engagement after silence. Gated by config and
/// suppressed when a generation is already active.
pub fn handle_silence(
    state: &mut TurnState,
    duration: Duration,
    silence_enabled: bool,
    talker_ready: bool,
    history: Vec<proto::ChatMessage>,
    tools: Vec<proto::ToolDefinition>,
) -> Vec<PipelineAction> {
    if !silence_enabled {
        return vec![];
    }

    if state.is_generating() {
        return vec![];
    }

    let turn_id = Uuid::new_v4().to_string();
    let ctx = context::for_silence_from_data(
        &state.conversation_id,
        &turn_id,
        duration,
        history,
        &state.last_memory_md,
        tools,
    );

    state.current_dispatch_kind = Some(DispatchKind::Silence);
    if talker_ready {
        vec![PipelineAction::DispatchTalker {
            context: ctx,
            kind: DispatchKind::Silence,
        }]
    } else {
        state.current_dispatch_kind = None;
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_state() -> TurnState {
        TurnState::new("conv-test".into(), "# Memory".into())
    }

    // ── TalkerOutput handlers ──────────────────────────────────────

    #[test]
    fn response_started_clears_response_buffer() {
        let mut state = fresh_state();
        state.current_response = "stale text".into();

        let actions = handle_response_started(&mut state, "turn-1");

        assert!(state.current_response.is_empty());
        assert_eq!(
            actions,
            vec![PipelineAction::SendResponseStarted {
                turn_id: "turn-1".into()
            }]
        );
    }

    #[test]
    fn sentence_accumulates_in_response_buffer() {
        let mut state = fresh_state();

        handle_sentence(&mut state, "Hello");
        let actions = handle_sentence(&mut state, "world");

        assert_eq!(state.current_response, "Hello world ");
        assert_eq!(
            actions,
            vec![PipelineAction::SendSentence {
                text: "world".into()
            }]
        );
    }

    #[test]
    fn emotion_returns_send_emotion() {
        let actions = handle_emotion("happy");
        assert_eq!(
            actions,
            vec![PipelineAction::SendEmotion {
                emotion: "happy".into()
            }]
        );
    }

    #[test]
    fn tool_request_dispatches_known_tool() {
        let actions = handle_tool_request("search", "req-1", "{}", true, "search, calc");
        assert_eq!(
            actions,
            vec![PipelineAction::DispatchTool {
                request_id: "req-1".into(),
                tool_name: "search".into(),
                args_json: "{}".into(),
            }]
        );
    }

    #[test]
    fn tool_request_rejects_unknown_tool() {
        let actions = handle_tool_request("hallucinated", "req-2", "{}", false, "search, calc");
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PipelineAction::AppendToolResultHistory { tool_name, content } => {
                assert_eq!(tool_name, "hallucinated");
                assert!(content.contains("Unknown tool"));
                assert!(content.contains("search, calc"));
            }
            other => panic!("expected AppendToolResultHistory, got {other:?}"),
        }
    }

    #[test]
    fn delegate_request_starts_reasoner() {
        let actions = handle_delegate_request("task-1", "summarize inbox");
        assert_eq!(
            actions,
            vec![PipelineAction::StartReasoner {
                task_id: "task-1".into(),
                description: "summarize inbox".into(),
            }]
        );
    }

    #[test]
    fn barge_in_ack_appends_partial_when_nonempty() {
        let actions = handle_barge_in_ack("I was say");
        assert_eq!(
            actions,
            vec![
                PipelineAction::AppendAssistantPartialHistory {
                    spoken_text: "I was say".into()
                },
                PipelineAction::UnmuteOutput,
            ]
        );
    }

    #[test]
    fn barge_in_ack_skips_partial_when_empty() {
        let actions = handle_barge_in_ack("");
        assert_eq!(actions, vec![PipelineAction::UnmuteOutput]);
    }

    // ── ResponseComplete ───────────────────────────────────────────

    #[test]
    fn response_complete_persists_memory_only_for_user_intent() {
        let mut state = fresh_state();
        state.current_response = "The answer is 42. ".into();
        state.current_dispatch_kind = Some(DispatchKind::UserIntent);
        state.last_turn_id = "turn-abc".into();

        let actions = handle_response_complete(
            &mut state,
            "turn-abc",
            false,
            Some("What is the meaning?".into()),
        );

        assert!(actions.contains(&PipelineAction::AppendAssistantHistory {
            text: "The answer is 42.".into(),
        }));
        assert!(actions
            .iter()
            .any(|a| matches!(a, PipelineAction::EvaluateAndStoreMemory { .. })));
        assert!(actions.contains(&PipelineAction::UpdatePersonaIfChanged));
        assert!(actions.contains(&PipelineAction::PrefillCache));
    }

    #[test]
    fn response_complete_skips_memory_for_tool_result_dispatch() {
        let mut state = fresh_state();
        state.current_response = "Tool says hello. ".into();
        state.current_dispatch_kind = Some(DispatchKind::ToolResult);

        let actions =
            handle_response_complete(&mut state, "turn-xyz", false, Some("old user input".into()));

        assert!(!actions
            .iter()
            .any(|a| matches!(a, PipelineAction::EvaluateAndStoreMemory { .. })));
        // Should still append assistant history and prefill.
        assert!(actions.contains(&PipelineAction::AppendAssistantHistory {
            text: "Tool says hello.".into(),
        }));
    }

    #[test]
    fn response_complete_skips_all_post_turn_when_interrupted() {
        let mut state = fresh_state();
        state.current_response = "partial ".into();
        state.current_dispatch_kind = Some(DispatchKind::UserIntent);

        let actions = handle_response_complete(&mut state, "turn-int", true, None);

        // No assistant history, no memory, no prefill.
        assert!(!actions
            .iter()
            .any(|a| matches!(a, PipelineAction::AppendAssistantHistory { .. })));
        assert!(!actions
            .iter()
            .any(|a| matches!(a, PipelineAction::EvaluateAndStoreMemory { .. })));
        assert!(!actions.contains(&PipelineAction::PrefillCache));

        // Still resets silence and sends complete.
        assert!(actions.contains(&PipelineAction::StartSilenceTimers));
        assert!(actions.contains(&PipelineAction::SendResponseComplete {
            turn_id: "turn-int".into(),
            was_interrupted: true,
        }));
    }

    #[test]
    fn response_complete_resets_turn_state() {
        let mut state = fresh_state();
        state.current_response = "text ".into();
        state.current_dispatch_kind = Some(DispatchKind::UserIntent);
        state.active_gen = Some(tokio_util::sync::CancellationToken::new());

        let _ = handle_response_complete(&mut state, "turn-r", false, None);

        assert!(state.active_gen.is_none());
        assert!(state.current_dispatch_kind.is_none());
        assert!(state.current_response.is_empty());
    }

    // ── P1: User Intent ────────────────────────────────────────────

    #[test]
    fn user_intent_dispatches_when_talker_ready() {
        let mut state = fresh_state();

        let actions = handle_user_intent(
            &mut state,
            "hello world",
            true, // is_voice
            true, // talker_ready
            vec![],
            vec![],
            vec![],
            &[],
        );

        // Should echo voice input.
        assert!(actions.contains(&PipelineAction::SendUserInput {
            text: "hello world".into(),
        }));
        // Should append to history.
        assert!(actions.contains(&PipelineAction::AppendUserHistory {
            text: "hello world".into(),
        }));
        // Should dispatch.
        assert!(actions.iter().any(|a| matches!(
            a,
            PipelineAction::DispatchTalker {
                kind: DispatchKind::UserIntent,
                ..
            }
        )));
        assert_eq!(state.current_dispatch_kind, Some(DispatchKind::UserIntent));
    }

    #[test]
    fn user_intent_skips_dispatch_when_talker_not_ready() {
        let mut state = fresh_state();

        let actions = handle_user_intent(
            &mut state,
            "hello",
            false, // not voice
            false, // talker NOT ready
            vec![],
            vec![],
            vec![],
            &[],
        );

        // No dispatch, no history append.
        assert!(!actions
            .iter()
            .any(|a| matches!(a, PipelineAction::DispatchTalker { .. })));
        assert!(!actions
            .iter()
            .any(|a| matches!(a, PipelineAction::AppendUserHistory { .. })));
        assert!(state.current_dispatch_kind.is_none());
    }

    #[test]
    fn user_intent_text_command_does_not_echo() {
        let mut state = fresh_state();

        let actions = handle_user_intent(
            &mut state,
            "typed input",
            false, // NOT voice → no echo
            true,
            vec![],
            vec![],
            vec![],
            &[],
        );

        assert!(!actions
            .iter()
            .any(|a| matches!(a, PipelineAction::SendUserInput { .. })));
    }

    #[test]
    fn user_intent_cancels_active_silence() {
        let mut state = fresh_state();
        let token = tokio_util::sync::CancellationToken::new();
        let child = token.clone();
        state.active_silence = Some(token);

        let _ = handle_user_intent(&mut state, "hi", false, true, vec![], vec![], vec![], &[]);

        assert!(state.active_silence.is_none());
        assert!(child.is_cancelled());
    }

    // ── P2: VAD ────────────────────────────────────────────────────

    #[test]
    fn vad_speech_start_triggers_barge_in_and_mute() {
        let mut state = fresh_state();

        let actions = handle_vad_speech_start(&mut state);

        assert_eq!(
            actions,
            vec![PipelineAction::BargeIn, PipelineAction::MuteOutput]
        );
    }

    #[test]
    fn vad_speech_start_cancels_silence() {
        let mut state = fresh_state();
        let token = tokio_util::sync::CancellationToken::new();
        let child = token.clone();
        state.active_silence = Some(token);

        let _ = handle_vad_speech_start(&mut state);

        assert!(child.is_cancelled());
    }

    // ── P3: Tool Result ────────────────────────────────────────────

    #[test]
    fn tool_result_dispatches_when_ready() {
        let mut state = fresh_state();

        let actions = handle_tool_result(
            &mut state,
            "req-1",
            "search",
            "result data",
            true, // talker_ready
            vec![],
            vec![],
            &[],
        );

        assert!(actions.contains(&PipelineAction::AppendToolResultHistory {
            tool_name: "search".into(),
            content: "result data".into(),
        }));
        assert!(actions.iter().any(|a| matches!(
            a,
            PipelineAction::DispatchTalker {
                kind: DispatchKind::ToolResult,
                ..
            }
        )));
        assert_eq!(state.current_dispatch_kind, Some(DispatchKind::ToolResult));
    }

    #[test]
    fn tool_result_clears_dispatch_kind_when_not_ready() {
        let mut state = fresh_state();

        let actions = handle_tool_result(
            &mut state,
            "req-1",
            "search",
            "data",
            false, // NOT ready
            vec![],
            vec![],
            &[],
        );

        assert!(!actions
            .iter()
            .any(|a| matches!(a, PipelineAction::DispatchTalker { .. })));
        assert!(state.current_dispatch_kind.is_none());
    }

    // ── P3: Reasoner Step ──────────────────────────────────────────

    #[test]
    fn reasoner_step_dispatches_narration_when_idle() {
        let mut state = fresh_state();

        let actions = handle_reasoner_step(
            &mut state,
            "analyzing data",
            true, // should_narrate
            true, // talker_ready
            vec![],
        );

        assert!(actions.iter().any(|a| matches!(
            a,
            PipelineAction::DispatchTalker {
                kind: DispatchKind::ReasonerNarration,
                ..
            }
        )));
    }

    #[test]
    fn reasoner_step_skipped_when_narration_filtered() {
        let mut state = fresh_state();

        let actions = handle_reasoner_step(
            &mut state,
            "step 2",
            false, // filtered out
            true,
            vec![],
        );

        assert!(actions.is_empty());
    }

    #[test]
    fn reasoner_step_skipped_when_generating() {
        let mut state = fresh_state();
        state.active_gen = Some(tokio_util::sync::CancellationToken::new());

        let actions = handle_reasoner_step(&mut state, "step 3", true, true, vec![]);

        assert!(actions.is_empty());
    }

    // ── P3: Reasoner Completed ─────────────────────────────────────

    #[test]
    fn reasoner_completed_dispatches_when_ready() {
        let mut state = fresh_state();

        let actions = handle_reasoner_completed(
            &mut state,
            "task-1",
            "summary text",
            true,
            vec![],
            vec![],
            &[],
        );

        assert!(actions.contains(&PipelineAction::AppendToolResultHistory {
            tool_name: "task-1".into(),
            content: "summary text".into(),
        }));
        assert!(actions.iter().any(|a| matches!(
            a,
            PipelineAction::DispatchTalker {
                kind: DispatchKind::ReasonerResult,
                ..
            }
        )));
    }

    // ── P4: Silence ────────────────────────────────────────────────

    #[test]
    fn silence_dispatches_when_enabled_and_idle() {
        let mut state = fresh_state();

        let actions = handle_silence(
            &mut state,
            Duration::from_secs(30),
            true, // enabled
            true, // talker_ready
            vec![],
            vec![],
        );

        assert!(actions.iter().any(|a| matches!(
            a,
            PipelineAction::DispatchTalker {
                kind: DispatchKind::Silence,
                ..
            }
        )));
    }

    #[test]
    fn silence_suppressed_when_disabled() {
        let mut state = fresh_state();

        let actions = handle_silence(
            &mut state,
            Duration::from_secs(30),
            false, // disabled
            true,
            vec![],
            vec![],
        );

        assert!(actions.is_empty());
    }

    #[test]
    fn silence_suppressed_when_generating() {
        let mut state = fresh_state();
        state.active_gen = Some(tokio_util::sync::CancellationToken::new());

        let actions = handle_silence(
            &mut state,
            Duration::from_secs(30),
            true,
            true,
            vec![],
            vec![],
        );

        assert!(actions.is_empty());
    }

    #[test]
    fn silence_clears_dispatch_kind_when_not_ready() {
        let mut state = fresh_state();

        let actions = handle_silence(
            &mut state,
            Duration::from_secs(30),
            true,
            false, // NOT ready
            vec![],
            vec![],
        );

        assert!(actions.is_empty());
        assert!(state.current_dispatch_kind.is_none());
    }

    // ── Stress tests: edge cases and state mutation correctness ────

    #[test]
    fn response_complete_empty_response_does_not_persist_memory() {
        // Regression: empty assistant_response should not be written to RAG.
        let mut state = fresh_state();
        state.current_response = "   ".into(); // whitespace only
        state.current_dispatch_kind = Some(DispatchKind::UserIntent);
        state.last_turn_id = "turn-empty".into();

        let actions = handle_response_complete(
            &mut state,
            "turn-empty",
            false,
            Some("user said something".into()),
        );

        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, PipelineAction::AppendAssistantHistory { .. })),
            "should not append empty assistant history"
        );
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, PipelineAction::EvaluateAndStoreMemory { .. })),
            "should not persist empty assistant response to memory"
        );
        // Post-turn persona + prefill still fire.
        assert!(actions.contains(&PipelineAction::UpdatePersonaIfChanged));
        assert!(actions.contains(&PipelineAction::PrefillCache));
    }

    #[test]
    fn response_complete_no_dispatch_kind_skips_memory() {
        let mut state = fresh_state();
        state.current_response = "answer text ".into();
        state.current_dispatch_kind = None; // e.g. stale state

        let actions =
            handle_response_complete(&mut state, "turn-none", false, Some("user question".into()));

        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, PipelineAction::EvaluateAndStoreMemory { .. })),
            "None dispatch kind → persist=false → no memory write"
        );
        // History should still be appended for non-empty text.
        assert!(actions.contains(&PipelineAction::AppendAssistantHistory {
            text: "answer text".into(),
        }));
    }

    #[test]
    fn response_complete_user_intent_no_last_user_input_skips_memory() {
        let mut state = fresh_state();
        state.current_response = "answer ".into();
        state.current_dispatch_kind = Some(DispatchKind::UserIntent);

        let actions = handle_response_complete(
            &mut state, "turn-nui", false, None, // no last_user_input
        );

        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, PipelineAction::EvaluateAndStoreMemory { .. })),
            "persist=true but no user input → no memory write"
        );
    }

    #[test]
    fn response_complete_action_ordering() {
        // Verify the exact action sequence for a non-interrupted UserIntent round.
        let mut state = fresh_state();
        state.current_response = "response text ".into();
        state.current_dispatch_kind = Some(DispatchKind::UserIntent);
        state.last_turn_id = "turn-ord".into();

        let actions =
            handle_response_complete(&mut state, "turn-ord", false, Some("user msg".into()));

        // Expected order:
        //  0: AppendAssistantHistory
        //  1: EvaluateAndStoreMemory
        //  2: UpdatePersonaIfChanged
        //  3: PrefillCache
        //  4: StartSilenceTimers
        //  5: UnmuteOutput
        //  6: SendResponseComplete
        assert!(
            matches!(&actions[0], PipelineAction::AppendAssistantHistory { .. }),
            "action[0] should be AppendAssistantHistory, got {:?}",
            actions[0]
        );
        assert!(
            matches!(&actions[1], PipelineAction::EvaluateAndStoreMemory { .. }),
            "action[1] should be EvaluateAndStoreMemory, got {:?}",
            actions[1]
        );
        assert_eq!(actions[2], PipelineAction::UpdatePersonaIfChanged);
        assert_eq!(actions[3], PipelineAction::PrefillCache);
        assert_eq!(actions[4], PipelineAction::StartSilenceTimers);
        assert_eq!(actions[5], PipelineAction::UnmuteOutput);
        assert!(
            matches!(
                &actions[6],
                PipelineAction::SendResponseComplete {
                    was_interrupted: false,
                    ..
                }
            ),
            "action[6] should be SendResponseComplete"
        );
        assert_eq!(actions.len(), 7);
    }

    #[test]
    fn user_intent_cancels_active_gen() {
        let mut state = fresh_state();
        let token = tokio_util::sync::CancellationToken::new();
        let child = token.clone();
        state.active_gen = Some(token);

        let _ = handle_user_intent(
            &mut state,
            "new question",
            false,
            true,
            vec![],
            vec![],
            vec![],
            &[],
        );

        assert!(state.active_gen.is_none(), "old gen should be cancelled");
        assert!(child.is_cancelled());
    }

    #[test]
    fn user_intent_overwrites_dispatch_kind() {
        let mut state = fresh_state();
        state.current_dispatch_kind = Some(DispatchKind::Silence);

        let _ = handle_user_intent(
            &mut state,
            "hello",
            false,
            true,
            vec![],
            vec![],
            vec![],
            &[],
        );

        assert_eq!(state.current_dispatch_kind, Some(DispatchKind::UserIntent));
    }

    #[test]
    fn tool_result_cancels_active_gen() {
        let mut state = fresh_state();
        let token = tokio_util::sync::CancellationToken::new();
        let child = token.clone();
        state.active_gen = Some(token);

        let _ = handle_tool_result(
            &mut state,
            "req-1",
            "search",
            "data",
            true,
            vec![],
            vec![],
            &[],
        );

        assert!(state.active_gen.is_none());
        assert!(child.is_cancelled());
    }

    #[test]
    fn reasoner_completed_cancels_active_gen() {
        let mut state = fresh_state();
        let token = tokio_util::sync::CancellationToken::new();
        let child = token.clone();
        state.active_gen = Some(token);

        let _ =
            handle_reasoner_completed(&mut state, "task-1", "summary", true, vec![], vec![], &[]);

        assert!(state.active_gen.is_none());
        assert!(child.is_cancelled());
    }

    #[test]
    fn reasoner_completed_clears_dispatch_kind_when_not_ready() {
        let mut state = fresh_state();

        let actions =
            handle_reasoner_completed(&mut state, "task-1", "summary", false, vec![], vec![], &[]);

        assert!(!actions
            .iter()
            .any(|a| matches!(a, PipelineAction::DispatchTalker { .. })));
        assert!(state.current_dispatch_kind.is_none());
    }

    #[test]
    fn reasoner_step_clears_dispatch_kind_when_not_ready() {
        let mut state = fresh_state();

        let actions = handle_reasoner_step(&mut state, "thinking about it", true, false, vec![]);

        assert!(actions.is_empty());
        assert!(state.current_dispatch_kind.is_none());
    }

    #[test]
    fn tool_request_unknown_does_not_dispatch_continuation() {
        // Rejected tools append an error to history but must NOT
        // produce a DispatchTalker — continuation would need the
        // full tool-result context assembly path.
        let actions = handle_tool_request("hallucinated_fn", "req-99", "{}", false, "search, calc");

        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, PipelineAction::DispatchTalker { .. })),
            "rejected tool must not trigger a continuation dispatch"
        );
        assert_eq!(actions.len(), 1, "only AppendToolResultHistory expected");
    }

    #[test]
    fn vad_speech_start_does_not_cancel_active_gen() {
        // VAD sends BargeIn to Talker. The Talker responds with
        // BargeInAck which carries spoken_text. active_gen is NOT
        // cancelled here — the Talker-side stream handles that.
        let mut state = fresh_state();
        let token = tokio_util::sync::CancellationToken::new();
        let child = token.clone();
        state.active_gen = Some(token);

        let _ = handle_vad_speech_start(&mut state);

        assert!(
            state.active_gen.is_some(),
            "active_gen should NOT be cancelled by speech start"
        );
        assert!(!child.is_cancelled());
    }
}
