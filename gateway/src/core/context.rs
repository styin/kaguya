//! Builds prompt snapshots and typed generation triggers without I/O.
//!
//! Handlers receive already-fetched history/tools and create a single
//! `GenerationStart`. Speculative prefill reads fresh post-response history
//! asynchronously, but carries no synthetic generation or triggering input.

use crate::core::history::History;
use crate::core::types::ActiveTask;
use crate::proto;
use crate::proto::SessionRef;
use crate::tools::ToolRegistry;

/// Read the prompt snapshot after post-response history/persona updates.
/// Pre-fetching it before those executor actions would omit the response
/// that just completed.
pub async fn for_prefill(
    history: &History,
    memory_md: &str,
    tools: &ToolRegistry,
    active_tasks: &[ActiveTask],
) -> proto::PromptContext {
    prompt_from_data(
        history.recent().await,
        memory_md,
        vec![],
        tools.definitions(),
        active_tasks,
    )
}

fn prompt_from_data(
    history: Vec<proto::ChatMessage>,
    memory_md: &str,
    retrieval_results: Vec<proto::RetrievalResult>,
    tools: Vec<proto::ToolDefinition>,
    active_tasks: &[ActiveTask],
) -> proto::PromptContext {
    proto::PromptContext {
        history,
        memory_contents: memory_md.into(),
        tools,
        active_tasks: active_tasks
            .iter()
            .map(|task| proto::ActiveTask {
                task_id: task.task_id.clone(),
                description: task.description.clone(),
            })
            .collect(),
        retrieval_results,
    }
}

fn generation(
    session: &SessionRef,
    generation_id: &str,
    prompt_context: proto::PromptContext,
    trigger: proto::generation_start::Trigger,
) -> proto::GenerationStart {
    proto::GenerationStart {
        session: Some(session.clone()),
        generation_id: generation_id.into(),
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        prompt_context: Some(prompt_context),
        trigger: Some(trigger),
    }
}

/// Build a fresh user-input generation, retaining its retrieved memories.
pub fn assemble_from_data(
    session: &SessionRef,
    generation_id: &str,
    user_input: &str,
    history: Vec<proto::ChatMessage>,
    memory_md: &str,
    retrieval_results: Vec<proto::RetrievalResult>,
    tools: Vec<proto::ToolDefinition>,
    active_tasks: &[ActiveTask],
) -> proto::GenerationStart {
    generation(
        session,
        generation_id,
        prompt_from_data(history, memory_md, retrieval_results, tools, active_tasks),
        proto::generation_start::Trigger::UserInput(proto::UserInput {
            text: user_input.into(),
        }),
    )
}

/// A result references its original tool request, under a fresh generation.
pub fn with_tool_result_from_data(
    session: &SessionRef,
    generation_id: &str,
    request_id: &str,
    content: &str,
    history: Vec<proto::ChatMessage>,
    memory_md: &str,
    tools: Vec<proto::ToolDefinition>,
    active_tasks: &[ActiveTask],
) -> proto::GenerationStart {
    generation(
        session,
        generation_id,
        prompt_from_data(history, memory_md, vec![], tools, active_tasks),
        proto::generation_start::Trigger::ToolResult(proto::ToolResult {
            request_id: request_id.into(),
            content: content.into(),
        }),
    )
}

/// A completion references its Reasoner task, under a fresh generation.
pub fn with_reasoner_result_from_data(
    session: &SessionRef,
    generation_id: &str,
    task_id: &str,
    result: &str,
    history: Vec<proto::ChatMessage>,
    memory_md: &str,
    tools: Vec<proto::ToolDefinition>,
    active_tasks: &[ActiveTask],
) -> proto::GenerationStart {
    generation(
        session,
        generation_id,
        prompt_from_data(history, memory_md, vec![], tools, active_tasks),
        proto::generation_start::Trigger::ReasonerResult(proto::ReasonerResult {
            task_id: task_id.into(),
            summary: result.into(),
        }),
    )
}

/// Failed delegation carries a typed cause rather than a completion summary.
pub fn with_reasoner_failure_from_data(
    session: &SessionRef,
    generation_id: &str,
    failure: proto::ReasonerFailure,
    history: Vec<proto::ChatMessage>,
    memory_md: &str,
    tools: Vec<proto::ToolDefinition>,
    active_tasks: &[ActiveTask],
) -> proto::GenerationStart {
    generation(
        session,
        generation_id,
        prompt_from_data(history, memory_md, vec![], tools, active_tasks),
        proto::generation_start::Trigger::ReasonerFailure(failure),
    )
}

/// Silence is typed input rather than a synthetic user utterance.
pub fn for_silence_from_data(
    session: &SessionRef,
    generation_id: &str,
    duration: std::time::Duration,
    history: Vec<proto::ChatMessage>,
    memory_md: &str,
    tools: Vec<proto::ToolDefinition>,
) -> proto::GenerationStart {
    generation(
        session,
        generation_id,
        prompt_from_data(history, memory_md, vec![], tools, &[]),
        proto::generation_start::Trigger::Silence(proto::SilenceTrigger {
            duration_ms: duration.as_millis().min(u64::MAX as u128) as u64,
        }),
    )
}

/// Retain the originating task when narrating an intermediate step.
/// Narration does not expose tools or active-task prompts.
pub fn for_narration_from_data(
    session: &SessionRef,
    generation_id: &str,
    task_id: &str,
    step: &str,
    history: Vec<proto::ChatMessage>,
    memory_md: &str,
) -> proto::GenerationStart {
    generation(
        session,
        generation_id,
        prompt_from_data(history, memory_md, vec![], vec![], &[]),
        proto::generation_start::Trigger::ReasonerProgress(proto::ReasonerProgress {
            task_id: task_id.into(),
            description: step.into(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_generation_keeps_prompt_snapshot_and_typed_tasks() {
        let session = SessionRef {
            instance_id: Some(proto::InstanceId {
                value: "00000000-0000-4000-8000-000000000001".into(),
            }),
            session_id: Some(proto::SessionId {
                value: "00000000-0000-4000-8000-000000000002".into(),
            }),
        };
        let history = vec![proto::ChatMessage {
            role: proto::Role::User as i32,
            content: "earlier input".into(),
            name: String::new(),
            timestamp_ms: 1,
        }];
        let retrieval = vec![proto::RetrievalResult {
            id: "memory-1".into(),
            content: "relevant fact".into(),
            source: "bm25".into(),
            score: 0.5,
        }];
        let tasks = vec![ActiveTask {
            task_id: "task-1".into(),
            description: "research".into(),
        }];
        let start = assemble_from_data(
            &session,
            "generation-1",
            "new question",
            history.clone(),
            "# Memory",
            retrieval.clone(),
            vec![],
            &tasks,
        );
        assert_eq!(start.session, Some(session.clone()));
        assert_eq!(start.generation_id, "generation-1");
        assert!(matches!(start.trigger,
            Some(proto::generation_start::Trigger::UserInput(proto::UserInput { text }))
            if text == "new question"));
        let prompt = start.prompt_context.unwrap();
        assert_eq!(prompt.history, history);
        assert_eq!(prompt.memory_contents, "# Memory");
        assert_eq!(prompt.retrieval_results, retrieval);
        assert_eq!(prompt.active_tasks[0].task_id, "task-1");
        assert_eq!(prompt.active_tasks[0].description, "research");
    }

    #[test]
    fn continuation_reference_lives_with_its_result() {
        let session = SessionRef {
            instance_id: Some(proto::InstanceId {
                value: "00000000-0000-4000-8000-000000000001".into(),
            }),
            session_id: Some(proto::SessionId {
                value: "00000000-0000-4000-8000-000000000002".into(),
            }),
        };
        let tool = with_tool_result_from_data(
            &session,
            "generation-2",
            "request-1",
            "tool data",
            vec![],
            "",
            vec![],
            &[],
        );
        assert!(matches!(tool.trigger,
            Some(proto::generation_start::Trigger::ToolResult(proto::ToolResult { request_id, content }))
            if request_id == "request-1" && content == "tool data"));
        let reasoner = with_reasoner_result_from_data(
            &session,
            "generation-3",
            "task-1",
            "summary",
            vec![],
            "",
            vec![],
            &[],
        );
        assert!(matches!(reasoner.trigger,
            Some(proto::generation_start::Trigger::ReasonerResult(proto::ReasonerResult { task_id, summary }))
            if task_id == "task-1" && summary == "summary"));
    }

    #[test]
    fn progress_keeps_task_reference_and_silence_keeps_duration() {
        let session = SessionRef {
            instance_id: Some(proto::InstanceId {
                value: "00000000-0000-4000-8000-000000000001".into(),
            }),
            session_id: Some(proto::SessionId {
                value: "00000000-0000-4000-8000-000000000002".into(),
            }),
        };
        let progress =
            for_narration_from_data(&session, "generation-4", "task-1", "searching", vec![], "");
        assert!(matches!(progress.trigger,
            Some(proto::generation_start::Trigger::ReasonerProgress(proto::ReasonerProgress { task_id, description }))
            if task_id == "task-1" && description == "searching"));
        let silence = for_silence_from_data(
            &session,
            "generation-5",
            std::time::Duration::from_millis(12_345),
            vec![],
            "",
            vec![],
        );
        assert!(matches!(
            silence.trigger,
            Some(proto::generation_start::Trigger::Silence(
                proto::SilenceTrigger {
                    duration_ms: 12_345
                }
            ))
        ));
    }
}
