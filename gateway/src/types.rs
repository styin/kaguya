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