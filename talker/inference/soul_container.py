"""inference/soul_container.py — Deterministic post-processing of LLM sentences.

Domain role: Splits spoken text from action tags, normalizes emotion tags,
enforces vocabulary rules, and validates structured output. Pure function —
stateless, deterministic, no LLM calls.

Inspired by Project Airi's "soul container" pattern. Operates on complete
sentences after boundary detection, never on individual tokens.

[CHANGE] TOOL extraction is no longer a single non-greedy regex. Tool args are
JSON objects that, for sandbox_exec, contain arbitrary source code — code
routinely contains ')', ']', '}', and even ')]' inside string literals, which
a non-greedy regex truncates. The new scanner parses the tool name, then the
JSON object argument by brace-depth tracking with JSON-string/escape awareness,
so brackets/quotes inside code are ignored. TOOL is also extracted FIRST so
tag-like substrings inside code (e.g. "[EMOTION:joy]" printed by the program)
are protected from the emotion/unknown-tag strippers.
"""

import logging
import re
import uuid
from dataclasses import dataclass, field

from proto import kaguya_pb2  # type: ignore[import]

logger = logging.getLogger(__name__)

# ──────────────────────────────────────────
# Tag patterns
# ──────────────────────────────────────────

# [EMOTION:value]
_EMOTION_RE = re.compile(r"\[EMOTION:(\w+)\]")

# [DELEGATE:description]
_DELEGATE_RE = re.compile(r"\[DELEGATE:(.+?)\]")

# Catch-all for any remaining bracket tags to strip hallucinations.
_UNKNOWN_TAG_RE = re.compile(r"\[[A-Z_]+:[^\]]*\]")

# Collapse runs of whitespace left after tag removal.
_MULTI_SPACE_RE = re.compile(r"\s{2,}")

# [TOOL: ... ] is parsed by a JSON-aware scanner, not a regex. See
# _extract_tool_requests below.
_TOOL_MARKER = "[TOOL:"

# ──────────────────────────────────────────
# Emotion normalization map
# ──────────────────────────────────────────

_EMOTION_ALIASES: dict[str, str] = {
    "happy": "joy",
    "happiness": "joy",
    "excited": "joy",
    "sad": "concern",
    "sadness": "concern",
    "worried": "concern",
    "anxious": "concern",
    "curious": "thinking",
    "confused": "thinking",
    "shocked": "surprise",
    "surprised": "surprise",
    "confident": "determined",
    "resolute": "determined",
}

# TODO: Move valid emotions and aliases to IDENTITY.md (configurable per persona).
# Deferred until a downstream consumer (frontend, TTS prosody) defines requirements.
# Canonical list. prompt_formatter.py imports this to keep prompt instructions in sync.
VALID_EMOTIONS = frozenset(
    {"joy", "concern", "thinking", "surprise", "neutral", "determined"}
)


# ──────────────────────────────────────────
# Vocabulary rules (parsed from IDENTITY.md)
# ──────────────────────────────────────────


@dataclass
class VocabRule:
    """A single (regex pattern, replacement) pair from IDENTITY.md ## Vocabulary."""

    pattern: re.Pattern[str]
    replacement: str


@dataclass
class IdentityConfig:
    """Parsed identity configuration. Built from PersonaConfig.identity_md."""

    vocab_rules: list[VocabRule] = field(default_factory=list)


def parse_identity_config(identity_md: str) -> IdentityConfig:
    """Parse the ## Vocabulary section of IDENTITY.md into vocab rules.

    Expected format:
        ## Vocabulary
        - /pattern/ → replacement
        - /pattern/ → replacement
    """
    config = IdentityConfig()
    in_vocab = False

    for line in identity_md.splitlines():
        stripped = line.strip()
        if stripped.lower().startswith("## vocabulary"):
            in_vocab = True
            continue
        if in_vocab and stripped.startswith("##"):
            break  # next section
        if not in_vocab or not stripped.startswith("- /"):
            continue

        # Parse: - /pattern/ → replacement
        match = re.match(r"^- /(.+?)/\s*→\s*(.+)$", stripped)
        if match:
            try:
                config.vocab_rules.append(
                    VocabRule(
                        pattern=re.compile(match.group(1)),
                        replacement=match.group(2),
                    )
                )
            except re.error as exc:
                logger.warning(
                    "Malformed vocab regex in IDENTITY.md, skipping: %s", exc
                )

    return config


# ──────────────────────────────────────────
# Soul container result
# ──────────────────────────────────────────


@dataclass
class SoulContainerResult:
    """Output of processing one sentence through the soul container."""

    spoken_text: str  # tags stripped, vocab applied — goes to TTS
    emotions: list[str]  # normalized emotion values
    tool_requests: list[kaguya_pb2.ToolRequest]
    delegate_requests: list[kaguya_pb2.DelegateRequest]


# ──────────────────────────────────────────
# Tool tag extraction (JSON-aware, bracket-balanced)
# ──────────────────────────────────────────


def _extract_tool_requests(
    text: str,
) -> tuple[list[kaguya_pb2.ToolRequest], str]:
    """Extract all `[TOOL:name({...})]` tags, returning (requests, cleaned_text).

    A regex cannot reliably parse tool args that are themselves source code:
    code contains ')', ']', '}' and ')]' inside string literals, which a
    non-greedy regex truncates. This scanner walks the text, and for each
    `[TOOL:` marker tries to parse: name → '(' → JSON object (brace-balanced,
    string/escape aware) → optional whitespace → ')' → ']'. Valid tags are
    removed from the text; malformed `[TOOL:` markers are left as literal text
    (and later cleaned by _UNKNOWN_TAG_RE if they form a closeable bracket).
    """
    results: list[kaguya_pb2.ToolRequest] = []
    out: list[str] = []
    i = 0
    n = len(text)

    while i < n:
        start = text.find(_TOOL_MARKER, i)
        if start == -1:
            out.append(text[i:])
            break

        out.append(text[i:start])  # text before the tag

        parsed = _try_parse_tool(text, start)
        if parsed is None:
            # Not well-formed — keep the marker literal, resume just past it.
            out.append(_TOOL_MARKER)
            i = start + len(_TOOL_MARKER)
            continue

        name, args_json, end = parsed
        results.append(
            kaguya_pb2.ToolRequest(
                request_id=str(uuid.uuid4()),
                tool_name=name,
                args_json=args_json,
            )
        )
        i = end  # resume after the full tag

    return results, "".join(out)


def _try_parse_tool(text: str, start: int) -> tuple[str, str, int] | None:
    """Parse one tool tag beginning at `start` (index of '[TOOL:').

    Returns (tool_name, args_json, end_index_exclusive) or None if malformed.
    """
    n = len(text)
    j = start + len(_TOOL_MARKER)

    # Tool name: [A-Za-z0-9_]+
    name_start = j
    while j < n and (text[j].isalnum() or text[j] == "_"):
        j += 1
    name = text[name_start:j]
    if not name:
        return None

    # Opening paren of the call.
    if j >= n or text[j] != "(":
        return None
    j += 1

    # Skip whitespace before the JSON object.
    while j < n and text[j].isspace():
        j += 1

    # Arguments must be a JSON object.
    if j >= n or text[j] != "{":
        return None
    obj_start = j

    depth = 0
    in_str = False
    escaped = False
    closed_at = -1
    while j < n:
        c = text[j]
        if in_str:
            if escaped:
                escaped = False
            elif c == "\\":
                escaped = True
            elif c == '"':
                in_str = False
        else:
            if c == '"':
                in_str = True
            elif c == "{":
                depth += 1
            elif c == "}":
                depth -= 1
                if depth == 0:
                    closed_at = j
                    break
        j += 1

    if closed_at == -1:
        return None  # unterminated object (e.g. tag split across sentences)
    args_json = text[obj_start : closed_at + 1]
    j = closed_at + 1

    # Skip whitespace, then require ')' then ']'.
    while j < n and text[j].isspace():
        j += 1
    if j >= n or text[j] != ")":
        return None
    j += 1
    if j >= n or text[j] != "]":
        return None
    j += 1

    return name, args_json, j


# ──────────────────────────────────────────
# Core processing function
# ──────────────────────────────────────────


def process(sentence: str, identity: IdentityConfig) -> SoulContainerResult:
    """Process one complete sentence through the soul container.

    Pure function: stateless, deterministic, no I/O.

    Order matters: TOOL tags are extracted FIRST (with JSON-aware scanning) so
    that bracketed/tag-like substrings inside code are removed with the tool
    tag and never seen by the emotion / delegate / hallucination strippers.

    Args:
        sentence: Complete sentence from sentence_detector.
        identity: Parsed identity config with vocabulary rules.

    Returns:
        SoulContainerResult with spoken text separated from action metadata.
    """
    result = SoulContainerResult(
        spoken_text="",
        emotions=[],
        tool_requests=[],
        delegate_requests=[],
    )

    text = sentence

    # 1. Extract tool requests FIRST (protects code content from later regexes).
    tool_requests, text = _extract_tool_requests(text)
    result.tool_requests.extend(tool_requests)

    # 2. Extract emotion tags and normalize.
    for match in _EMOTION_RE.finditer(text):
        raw = match.group(1).lower()
        normalized = _EMOTION_ALIASES.get(raw, raw)
        if normalized in VALID_EMOTIONS:
            result.emotions.append(normalized)
        else:
            logger.debug("Unknown emotion tag dropped: %s", raw)
    text = _EMOTION_RE.sub("", text)

    # 3. Extract delegate requests.
    for match in _DELEGATE_RE.finditer(text):
        result.delegate_requests.append(
            kaguya_pb2.DelegateRequest(
                task_id=str(uuid.uuid4()),
                description=match.group(1).strip(),
            )
        )
    text = _DELEGATE_RE.sub("", text)

    # 4. Strip any remaining hallucinated tags.
    text = _UNKNOWN_TAG_RE.sub("", text)

    # 5. Clean up whitespace.
    text = _MULTI_SPACE_RE.sub(" ", text).strip()

    # 6. Apply vocabulary rules from IDENTITY.md.
    for rule in identity.vocab_rules:
        text = rule.pattern.sub(rule.replacement, text)

    # 7. Default emotion injection.
    if not result.emotions:
        result.emotions.append("neutral")

    result.spoken_text = text
    return result