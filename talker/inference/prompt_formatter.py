"""inference/prompt_formatter.py — TalkerContext + PersonaConfig → LLM prompt string.

Domain role: Formats the structured context package from the Gateway into a
complete prompt for the LLM. The Gateway assembles the context;
the Talker formats. Gateway has zero knowledge of prompt format.

[DECISION] This boundary is strict (implementation plan §2, design principle 1).

TODO: Migrate from /v1/completions (raw prompt string with hardcoded chat
template delimiters) to /v1/chat/completions (structured messages[] array).
This shifts template responsibility to the inference server, making the
Talker model-agnostic. Currently hardcodes ChatML-style delimiters
(<|im_start|>/<|im_end|>) which are correct for Qwen3 but wrong for
Llama 3, Mistral, Phi, etc. The /v1/chat/completions API is supported by
llama.cpp, LM Studio, and all OpenAI-compatible servers.
"""

import logging
from datetime import datetime, timezone

from inference.soul_container import VALID_EMOTIONS
from proto import kaguya_pb2  # type: ignore[import]

logger = logging.getLogger(__name__)

# ChatML-style delimiters. Used by Qwen3, Yi, and others.
# TODO: Remove when migrating to /v1/chat/completions (see module docstring).
_IM_START = "<|im_start|>"
_IM_END = "<|im_end|>"

# Canonical emotion tag values (spec-agent §3.3, EmotionEvent comment).
# Single source of truth lives in soul_container.VALID_EMOTIONS.
_EMOTION_VALUES = "|".join(sorted(VALID_EMOTIONS))

# Structured output instructions injected into the system prompt.
_TAG_INSTRUCTIONS = f"""\
You may use the following tags in your response. Tags are extracted before \
the user hears your speech — they are metadata, not spoken aloud.

Emotion (one per sentence, optional — defaults to neutral if omitted):
  [EMOTION:value]  where value is one of: {_EMOTION_VALUES}

Tool call (non-blocking — the tag is extracted and forwarded to the Gateway \
for execution while you continue speaking):
  [TOOL:tool_name({{"param": "value"}})]

Delegation (hands off a task to a Reasoner agent for deep work — you may \
continue speaking while the Reasoner works in the background):
  [DELEGATE:description of the task]"""

# Few-shot tool/delegation examples.
_TOOL_EXAMPLES = """\
Examples:
  User asks about a URL → "Let me check that. [TOOL:web_fetch({"url": "https://..."})]"
  User asks to save something → [TOOL:write_file({"path": "/notes.md", "content": "..."})]
  User asks a complex multi-step task → "I'll look into that. [DELEGATE:research and summarize X]"\
"""


def assemble_prompt(
    ctx: kaguya_pb2.TalkerContext,
    persona: kaguya_pb2.PersonaConfig,
) -> str:
    """Build a complete chat-template prompt from context + persona.

    Prompt structure (per spec-agent §3.3):
        1. System: SOUL.md + IDENTITY.md persona
        2. System: structured output instructions (tags)
        3. System: available tools list
        4. System: tool use examples
        5. System: current context (timestamp, active tasks)
        6. Memory: MEMORY.md contents
        7. Conversation history as alternating turns
        8. If tool_result_content: inject as tool turn
        9. If reasoner_result_content: inject as tool turn
       10. User: user_input
       11. Prime assistant generation (open-ended, no closing delimiter)
    """
    parts: list[str] = []

    # ── System message (sections 1-6 combined) ──
    system_sections: list[str] = []

    # 1. Persona
    if persona.soul_md:
        system_sections.append(persona.soul_md.strip())
    if persona.identity_md:
        system_sections.append(persona.identity_md.strip())

    # 2. Tag instructions
    system_sections.append(_TAG_INSTRUCTIONS)

    # 3. Available tools
    if ctx.tools:
        tools_text = "Available tools:\n" + "\n".join(
            f"  - {t.name}: {t.description}" for t in ctx.tools
        )
        system_sections.append(tools_text)

    # 4. Tool use examples
    system_sections.append(_TOOL_EXAMPLES)

    # 5. Current context
    context_lines: list[str] = []
    if ctx.timestamp_ms:
        ts = datetime.fromtimestamp(ctx.timestamp_ms / 1000, tz=timezone.utc)
        context_lines.append(f"Current time: {ts.isoformat()}")
    if ctx.active_tasks_json:
        context_lines.append(f"Active tasks: {ctx.active_tasks_json}")
    if context_lines:
        system_sections.append("\n".join(context_lines))

    # 6. Memory
    if ctx.memory_contents:
        system_sections.append(f"Memory:\n{ctx.memory_contents.strip()}")

    parts.append(_msg("system", "\n\n".join(system_sections)))

    # 6.5 RAG Retrieval Results (injected by Gateway)
    if ctx.retrieval_results:
        retrieval_text = "Relevant context retrieved from memory:\n"
        for r in ctx.retrieval_results:
            retrieval_text += f"  [{r.source}] {r.content}\n"
        system_sections.append(retrieval_text.strip())

    # ── Conversation history (section 7) ──
    for turn in ctx.history:
        role = _role_to_str(turn.role)
        # name parameter supports multi-user voice chat (e.g., "user:Alice").
        # Currently single-user, but the plumbing is here for future use.
        parts.append(_msg(role, turn.content, name=turn.name or None))

    # ── Tool result injection (section 8) ──
    if ctx.tool_result_content:
        label = f"[TOOL_RESULT:{ctx.tool_request_id}] " if ctx.tool_request_id else ""
        parts.append(_msg("tool", f"{label}{ctx.tool_result_content}"))

    # ── Reasoner result injection (section 9) ──
    if ctx.reasoner_result_content:
        label = (
            f"[REASONER_RESULT:{ctx.reasoner_task_id}] " if ctx.reasoner_task_id else ""
        )
        parts.append(_msg("tool", f"{label}{ctx.reasoner_result_content}"))

    # ── User input (section 10) ──
    if ctx.user_input:
        parts.append(_msg("user", ctx.user_input))

    # ── Prime assistant generation (section 11) ──
    # No closing _IM_END — this is intentional. The open-ended message signals
    # the model to generate from here. Adding _IM_END would close the message
    # and the model would start a new turn instead of generating content.
    parts.append(f"{_IM_START}assistant\n")

    return "".join(parts)


def _msg(role: str, content: str, *, name: str | None = None) -> str:
    """Format a single chat message with ChatML-style delimiters.

    The name parameter produces "<|im_start|>role:name\\n" headers, supporting
    multi-participant conversations (e.g., "user:Alice", "user:Bob").
    """
    header = f"{role}:{name}" if name else role
    return f"{_IM_START}{header}\n{content}{_IM_END}\n"


_ROLE_MAP = {
    kaguya_pb2.ROLE_SYSTEM: "system",
    kaguya_pb2.ROLE_USER: "user",
    kaguya_pb2.ROLE_ASSISTANT: "assistant",
    kaguya_pb2.ROLE_TOOL: "tool",
}


def _role_to_str(role: kaguya_pb2.Role.ValueType) -> str:
    """Convert proto Role enum to chat template role string."""
    result = _ROLE_MAP.get(role)
    if result is None:
        logger.warning("Unknown proto Role enum value %d, defaulting to 'user'", role)
        return "user"
    return result
