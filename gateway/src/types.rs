//! Gateway 内部类型。
//! Proto 类型（ChatMessage, TalkerContext 等）在 proto 模块中。

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// P0: 控制信号 — 绕过 Input Stream，最高优先级
#[derive(Debug, Clone)]
pub enum ControlSignal {
    Stop,
    Approval { context: String },
    Shutdown,
}

/// P1–P5 Input Stream 事件
#[derive(Debug, Clone)]
pub enum InputEvent {
    // P1: 完整用户意图
    FinalTranscript { text: String, confidence: f32 },
    TextCommand { text: String },
    // P2: 部分信号
    VadSpeechStart,
    VadSpeechEnd { silence_duration_ms: f32 },
    PartialTranscript { text: String },
    // P3: 异步结果
    ToolResult { request_id: String, content: String },
    ReasonerStep { task_id: String, description: String },
    ReasonerCompleted { task_id: String, summary: String },
    ReasonerError { task_id: String, message: String, code: i32 },
    // P4: 静默
    SilenceExceeded { duration: Duration },
    // P5: 环境
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