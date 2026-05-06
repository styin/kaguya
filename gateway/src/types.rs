//! Gateway Internal Types

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// P0: Control — Bypasses input queue. Highest priority.
#[derive(Debug, Clone)]
pub enum ControlSignal {
    Stop,
    Approval { context: String },
    Shutdown,
}

/// P1–P5 Input Priority Queue
/// - P1: User intent (`FinalTranscript`, `TextCommand`)
/// - P2: ASR states (`VadSpeechStart`, `VadSpeechEnd`, `PartialTranscript`)
/// - P3: Tool use & reasoner callbacks (`ToolResult`, `ReasonerStep`, `ReasonerCompleted`, `ReasonerError`)
/// - P4: Conversation state (`SilenceExceeded`)
/// - P5: Auxiliary events (`Telemetry`)
#[derive(Debug, Clone)]
pub enum InputEvent {
    FinalTranscript { text: String, confidence: f32 },
    TextCommand { text: String },

    VadSpeechStart,
    VadSpeechEnd { silence_duration_ms: f32 },
    PartialTranscript { text: String },

    ToolResult { request_id: String, tool_name: String, content: String },
    ReasonerStep { task_id: String, description: String },
    ReasonerCompleted { task_id: String, summary: String },
    ReasonerError { task_id: String, message: String, code: i32 },
    
    SilenceExceeded { duration: Duration },
    
    Telemetry { data: serde_json::Value },
}

#[derive(Debug, Clone, Serialize)]
pub struct ActiveTask {
    pub task_id: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataEvent {
    pub event_type: String,
    pub data: serde_json::Value,
}

/// What kind of round triggered the active Talker dispatch?
///
/// Threaded through `main.rs` so `ResponseComplete` handling can decide
/// whether to call `RagEngine::evaluate_and_store`. Only `UserIntent`
/// rounds correspond to a fresh user statement that should be paired
/// with the assistant response into a memory.
///
/// Tool-result, reasoner-narration, reasoner-result, and silence-triggered
/// rounds all run on top of an *older* user turn — `last_user_input()`
/// would return that older text and pair it with assistant text that's
/// actually responding to the tool/reasoner/silence cue, fabricating a
/// memory pair that misattributes both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchKind {
    /// P1 input — `final_transcript` or `text_command`. The assistant
    /// response is genuinely answering a fresh user statement.
    UserIntent,
    /// P3 input — assistant continues with a tool result.
    ToolResult,
    /// P3 input — narration of an in-progress Reasoner step.
    ReasonerNarration,
    /// P3 input — assistant continues with a Reasoner task summary.
    ReasonerResult,
    /// P4 input — silence-triggered re-engagement.
    Silence,
}

impl DispatchKind {
    /// Should the post-turn RAG memory write run for this dispatch kind?
    /// Only `UserIntent` rounds carry a fresh (user, assistant) pair that
    /// makes sense to persist as a memory.
    pub fn should_persist_memory(self) -> bool {
        matches!(self, DispatchKind::UserIntent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── P0-3: dispatch-kind gating for evaluate_and_store ──

    #[test]
    fn user_intent_triggers_memory_persistence() {
        assert!(DispatchKind::UserIntent.should_persist_memory());
    }

    #[test]
    fn non_user_dispatch_kinds_do_not_persist_memory() {
        // Each of these rounds runs on top of an older user turn — pairing
        // `last_user_input()` with the assistant text would produce a
        // fabricated Q/A memory misattributed to the wrong question.
        assert!(!DispatchKind::ToolResult.should_persist_memory());
        assert!(!DispatchKind::ReasonerNarration.should_persist_memory());
        assert!(!DispatchKind::ReasonerResult.should_persist_memory());
        assert!(!DispatchKind::Silence.should_persist_memory());
    }
}