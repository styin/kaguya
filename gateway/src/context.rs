//! Context Package 组装。
//!
//! Spec §2.3: "Before every Talker dispatch, assemble a structured
//! context package containing: user input, MEMORY.md contents, conversation
//! history, active task state, current tool list, tool/reasoner results,
//! and metadata."
//!
//! Gateway 组装结构化数据。Gateway 不知道 prompt 格式。

use crate::history::History;
use crate::memory::Memory;
use crate::tools::ToolRegistry;
use crate::types::*;
use std::time::Duration;

pub async fn assemble(
    user_input: &str,
    history: &History,
    memory: &Memory,
    tools: &ToolRegistry,
    active_tasks: &[ActiveTask],
    pending_results: &[AsyncResult],
) -> ContextPackage {
    let now = chrono::Local::now();
    ContextPackage {
        user_input: user_input.to_string(),
        memory_contents: memory.contents().await,
        history: history.recent().await,
        active_tasks: active_tasks.to_vec(),
        available_tools: tools.list(),
        pending_results: pending_results.to_vec(),
        metadata: ContextMetadata {
            timestamp: now.to_rfc3339(),
            timezone: now.format("%Z").to_string(),
        },
    }
}

/// 带工具结果的 context（工具完成后的续接轮次）
pub async fn with_tool_result(
    tool_name: &str,
    result: &serde_json::Value,
    success: bool,
    history: &History,
    memory: &Memory,
    tools: &ToolRegistry,
    active_tasks: &[ActiveTask],
) -> ContextPackage {
    assemble(
        "", // 无新用户输入 — 续接轮次
        history,
        memory,
        tools,
        active_tasks,
        &[AsyncResult {
            source: "tool".into(),
            name_or_id: tool_name.into(),
            content: result.to_string(),
            success,
        }],
    )
    .await
}

/// 带推理结果的 context
pub async fn with_reasoner_result(
    task_id: &str,
    result: &str,
    history: &History,
    memory: &Memory,
    tools: &ToolRegistry,
    active_tasks: &[ActiveTask],
) -> ContextPackage {
    assemble(
        "",
        history,
        memory,
        tools,
        active_tasks,
        &[AsyncResult {
            source: "reasoner".into(),
            name_or_id: task_id.into(),
            content: result.into(),
            success: true,
        }],
    )
    .await
}

/// 静默触发的 context
pub async fn for_silence(
    duration: Duration,
    history: &History,
    memory: &Memory,
    tools: &ToolRegistry,
) -> ContextPackage {
    assemble(
        &format!("[SYSTEM: {}s silence since last exchange]", duration.as_secs()),
        history,
        memory,
        tools,
        &[],
        &[],
    )
    .await
}

/// 叙事用 context（Reasoner 中间步骤）
pub async fn for_narration(
    step: &str,
    history: &History,
    memory: &Memory,
) -> ContextPackage {
    let now = chrono::Local::now();
    ContextPackage {
        user_input: format!("[REASONER_UPDATE: {step}]"),
        memory_contents: memory.contents().await,
        history: history.recent().await,
        active_tasks: vec![],
        available_tools: vec![],
        pending_results: vec![],
        metadata: ContextMetadata {
            timestamp: now.to_rfc3339(),
            timezone: now.format("%Z").to_string(),
        },
    }
}

/// 投机预填充用 context（n_predict: 0）
pub async fn for_prefill(
    history: &History,
    memory: &Memory,
    tools: &ToolRegistry,
    active_tasks: &[ActiveTask],
) -> ContextPackage {
    assemble("", history, memory, tools, active_tasks, &[]).await
}