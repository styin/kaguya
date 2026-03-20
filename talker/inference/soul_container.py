"""inference/soul_container.py — Deterministic post-processing of LLM sentences.

Domain role: Splits spoken text from action tags, normalizes emotion tags,
enforces vocabulary rules, and validates structured output. Pure function —
stateless, deterministic, no LLM calls.

Inspired by Project Airi's "soul container" pattern. Operates on complete
sentences after boundary detection, never on individual tokens.
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

# [TOOL:name({...})] — captures tool name and raw args string.
# No re.DOTALL: tool args are single-line JSON.
_TOOL_RE = re.compile(r"\[TOOL:(\w+)\((.+?)\)\]")

# [DELEGATE:description]
_DELEGATE_RE = re.compile(r"\[DELEGATE:(.+?)\]")

# Catch-all for any remaining bracket tags to strip hallucinations.
_UNKNOWN_TAG_RE = re.compile(r"\[[A-Z_]+:[^\]]*\]")

# Collapse runs of whitespace left after tag removal.
_MULTI_SPACE_RE = re.compile(r"\s{2,}")

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
# Core processing function
# ──────────────────────────────────────────


def process(sentence: str, identity: IdentityConfig) -> SoulContainerResult:
    """Process one complete sentence through the soul container.

    Pure function: stateless, deterministic, no I/O.

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

    # 1. Extract emotion tags and normalize.
    for match in _EMOTION_RE.finditer(text):
        raw = match.group(1).lower()
        normalized = _EMOTION_ALIASES.get(raw, raw)
        if normalized in VALID_EMOTIONS:
            result.emotions.append(normalized)
        else:
            logger.debug("Unknown emotion tag dropped: %s", raw)
    text = _EMOTION_RE.sub("", text)

    # 2. Extract tool requests.
    for match in _TOOL_RE.finditer(text):
        result.tool_requests.append(
            kaguya_pb2.ToolRequest(
                request_id=str(uuid.uuid4()),
                tool_name=match.group(1),
                args_json=match.group(2).strip(),
            )
        )
    text = _TOOL_RE.sub("", text)

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
