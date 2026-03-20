"""Tests for inference/prompt_formatter.py."""

from inference.prompt_formatter import assemble_prompt
from proto import kaguya_pb2  # type: ignore[import]


def test_basic_prompt_structure():
    ctx = kaguya_pb2.TalkerContext(
        user_input="What time is it?",
        turn_id="turn-1",
    )
    persona = kaguya_pb2.PersonaConfig(
        soul_md="You are Kaguya.",
        identity_md="Be concise.",
    )
    prompt = assemble_prompt(ctx, persona)

    # Should contain Qwen3 delimiters.
    assert "<|im_start|>system" in prompt
    assert "<|im_end|>" in prompt
    assert "<|im_start|>assistant" in prompt

    # Should contain persona text.
    assert "You are Kaguya." in prompt
    assert "Be concise." in prompt

    # Should contain user input.
    assert "What time is it?" in prompt

    # Should contain tag instructions.
    assert "[EMOTION:" in prompt
    assert "[TOOL:" in prompt
    assert "[DELEGATE:" in prompt


def test_history_included():
    ctx = kaguya_pb2.TalkerContext(
        user_input="Follow up question.",
        history=[
            kaguya_pb2.ChatMessage(
                role=kaguya_pb2.ROLE_USER,
                content="Hello",
            ),
            kaguya_pb2.ChatMessage(
                role=kaguya_pb2.ROLE_ASSISTANT,
                content="Hi there!",
            ),
        ],
    )
    persona = kaguya_pb2.PersonaConfig()
    prompt = assemble_prompt(ctx, persona)

    assert "<|im_start|>user\nHello" in prompt
    assert "<|im_start|>assistant\nHi there!" in prompt


def test_tool_result_injection():
    ctx = kaguya_pb2.TalkerContext(
        user_input="",
        tool_result_content='{"status": "healthy"}',
        tool_request_id="req-123",
    )
    persona = kaguya_pb2.PersonaConfig()
    prompt = assemble_prompt(ctx, persona)

    assert "[TOOL_RESULT:req-123]" in prompt
    assert '{"status": "healthy"}' in prompt


def test_tools_listed():
    ctx = kaguya_pb2.TalkerContext(
        user_input="Search for something.",
        tools=[
            kaguya_pb2.ToolDefinition(
                name="web_fetch",
                description="Fetch content from a URL",
            ),
        ],
    )
    persona = kaguya_pb2.PersonaConfig()
    prompt = assemble_prompt(ctx, persona)

    assert "web_fetch" in prompt
    assert "Fetch content from a URL" in prompt


def test_memory_included():
    ctx = kaguya_pb2.TalkerContext(
        user_input="Remind me.",
        memory_contents="User prefers concise responses.",
    )
    persona = kaguya_pb2.PersonaConfig()
    prompt = assemble_prompt(ctx, persona)

    assert "User prefers concise responses." in prompt


def test_empty_context_does_not_crash():
    ctx = kaguya_pb2.TalkerContext()
    persona = kaguya_pb2.PersonaConfig()
    prompt = assemble_prompt(ctx, persona)
    assert "<|im_start|>assistant\n" in prompt
