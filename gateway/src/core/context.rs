//! Builds [`proto::TalkerContext`] for each dispatch variant.
//!
//! Pipeline handlers are sync — they receive pre-fetched data and return
//! `Vec<PipelineAction>`. Context assembly therefore uses plain functions
//! (`assemble_from_data`, `with_tool_result_from_data`, …) that move
//! already-fetched `Vec<ChatMessage>` / `Vec<ToolDefinition>` into the
//! proto struct with no I/O.
//!
//! The one exception is [`for_prefill`], which is async — see its doc
//! comment for the rationale.

use crate::history::History;
use crate::proto;
use crate::tools::ToolRegistry;
use crate::types::ActiveTask;

/// Build context for speculative KV-cache prefill.
///
/// Unlike the other context builders, this function is **async** and
/// fetches history/tools itself. This is intentional: `PrefillCache` is
/// a post-turn action that runs *after* earlier actions in the same
/// `execute_all` batch have mutated state (`AppendAssistantHistory`,
/// `EvaluateAndStoreMemory`, `UpdatePersonaIfChanged`). Pre-fetching
/// history in the orchestrator — before the handler runs — would produce
/// a stale snapshot missing the assistant response that was just appended.
///
/// The executor holds `&History` and `&ToolRegistry` references, so it
/// can pass them here for a fresh read at the right moment.
pub async fn for_prefill(
    conversation_id: &str,
    history: &History,
    memory_md: &str,
    tools: &ToolRegistry,
    active_tasks: &[ActiveTask],
) -> proto::TalkerContext {
    proto::TalkerContext {
        conversation_id: conversation_id.into(),
        turn_id: String::new(),
        user_input: String::new(),
        history: history.recent().await,
        memory_contents: memory_md.into(),
        tools: tools.definitions(),
        active_tasks_json: serde_json::to_string(active_tasks).unwrap_or_default(),
        tool_result_content: String::new(),
        tool_request_id: String::new(),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        reasoner_task_id: String::new(),
        reasoner_result_content: String::new(),
        retrieval_results: vec![],
    }
}

// ── Sync builders (pre-fetched data, used by pipeline handlers) ───

/// Build context for a regular user turn with RAG retrieval results.
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

/// Build context for a tool-result continuation dispatch.
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

/// Build context for a reasoner-result continuation dispatch.
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

/// Build context for a silence-triggered re-engagement prompt.
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

/// Build context for a reasoner narration step.
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
