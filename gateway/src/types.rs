//! 核心类型定义 — 系统中所有流动数据的形状。
//!
//! 设计原则：类型即文档。每个类型都直接映射到 Spec 中的一个概念。

use serde::{Deserialize, Serialize};
use std::time::Duration;

// ═══════════════════════════════════════════════
// § P0: 控制信号 — 绕过 Input Stream
// ═══════════════════════════════════════════════

/// 控制信号绕过 Input Stream，直达事件循环。
/// Spec §3.1: "No event may delay a STOP."
#[derive(Debug, Clone)]
pub enum ControlSignal {
    /// 立即停止一切活动
    Stop,
    /// 批准待审批的特权操作
    Approval { request_id: String },
    /// 优雅关闭 Gateway
    Shutdown,
}

// ═══════════════════════════════════════════════
// § Input Stream 事件 — P1 到 P5
// ═══════════════════════════════════════════════

/// 所有进入 Input Stream 优先级队列的事件。
/// 事件通过对应优先级的 channel 发送。
#[derive(Debug, Clone)]
pub enum InputEvent {
    // ── P1: 完整用户意图 ──
    FinalTranscript { text: String, confidence: f32 },
    TextCommand { text: String },

    // ── P2: 部分用户信号 ──
    VadSpeechStart,
    VadSpeechEnd,
    PartialTranscript { text: String },

    // ── P3: 异步结果 ──
    ToolResult {
        call_id: String,
        tool_name: String,
        result: serde_json::Value,
        success: bool,
        error: Option<String>,
    },
    ReasonerStep {
        task_id: String,
        description: String,
        progress: f32,
    },
    ReasonerOutput {
        task_id: String,
        result: String,
    },
    ReasonerError {
        task_id: String,
        error: String,
    },

    // ── P4: 定时事件 ──
    SilenceExceeded { duration: Duration },
    ScheduledReminder { message: String },

    // ── P5: 环境 ──
    Telemetry { data: serde_json::Value },
}

// ═══════════════════════════════════════════════
// § Talker 输出事件
// ═══════════════════════════════════════════════

/// Talker 流式返回的事件。
/// 对应 proto TalkerOutput 的 oneof payload。
#[derive(Debug, Clone)]
pub enum TalkerEvent {
    /// 文本片段（流式生成）
    TextChunk { content: String, is_final: bool },
    /// TTS 音频帧
    AudioChunk { data: bytes::Bytes, sample_rate: u32 },
    /// 情绪标签 [EMOTION:...]
    EmotionTag { emotion: String, intensity: f32 },
    /// 工具调用 [TOOL:...] — Talker 决定的，不是 Gateway
    ToolCall {
        call_id: String,
        tool_name: String,
        params: serde_json::Value,
    },
    /// 委派推理 [DELEGATE:...] — Talker 决定的
    Delegate {
        task_description: String,
        context: serde_json::Value,
    },
    /// 回复完成 — 触发 history append、memory eval、prefill、silence timer
    ResponseComplete { full_text: String },
    /// 错误
    Error { message: String },
}

// ═══════════════════════════════════════════════
// § Context Package — Gateway 组装，Talker 格式化
// ═══════════════════════════════════════════════

/// 结构化 context package。
///
/// Spec §2.3: "The Talker formats this into a prompt;
/// the Gateway has no knowledge of prompt format."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPackage {
    pub user_input: String,
    /// MEMORY.md 全文（启动时读取，~1ms）
    pub memory_contents: String,
    /// 对话历史（内存中的 rolling log）
    pub history: Vec<Turn>,
    /// 活跃的 Reasoner 任务
    pub active_tasks: Vec<ActiveTask>,
    /// 可用工具列表
    pub available_tools: Vec<ToolInfo>,
    /// 待处理的异步结果（工具/推理）
    pub pending_results: Vec<AsyncResult>,
    /// 元数据
    pub metadata: ContextMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub role: TurnRole,
    pub content: String,
    pub timestamp_ms: i64,
    /// true = 这是被打断的助手回复（仅 spoken 部分）
    pub was_interrupted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TurnRole {
    User,
    Assistant,
    Tool,
    Reasoner,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveTask {
    pub task_id: String,
    pub description: String,
    pub status: TaskStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub parameters_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AsyncResult {
    pub source: String,     // "tool" | "reasoner"
    pub name_or_id: String,
    pub content: String,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextMetadata {
    pub timestamp: String,
    pub timezone: String,
}

// ═══════════════════════════════════════════════
// § 输出 / 人格
// ═══════════════════════════════════════════════

/// 元数据输出事件（文本、情绪、状态）→ endpoint display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataEvent {
    pub event_type: String,
    pub data: serde_json::Value,
}

/// 人格配置包 — 发给 Talker 的 UpdatePersona
#[derive(Debug, Clone)]
pub struct PersonaBundle {
    pub soul_md: String,
    pub identity_md: String,
    pub memory_md: String,
}