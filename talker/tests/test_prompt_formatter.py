"""Prompt snapshots and typed triggers preserve the LLM's semantic input."""

import pytest

from inference.prompt_formatter import assemble_generation_prompt, assemble_prompt
from proto import kaguya_pb2


def test_prompt_snapshot_contains_cached_persona_history_tools_memory_and_retrieval():
    prompt_context = kaguya_pb2.PromptContext(
        history=[
            kaguya_pb2.ChatMessage(role=kaguya_pb2.ROLE_USER, content="Hello"),
            kaguya_pb2.ChatMessage(role=kaguya_pb2.ROLE_ASSISTANT, content="Hi there!"),
        ],
        tools=[kaguya_pb2.ToolDefinition(name="web_fetch", description="Fetch a URL")],
        memory_contents="User prefers concise responses.",
        active_tasks=[kaguya_pb2.ActiveTask(task_id="task-1", description="Research")],
        retrieval_results=[
            kaguya_pb2.RetrievalResult(source="bm25", content="Retrieved fact")
        ],
    )
    persona = kaguya_pb2.PersonaConfig(
        soul_md="You are Kaguya.", identity_md="Be concise."
    )
    start = kaguya_pb2.GenerationStart(
        generation_id="generation-1",
        timestamp_ms=1000,
        prompt_context=prompt_context,
        user_input=kaguya_pb2.UserInput(text="What time is it?"),
    )
    prompt = assemble_generation_prompt(start, persona)
    for text in (
        "<|im_start|>system",
        "<|im_end|>",
        "<|im_start|>assistant\n",
        "You are Kaguya.",
        "Be concise.",
        "What time is it?",
        "[EMOTION:",
        "[TOOL:",
        "[DELEGATE:",
        "web_fetch",
        "Fetch a URL",
        "User prefers concise responses.",
        "[bm25] Retrieved fact",
        "<|im_start|>user\nHello",
        "<|im_start|>assistant\nHi there!",
        'Active tasks: [{"task_id":"task-1","description":"Research"}]',
        "Current time: 1970-01-01T00:00:01+00:00",
    ):
        assert text in prompt
    # Retrieval remains in the system block rather than being appended too late.
    assert prompt.index("Retrieved fact") < prompt.index("<|im_end|>")


@pytest.mark.parametrize(
    ("field", "trigger", "role", "text"),
    [
        ("user_input", kaguya_pb2.UserInput(text="Follow up."), "user", "Follow up."),
        (
            "tool_result",
            kaguya_pb2.ToolResult(request_id="request-1", content="Healthy"),
            "tool",
            "[TOOL_RESULT:request-1] Healthy",
        ),
        (
            "reasoner_result",
            kaguya_pb2.ReasonerResult(task_id="task-1", summary="Done"),
            "tool",
            "[REASONER_RESULT:task-1] Done",
        ),
        (
            "reasoner_failure",
            kaguya_pb2.ReasonerFailure(
                task_id="task-1", message="Service unavailable", code=14
            ),
            "tool",
            "[REASONER_FAILURE:task-1 code=14] Service unavailable",
        ),
        (
            "reasoner_progress",
            kaguya_pb2.ReasonerProgress(
                task_id="task-1", description="Reading sources"
            ),
            "user",
            "[REASONER_UPDATE: Reading sources]",
        ),
        (
            "silence",
            kaguya_pb2.SilenceTrigger(duration_ms=2500),
            "user",
            "[SYSTEM: 2s silence since last exchange]",
        ),
    ],
)
def test_typed_trigger_preserves_rendering(field, trigger, role, text):
    start = kaguya_pb2.GenerationStart(
        generation_id="generation-1",
        prompt_context=kaguya_pb2.PromptContext(),
        **{field: trigger},
    )
    prompt = assemble_generation_prompt(start, kaguya_pb2.PersonaConfig())
    assert f"<|im_start|>{role}\n{text}<|im_end|>" in prompt
    assert prompt.endswith("<|im_start|>assistant\n")


def test_prefill_renders_snapshot_without_synthetic_generation_or_trigger():
    prompt = assemble_prompt(
        kaguya_pb2.PromptContext(memory_contents="Shared memory"),
        kaguya_pb2.PersonaConfig(soul_md="Cached persona"),
    )
    assert "Shared memory" in prompt
    assert "Cached persona" in prompt
    assert "Current time:" not in prompt
    assert "<|im_start|>user" not in prompt
    assert prompt.endswith("<|im_start|>assistant\n")


@pytest.mark.parametrize("role", [kaguya_pb2.ROLE_UNSPECIFIED, 99])
def test_unknown_history_role_is_rejected(role):
    prompt_context = kaguya_pb2.PromptContext(
        history=[kaguya_pb2.ChatMessage(role=role, content="Unclassified content")]
    )
    with pytest.raises(ValueError, match="Unsupported history role"):
        assemble_prompt(prompt_context, kaguya_pb2.PersonaConfig())
