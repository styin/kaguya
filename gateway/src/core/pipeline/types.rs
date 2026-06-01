//! Core pipeline data types.
//!
//! [`TurnState`] holds mutable per-conversation state across event loop
//! iterations. [`PipelineAction`] enumerates every side effect a handler
//! can request — one variant per I/O call, enabling precise test assertions.

use tokio_util::sync::CancellationToken;

use crate::proto;
use crate::types::DispatchKind;

/// Mutable conversation state shared between the orchestrator, handlers,
/// and executor across event loop iterations.
#[derive(Debug)]
pub struct TurnState {
    /// Cancellation token for the active Talker generation, if any.
    pub active_gen: Option<CancellationToken>,
    /// Cancellation token for the active silence timer cascade, if any.
    pub active_silence: Option<CancellationToken>,
    /// Accumulated sentence text from the current response stream.
    pub current_response: String,
    /// Turn ID of the most recent user-intent dispatch.
    pub last_turn_id: String,
    /// What kind of round triggered the active dispatch.
    /// Read on `ResponseComplete` to gate memory persistence.
    pub current_dispatch_kind: Option<DispatchKind>,
    /// Last exported memory markdown, used to detect changes for persona push.
    pub last_memory_md: String,
    /// Conversation ID for this session.
    pub conversation_id: String,
}

impl TurnState {
    /// Initialize state for a new conversation session.
    pub fn new(conversation_id: String, initial_memory_md: String) -> Self {
        Self {
            active_gen: None,
            active_silence: None,
            current_response: String::new(),
            last_turn_id: String::new(),
            current_dispatch_kind: None,
            last_memory_md: initial_memory_md,
            conversation_id,
        }
    }

    /// Returns `true` if a Talker generation is currently in progress.
    pub fn is_generating(&self) -> bool {
        self.active_gen.is_some()
    }

    /// Cancel the active Talker generation, if any.
    pub fn cancel_active_gen(&mut self) {
        if let Some(t) = self.active_gen.take() {
            t.cancel();
        }
    }

    /// Cancel the active silence timer cascade, if any.
    pub fn cancel_active_silence(&mut self) {
        if let Some(t) = self.active_silence.take() {
            t.cancel();
        }
    }
}

/// A side effect requested by a handler, executed by [`ActionExecutor`].
///
/// One variant per I/O operation. Handlers return `Vec<PipelineAction>`
/// and never call components directly.
#[derive(Debug, PartialEq)]
pub enum PipelineAction {
    // ── Talker ──
    /// Open a Converse bidi stream. Executor stores the returned
    /// cancellation token in `TurnState::active_gen`.
    DispatchTalker {
        context: proto::TalkerContext,
        kind: DispatchKind,
    },
    /// Interrupt the active generation stream.
    BargeIn,

    // ── Output ──
    SendResponseStarted {
        turn_id: String,
    },
    SendSentence {
        text: String,
    },
    SendEmotion {
        emotion: String,
    },
    SendResponseComplete {
        turn_id: String,
        was_interrupted: bool,
    },
    /// Echo voice transcript to WebSocket clients.
    SendUserInput {
        text: String,
    },
    MuteOutput,
    UnmuteOutput,

    // ── History ──
    AppendUserHistory {
        text: String,
    },
    AppendAssistantHistory {
        text: String,
    },
    /// Record what was spoken before a barge-in interruption.
    AppendAssistantPartialHistory {
        spoken_text: String,
    },
    AppendToolResultHistory {
        tool_name: String,
        content: String,
    },

    // ── Tool / Reasoner ──
    DispatchTool {
        request_id: String,
        tool_name: String,
        args_json: String,
    },
    StartReasoner {
        task_id: String,
        description: String,
    },

    // ── Post-turn ──
    /// Persist a (user, assistant) memory pair via RAG.
    EvaluateAndStoreMemory {
        user_input: String,
        assistant_response: String,
        turn_id: String,
    },
    /// Push updated persona to Talker if memory changed since last turn.
    UpdatePersonaIfChanged,
    /// Speculatively prefill the Talker KV cache.
    PrefillCache,

    // ── Silence ──
    /// Start the three-tier silence timer cascade (soft → follow-up → context-shift).
    StartSilenceTimers,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_state_new_initializes_empty() {
        let state = TurnState::new("conv-1".into(), "# Memory".into());
        assert_eq!(state.conversation_id, "conv-1");
        assert_eq!(state.last_memory_md, "# Memory");
        assert!(!state.is_generating());
        assert!(state.current_response.is_empty());
        assert!(state.last_turn_id.is_empty());
        assert!(state.current_dispatch_kind.is_none());
    }

    #[test]
    fn is_generating_reflects_active_gen() {
        let mut state = TurnState::new("c".into(), String::new());
        assert!(!state.is_generating());

        state.active_gen = Some(CancellationToken::new());
        assert!(state.is_generating());
    }

    #[test]
    fn cancel_active_gen_clears_and_cancels() {
        let mut state = TurnState::new("c".into(), String::new());
        let token = CancellationToken::new();
        let child = token.clone();
        state.active_gen = Some(token);

        state.cancel_active_gen();

        assert!(state.active_gen.is_none());
        assert!(child.is_cancelled());
    }

    #[test]
    fn cancel_active_gen_noop_when_none() {
        let mut state = TurnState::new("c".into(), String::new());
        state.cancel_active_gen(); // should not panic
        assert!(state.active_gen.is_none());
    }

    #[test]
    fn cancel_active_silence_clears_and_cancels() {
        let mut state = TurnState::new("c".into(), String::new());
        let token = CancellationToken::new();
        let child = token.clone();
        state.active_silence = Some(token);

        state.cancel_active_silence();

        assert!(state.active_silence.is_none());
        assert!(child.is_cancelled());
    }

    #[test]
    fn pipeline_action_variants_are_comparable() {
        // Verify PartialEq works for test assertions.
        let a = PipelineAction::MuteOutput;
        let b = PipelineAction::MuteOutput;
        assert_eq!(a, b);

        let c = PipelineAction::SendSentence {
            text: "hello".into(),
        };
        let d = PipelineAction::SendSentence {
            text: "hello".into(),
        };
        assert_eq!(c, d);

        assert_ne!(PipelineAction::MuteOutput, PipelineAction::UnmuteOutput,);
    }
}
