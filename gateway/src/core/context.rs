//! Builds [`proto::TalkerContext`] for each dispatch variant.
//!
//! Two families: async wrappers (`assemble`, `with_tool_result`, …) that
//! fetch history/tools internally, and sync `_from_data` variants that
//! take pre-fetched `Vec<ChatMessage>` / `Vec<ToolDefinition>` for use
//! in pipeline handlers.

use crate::history::History;
use crate::proto;
use crate::tools::ToolRegistry;
use crate::types::ActiveTask;

/// Build context for a regular user turn with RAG retrieval results.
pub async fn assemble(
    conversation_id: &str,
    turn_id: &str,
    user_input: &str,
    history: &History,
    memory_md: &str,
    retrieval_results: Vec<proto::RetrievalResult>,
    tools: &ToolRegistry,
    active_tasks: &[ActiveTask],
) -> proto::TalkerContext {
    proto::TalkerContext {
        conversation_id: conversation_id.into(),
        turn_id: turn_id.into(),
        user_input: user_input.into(),
        history: history.recent().await,
        memory_contents: memory_md.into(),
        tools: tools.definitions(),
        active_tasks_json: serde_json::to_string(active_tasks).unwrap_or_default(),
        tool_result_content: String::new(),
        tool_request_id: String::new(),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        reasoner_task_id: String::new(),
        reasoner_result_content: String::new(),
        retrieval_results,
    }
}

/// Build context for a tool-result continuation dispatch.
pub async fn with_tool_result(
    conversation_id: &str,
    turn_id: &str,
    request_id: &str,
    content: &str,
    history: &History,
    memory_md: &str,
    tools: &ToolRegistry,
    active_tasks: &[ActiveTask],
) -> proto::TalkerContext {
    let mut ctx = assemble(
        conversation_id,
        turn_id,
        "",
        history,
        memory_md,
        vec![],
        tools,
        active_tasks,
    )
    .await;
    ctx.tool_request_id = request_id.into();
    ctx.tool_result_content = content.into();
    ctx
}

/// Build context for a reasoner-result continuation dispatch.
pub async fn with_reasoner_result(
    conversation_id: &str,
    turn_id: &str,
    task_id: &str,
    result: &str,
    history: &History,
    memory_md: &str,
    tools: &ToolRegistry,
    active_tasks: &[ActiveTask],
) -> proto::TalkerContext {
    let mut ctx = assemble(
        conversation_id,
        turn_id,
        "",
        history,
        memory_md,
        vec![],
        tools,
        active_tasks,
    )
    .await;
    ctx.reasoner_task_id = task_id.into();
    ctx.reasoner_result_content = result.into();
    ctx
}

/// Build context for a silence-triggered re-engagement prompt.
pub async fn for_silence(
    conversation_id: &str,
    turn_id: &str,
    duration: std::time::Duration,
    history: &History,
    memory_md: &str,
    tools: &ToolRegistry,
) -> proto::TalkerContext {
    assemble(
        conversation_id,
        turn_id,
        &format!(
            "[SYSTEM: {}s silence since last exchange]",
            duration.as_secs()
        ),
        history,
        memory_md,
        vec![],
        tools,
        &[],
    )
    .await
}

/// Build context for a reasoner narration step.
pub async fn for_narration(
    conversation_id: &str,
    turn_id: &str,
    step: &str,
    history: &History,
    memory_md: &str,
) -> proto::TalkerContext {
    proto::TalkerContext {
        conversation_id: conversation_id.into(),
        turn_id: turn_id.into(),
        user_input: format!("[REASONER_UPDATE: {step}]"),
        history: history.recent().await,
        memory_contents: memory_md.into(),
        tools: vec![],
        active_tasks_json: String::new(),
        tool_result_content: String::new(),
        tool_request_id: String::new(),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        reasoner_task_id: String::new(),
        reasoner_result_content: String::new(),
        retrieval_results: vec![],
    }
}

/// Build context for speculative KV-cache prefill.
pub async fn for_prefill(
    conversation_id: &str,
    history: &History,
    memory_md: &str,
    tools: &ToolRegistry,
    active_tasks: &[ActiveTask],
) -> proto::TalkerContext {
    assemble(
        conversation_id,
        "",
        "",
        history,
        memory_md,
        vec![],
        tools,
        active_tasks,
    )
    .await
}

// ── Sync variants (pre-fetched data, used by pipeline handlers) ────

/// Sync variant of [`assemble`]. Takes pre-fetched history and tool defs.
pub fn assemble_from_data(
    conversation_id: &str,
    turn_id: &str,
    user_input: &str,
    history: Vec<proto::ChatMessage>,
    memory_md: &str,
    retrieval_results: Vec<proto::RetrievalResult>,
    tools: Vec<proto::ToolDefinition>,
    active_tasks: &[ActiveTask],
) -> proto::TalkerContext {
    proto::TalkerContext {
        conversation_id: conversation_id.into(),
        turn_id: turn_id.into(),
        user_input: user_input.into(),
        history,
        memory_contents: memory_md.into(),
        tools,
        active_tasks_json: serde_json::to_string(active_tasks).unwrap_or_default(),
        tool_result_content: String::new(),
        tool_request_id: String::new(),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        reasoner_task_id: String::new(),
        reasoner_result_content: String::new(),
        retrieval_results,
    }
}

/// Sync variant of [`with_tool_result`].
pub fn with_tool_result_from_data(
    conversation_id: &str,
    turn_id: &str,
    request_id: &str,
    content: &str,
    history: Vec<proto::ChatMessage>,
    memory_md: &str,
    tools: Vec<proto::ToolDefinition>,
    active_tasks: &[ActiveTask],
) -> proto::TalkerContext {
    let mut ctx = assemble_from_data(
        conversation_id,
        turn_id,
        "",
        history,
        memory_md,
        vec![],
        tools,
        active_tasks,
    );
    ctx.tool_request_id = request_id.into();
    ctx.tool_result_content = content.into();
    ctx
}

/// Sync variant of [`with_reasoner_result`].
pub fn with_reasoner_result_from_data(
    conversation_id: &str,
    turn_id: &str,
    task_id: &str,
    result: &str,
    history: Vec<proto::ChatMessage>,
    memory_md: &str,
    tools: Vec<proto::ToolDefinition>,
    active_tasks: &[ActiveTask],
) -> proto::TalkerContext {
    let mut ctx = assemble_from_data(
        conversation_id,
        turn_id,
        "",
        history,
        memory_md,
        vec![],
        tools,
        active_tasks,
    );
    ctx.reasoner_task_id = task_id.into();
    ctx.reasoner_result_content = result.into();
    ctx
}

/// Sync variant of [`for_silence`].
pub fn for_silence_from_data(
    conversation_id: &str,
    turn_id: &str,
    duration: std::time::Duration,
    history: Vec<proto::ChatMessage>,
    memory_md: &str,
    tools: Vec<proto::ToolDefinition>,
) -> proto::TalkerContext {
    assemble_from_data(
        conversation_id,
        turn_id,
        &format!(
            "[SYSTEM: {}s silence since last exchange]",
            duration.as_secs()
        ),
        history,
        memory_md,
        vec![],
        tools,
        &[],
    )
}

/// Sync variant of [`for_narration`].
pub fn for_narration_from_data(
    conversation_id: &str,
    turn_id: &str,
    step: &str,
    history: Vec<proto::ChatMessage>,
    memory_md: &str,
) -> proto::TalkerContext {
    proto::TalkerContext {
        conversation_id: conversation_id.into(),
        turn_id: turn_id.into(),
        user_input: format!("[REASONER_UPDATE: {step}]"),
        history,
        memory_contents: memory_md.into(),
        tools: vec![],
        active_tasks_json: String::new(),
        tool_result_content: String::new(),
        tool_request_id: String::new(),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        reasoner_task_id: String::new(),
        reasoner_result_content: String::new(),
        retrieval_results: vec![],
    }
}

/// Sync variant of [`for_prefill`].
pub fn for_prefill_from_data(
    conversation_id: &str,
    history: Vec<proto::ChatMessage>,
    memory_md: &str,
    tools: Vec<proto::ToolDefinition>,
    active_tasks: &[ActiveTask],
) -> proto::TalkerContext {
    assemble_from_data(
        conversation_id,
        "",
        "",
        history,
        memory_md,
        vec![],
        tools,
        active_tasks,
    )
}
