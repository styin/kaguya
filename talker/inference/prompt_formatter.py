"""Render shared prompt snapshots and typed generation triggers with cached persona."""

import json
from datetime import UTC, datetime

from inference.soul_container import VALID_EMOTIONS
from proto import kaguya_pb2

_IM_START = "<|im_start|>"
_IM_END = "<|im_end|>"

_EMOTION_VALUES = "|".join(sorted(VALID_EMOTIONS))

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

_TOOL_EXAMPLES = """\
Examples:
  User asks about a URL → "Let me check that. [TOOL:web_fetch({"url": "https://..."})]"
  User asks to save something → [TOOL:write_file({"path": "/notes.md", "content": "..."})]
  User asks a complex multi-step task → "I'll look into that. [DELEGATE:research and summarize X]"\
"""


def assemble_prompt(
    ctx: kaguya_pb2.PromptContext,
    persona: kaguya_pb2.PersonaConfig,
) -> str:
    """Render a reusable prefill snapshot without inventing a generation trigger."""
    return _assemble_prompt(ctx, persona, None)


def assemble_generation_prompt(
    start: kaguya_pb2.GenerationStart,
    persona: kaguya_pb2.PersonaConfig,
) -> str:
    return _assemble_prompt(start.prompt_context, persona, start)


def _assemble_prompt(
    ctx: kaguya_pb2.PromptContext,
    persona: kaguya_pb2.PersonaConfig,
    start: kaguya_pb2.GenerationStart | None,
) -> str:
    parts: list[str] = []
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
    if start is not None and start.timestamp_ms:
        ts = datetime.fromtimestamp(start.timestamp_ms / 1000, tz=UTC)
        context_lines.append(f"Current time: {ts.isoformat()}")
    if ctx.active_tasks:
        tasks = json.dumps(
            [
                {"task_id": task.task_id, "description": task.description}
                for task in ctx.active_tasks
            ],
            ensure_ascii=False,
            separators=(",", ":"),
        )
        context_lines.append(f"Active tasks: {tasks}")
    if context_lines:
        system_sections.append("\n".join(context_lines))

    # 6. Memory
    if ctx.memory_contents:
        system_sections.append(f"Memory:\n{ctx.memory_contents.strip()}")

    # 7. Retrieval results
    if ctx.retrieval_results:
        retrieval_text = "Relevant context retrieved from memory:\n" + "\n".join(
            f"  [{r.source}] {r.content}" for r in ctx.retrieval_results
        )
        system_sections.append(retrieval_text)

    parts.append(_msg("system", "\n\n".join(system_sections)))

    # 8. Conversation history
    for turn in ctx.history:
        role = _role_to_str(turn.role)
        parts.append(_msg(role, turn.content, name=turn.name or None))

    # Render the typed trigger after the shared history snapshot.
    if start is not None:
        which = start.WhichOneof("trigger")
        if which == "tool_result":
            result = start.tool_result
            parts.append(
                _msg("tool", f"[TOOL_RESULT:{result.request_id}] {result.content}")
            )
        elif which == "reasoner_result":
            result = start.reasoner_result
            parts.append(
                _msg("tool", f"[REASONER_RESULT:{result.task_id}] {result.summary}")
            )
        elif which == "reasoner_failure":
            failure = start.reasoner_failure
            parts.append(
                _msg(
                    "tool",
                    f"[REASONER_FAILURE:{failure.task_id} code={failure.code}] "
                    f"{failure.message}",
                )
            )
        elif which == "user_input":
            parts.append(_msg("user", start.user_input.text))
        elif which == "reasoner_progress":
            parts.append(
                _msg(
                    "user", f"[REASONER_UPDATE: {start.reasoner_progress.description}]"
                )
            )
        elif which == "silence":
            seconds = start.silence.duration_ms // 1000
            parts.append(
                _msg("user", f"[SYSTEM: {seconds}s silence since last exchange]")
            )

    # 9. Prime assistant generation
    parts.append(f"{_IM_START}assistant\n")

    return "".join(parts)


def _msg(role: str, content: str, *, name: str | None = None) -> str:
    header = f"{role}:{name}" if name else role
    return f"{_IM_START}{header}\n{content}{_IM_END}\n"


_ROLE_MAP = {
    kaguya_pb2.ROLE_SYSTEM: "system",
    kaguya_pb2.ROLE_USER: "user",
    kaguya_pb2.ROLE_ASSISTANT: "assistant",
    kaguya_pb2.ROLE_TOOL: "tool",
}


def _role_to_str(role: kaguya_pb2.Role.ValueType) -> str:
    try:
        return _ROLE_MAP[role]
    except KeyError as exc:
        raise ValueError(f"Unsupported history role: {role}") from exc
