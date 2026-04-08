//! Context Management
//!
//! This module provides functions to assemble the TalkerContext for different scenarios:
//! - Regular user turns -> assemble()
//! - Tool result callbacks -> with_tool_result()
//! - Reasoner results -> with_reasoner_result()
//! - Silence events -> for_silence()
//! - Narration steps -> for_narration()
//! - Prefill scenarios -> for_prefill()


use crate::history::History;
use crate::memory::Memory;
use crate::proto;
use crate::tools::ToolRegistry;
use crate::types::ActiveTask;

/// 常规 user turn
pub async fn assemble(
    conversation_id: &str,
    turn_id: &str,
    user_input: &str,
    history: &History,
    memory: &Memory,
    tools: &ToolRegistry,
    active_tasks: &[ActiveTask],
) -> proto::TalkerContext {
    proto::TalkerContext {
        conversation_id: conversation_id.into(),
        turn_id: turn_id.into(),
        user_input: user_input.into(),
        history: history.recent().await,
        memory_contents: memory.contents().await,
        tools: tools.definitions(),
        active_tasks_json: serde_json::to_string(active_tasks).unwrap_or_default(),
        tool_result_content: String::new(),
        tool_request_id: String::new(),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        reasoner_task_id: String::new(),
        reasoner_result_content: String::new(),
    }
}

/// 工具结果续接
pub async fn with_tool_result(
    conversation_id: &str,
    turn_id: &str,
    request_id: &str,
    content: &str,
    history: &History,
    memory: &Memory,
    tools: &ToolRegistry,
    active_tasks: &[ActiveTask],
) -> proto::TalkerContext {
    let mut ctx = assemble(conversation_id, turn_id, "", history, memory, tools, active_tasks).await;
    ctx.tool_request_id = request_id.into();
    ctx.tool_result_content = content.into();
    ctx
}

/// Reasoner 结果续接
pub async fn with_reasoner_result(
    conversation_id: &str,
    turn_id: &str,
    task_id: &str,
    result: &str,
    history: &History,
    memory: &Memory,
    tools: &ToolRegistry,
    active_tasks: &[ActiveTask],
) -> proto::TalkerContext {
    let mut ctx = assemble(conversation_id, turn_id, "", history, memory, tools, active_tasks).await;
    ctx.reasoner_task_id = task_id.into();
    ctx.reasoner_result_content = result.into();
    ctx
}

/// 静默触发
pub async fn for_silence(
    conversation_id: &str,
    turn_id: &str,
    duration: std::time::Duration,
    history: &History,
    memory: &Memory,
    tools: &ToolRegistry,
) -> proto::TalkerContext {
    assemble(
        conversation_id,
        turn_id,
        &format!("[SYSTEM: {}s silence since last exchange]", duration.as_secs()),
        history, memory, tools, &[],
    ).await
}

/// Reasoner 中间步骤叙事
pub async fn for_narration(
    conversation_id: &str,
    turn_id: &str,
    step: &str,
    history: &History,
    memory: &Memory,
) -> proto::TalkerContext {
    proto::TalkerContext {
        conversation_id: conversation_id.into(),
        turn_id: turn_id.into(),
        user_input: format!("[REASONER_UPDATE: {step}]"),
        history: history.recent().await,
        memory_contents: memory.contents().await,
        tools: vec![],
        active_tasks_json: String::new(),
        tool_result_content: String::new(),
        tool_request_id: String::new(),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        reasoner_task_id: String::new(),
        reasoner_result_content: String::new(),
    }
}

/// 投机预填充
pub async fn for_prefill(
    conversation_id: &str,
    history: &History,
    memory: &Memory,
    tools: &ToolRegistry,
    active_tasks: &[ActiveTask],
) -> proto::TalkerContext {
    assemble(conversation_id, "", "", history, memory, tools, active_tasks).await
}