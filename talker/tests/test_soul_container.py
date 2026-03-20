"""Tests for inference/soul_container.py.

Organized by category so a failing test name immediately tells you
*what class* of behavior regressed.
"""

import re
import uuid

from inference.soul_container import (
    IdentityConfig,
    VocabRule,
    parse_identity_config,
    process,
)


# ── Category 1: Plain text (no tags) ────────────────────────


class TestPlainText:
    """Sentences with no tags — spoken text passes through, default emotion added."""

    def test_plain_sentence(self):
        result = process("Hello, how are you today?", IdentityConfig())
        assert result.spoken_text == "Hello, how are you today?"
        assert result.emotions == ["neutral"]
        assert result.tool_requests == []
        assert result.delegate_requests == []

    def test_empty_string(self):
        result = process("", IdentityConfig())
        assert result.spoken_text == ""
        assert result.emotions == ["neutral"]

    def test_whitespace_only(self):
        result = process("   ", IdentityConfig())
        assert result.spoken_text == ""
        assert result.emotions == ["neutral"]

    def test_punctuation_preserved(self):
        result = process("Really?! That's amazing!", IdentityConfig())
        assert result.spoken_text == "Really?! That's amazing!"

    def test_unicode_preserved(self):
        result = process("The caf\u00e9 is lovely.", IdentityConfig())
        assert result.spoken_text == "The caf\u00e9 is lovely."


# ── Category 2: Emotion extraction and normalization ─────────


class TestEmotions:
    """[EMOTION:value] tag extraction, alias resolution, and validation."""

    def test_direct_valid_emotion(self):
        result = process("[EMOTION:joy] Great!", IdentityConfig())
        assert result.emotions == ["joy"]

    def test_alias_happy_to_joy(self):
        result = process("Nice! [EMOTION:happy]", IdentityConfig())
        assert result.emotions == ["joy"]

    def test_alias_happiness_to_joy(self):
        result = process("[EMOTION:happiness] Wonderful!", IdentityConfig())
        assert result.emotions == ["joy"]

    def test_alias_excited_to_joy(self):
        result = process("[EMOTION:excited] Yes!", IdentityConfig())
        assert result.emotions == ["joy"]

    def test_alias_sad_to_concern(self):
        result = process("[EMOTION:sad] That's tough.", IdentityConfig())
        assert result.emotions == ["concern"]

    def test_alias_sadness_to_concern(self):
        result = process("[EMOTION:sadness] I'm sorry.", IdentityConfig())
        assert result.emotions == ["concern"]

    def test_alias_worried_to_concern(self):
        result = process("[EMOTION:worried] Be careful.", IdentityConfig())
        assert result.emotions == ["concern"]

    def test_alias_anxious_to_concern(self):
        result = process("[EMOTION:anxious] I hope it works.", IdentityConfig())
        assert result.emotions == ["concern"]

    def test_alias_curious_to_thinking(self):
        result = process("[EMOTION:curious] Tell me more.", IdentityConfig())
        assert result.emotions == ["thinking"]

    def test_alias_confused_to_thinking(self):
        result = process("[EMOTION:confused] I don't understand.", IdentityConfig())
        assert result.emotions == ["thinking"]

    def test_alias_shocked_to_surprise(self):
        result = process("[EMOTION:shocked] No way!", IdentityConfig())
        assert result.emotions == ["surprise"]

    def test_alias_surprised_to_surprise(self):
        result = process("[EMOTION:surprised] Really?", IdentityConfig())
        assert result.emotions == ["surprise"]

    def test_alias_confident_to_determined(self):
        result = process("[EMOTION:confident] We can do this.", IdentityConfig())
        assert result.emotions == ["determined"]

    def test_alias_resolute_to_determined(self):
        result = process("[EMOTION:resolute] No turning back.", IdentityConfig())
        assert result.emotions == ["determined"]

    def test_direct_thinking(self):
        result = process("[EMOTION:thinking] Let me consider.", IdentityConfig())
        assert result.emotions == ["thinking"]

    def test_direct_neutral(self):
        result = process("[EMOTION:neutral] Okay.", IdentityConfig())
        assert result.emotions == ["neutral"]

    def test_direct_determined(self):
        result = process("[EMOTION:determined] Let's go.", IdentityConfig())
        assert result.emotions == ["determined"]

    def test_unknown_emotion_dropped(self):
        """Unknown emotions not in aliases or valid set are silently dropped."""
        result = process("[EMOTION:angry] I'm upset.", IdentityConfig())
        assert result.emotions == ["neutral"]  # fell through to default
        assert result.spoken_text == "I'm upset."

    def test_unknown_emotion_tag_still_stripped(self):
        """Even dropped emotions get their tags removed from spoken text."""
        result = process("Hello [EMOTION:furious] there.", IdentityConfig())
        assert "[EMOTION" not in result.spoken_text
        assert result.spoken_text == "Hello there."

    def test_case_insensitive_emotion(self):
        """Emotion values are lowercased before lookup."""
        result = process("[EMOTION:Happy] Yes!", IdentityConfig())
        assert result.emotions == ["joy"]

    def test_default_neutral_when_no_emotion(self):
        result = process("Just a normal sentence.", IdentityConfig())
        assert result.emotions == ["neutral"]

    def test_no_default_when_explicit_emotion(self):
        result = process("[EMOTION:surprise] Wow!", IdentityConfig())
        assert result.emotions == ["surprise"]
        assert "neutral" not in result.emotions

    def test_multiple_emotions(self):
        result = process(
            "[EMOTION:joy] Yay! [EMOTION:surprise] Wait!", IdentityConfig()
        )
        assert result.emotions == ["joy", "surprise"]

    def test_emotion_at_start(self):
        result = process("[EMOTION:joy] Good morning!", IdentityConfig())
        assert result.spoken_text == "Good morning!"

    def test_emotion_at_end(self):
        result = process("Good morning! [EMOTION:joy]", IdentityConfig())
        assert result.spoken_text == "Good morning!"

    def test_emotion_mid_sentence(self):
        result = process("I'm [EMOTION:joy] so glad!", IdentityConfig())
        assert result.spoken_text == "I'm so glad!"


# ── Category 3: Tool request extraction ─────────────────────


class TestToolRequests:
    """[TOOL:name({...})] tag extraction."""

    def test_basic_tool(self):
        result = process(
            'Let me check. [TOOL:web_fetch({"url": "https://example.com"})]',
            IdentityConfig(),
        )
        assert result.spoken_text == "Let me check."
        assert len(result.tool_requests) == 1
        assert result.tool_requests[0].tool_name == "web_fetch"
        assert '"https://example.com"' in result.tool_requests[0].args_json

    def test_tool_request_has_uuid(self):
        result = process('[TOOL:test({"a": 1})]', IdentityConfig())
        # Verify it's a valid UUID.
        uuid.UUID(result.tool_requests[0].request_id)

    def test_tool_args_stripped(self):
        result = process('[TOOL:foo(  {"key": "val"}  )]', IdentityConfig())
        assert result.tool_requests[0].args_json == '{"key": "val"}'

    def test_tool_tag_stripped_from_text(self):
        result = process(
            'I will search now. [TOOL:search({"q": "test"})] Please wait.',
            IdentityConfig(),
        )
        assert "[TOOL" not in result.spoken_text
        assert "I will search now." in result.spoken_text
        assert "Please wait." in result.spoken_text

    def test_multiple_tools(self):
        sentence = '[TOOL:foo({"a": 1})] Doing two things. [TOOL:bar({"b": 2})]'
        result = process(sentence, IdentityConfig())
        assert len(result.tool_requests) == 2
        assert result.tool_requests[0].tool_name == "foo"
        assert result.tool_requests[1].tool_name == "bar"

    def test_tool_with_nested_json(self):
        sentence = '[TOOL:complex({"nested": {"key": "val"}})]'
        result = process(sentence, IdentityConfig())
        assert len(result.tool_requests) == 1
        # Non-greedy match stops at first )] — check what we get.
        assert result.tool_requests[0].tool_name == "complex"

    def test_tool_name_alphanumeric(self):
        result = process('[TOOL:my_tool_v2({"x": 1})]', IdentityConfig())
        assert result.tool_requests[0].tool_name == "my_tool_v2"

    def test_malformed_tool_not_extracted(self):
        """Missing closing bracket — should not match _TOOL_RE."""
        result = process("Check [TOOL:bad({) here.", IdentityConfig())
        # _TOOL_RE won't match, but _UNKNOWN_TAG_RE might catch part of it.
        assert len(result.tool_requests) == 0


# ── Category 4: Delegate request extraction ──────────────────


class TestDelegateRequests:
    """[DELEGATE:description] tag extraction."""

    def test_basic_delegate(self):
        result = process(
            "I'll look into that. [DELEGATE:research pipeline status]",
            IdentityConfig(),
        )
        assert result.spoken_text == "I'll look into that."
        assert len(result.delegate_requests) == 1
        assert "pipeline status" in result.delegate_requests[0].description

    def test_delegate_has_uuid(self):
        result = process("[DELEGATE:do something]", IdentityConfig())
        uuid.UUID(result.delegate_requests[0].task_id)

    def test_delegate_description_stripped(self):
        result = process("[DELEGATE:  analyze the data  ]", IdentityConfig())
        assert result.delegate_requests[0].description == "analyze the data"

    def test_delegate_tag_stripped_from_text(self):
        result = process(
            "Let me hand this off. [DELEGATE:deep analysis] Back to you.",
            IdentityConfig(),
        )
        assert "[DELEGATE" not in result.spoken_text
        assert "Let me hand this off." in result.spoken_text

    def test_multiple_delegates(self):
        sentence = "[DELEGATE:task one] [DELEGATE:task two] Working on it."
        result = process(sentence, IdentityConfig())
        assert len(result.delegate_requests) == 2


# ── Category 5: Hallucinated tag stripping ───────────────────


class TestHallucinatedTags:
    """Unknown [TAG:...] patterns stripped by _UNKNOWN_TAG_RE."""

    def test_unknown_tag_stripped(self):
        result = process("Hello [FAKE:garbage] world.", IdentityConfig())
        assert result.spoken_text == "Hello world."

    def test_multiple_unknown_tags(self):
        result = process("[FOO:bar] Hi [BAZ:qux] there.", IdentityConfig())
        assert result.spoken_text == "Hi there."

    def test_underscore_in_tag_name(self):
        result = process("[MY_TAG:value] Text.", IdentityConfig())
        assert result.spoken_text == "Text."

    def test_known_tags_not_double_stripped(self):
        """EMOTION/TOOL/DELEGATE are extracted first — _UNKNOWN_TAG_RE is a safety net."""
        result = process("[EMOTION:joy] Hi.", IdentityConfig())
        assert result.spoken_text == "Hi."
        assert result.emotions == ["joy"]

    def test_lowercase_tag_not_stripped(self):
        """_UNKNOWN_TAG_RE only matches [A-Z_]+ — lowercase tags pass through."""
        result = process("See [note:important] here.", IdentityConfig())
        assert "[note:important]" in result.spoken_text


# ── Category 6: Multiple tags in one sentence ───────────────


class TestMultipleTags:
    """Sentences with combinations of emotion, tool, delegate, and unknown tags."""

    def test_emotion_and_tool(self):
        sentence = '[EMOTION:joy] Great! [TOOL:notify({"msg": "done"})]'
        result = process(sentence, IdentityConfig())
        assert result.emotions == ["joy"]
        assert len(result.tool_requests) == 1
        assert "Great!" in result.spoken_text

    def test_emotion_and_delegate(self):
        sentence = "[EMOTION:thinking] Let me think. [DELEGATE:deep analysis]"
        result = process(sentence, IdentityConfig())
        assert result.emotions == ["thinking"]
        assert len(result.delegate_requests) == 1

    def test_tool_and_delegate(self):
        sentence = '[TOOL:fetch({"x": 1})] [DELEGATE:process results] Doing it.'
        result = process(sentence, IdentityConfig())
        assert len(result.tool_requests) == 1
        assert len(result.delegate_requests) == 1

    def test_all_tag_types(self):
        sentence = (
            "[EMOTION:determined] On it! "
            '[TOOL:search({"q": "data"})] '
            "[DELEGATE:analyze findings] "
            "[UNKNOWN:hallucination]"
        )
        result = process(sentence, IdentityConfig())
        assert result.emotions == ["determined"]
        assert len(result.tool_requests) == 1
        assert len(result.delegate_requests) == 1
        assert "[" not in result.spoken_text
        assert "On it!" in result.spoken_text


# ── Category 7: Whitespace cleanup ──────────────────────────


class TestWhitespace:
    """Tags leave gaps — double spaces should collapse, edges should trim."""

    def test_double_space_collapsed(self):
        result = process("Hello  world.", IdentityConfig())
        assert result.spoken_text == "Hello world."

    def test_tag_gap_collapsed(self):
        result = process("Before [EMOTION:joy] after.", IdentityConfig())
        assert result.spoken_text == "Before after."

    def test_leading_trailing_stripped(self):
        result = process("  Hello world.  ", IdentityConfig())
        assert result.spoken_text == "Hello world."

    def test_multiple_tags_leave_clean_text(self):
        result = process(
            "[EMOTION:joy] Hello [FAKE:x] beautiful [EMOTION:surprise] world!",
            IdentityConfig(),
        )
        assert result.spoken_text == "Hello beautiful world!"


# ── Category 8: Vocabulary rules ────────────────────────────


class TestVocabRules:
    """Vocabulary substitution from IDENTITY.md rules."""

    def test_simple_replacement(self):
        identity = IdentityConfig(
            vocab_rules=[
                VocabRule(pattern=re.compile(r"\bAI\b"), replacement="A.I."),
            ]
        )
        result = process("AI is fascinating.", identity)
        assert result.spoken_text == "A.I. is fascinating."

    def test_multiple_rules(self):
        identity = IdentityConfig(
            vocab_rules=[
                VocabRule(pattern=re.compile(r"\bAI\b"), replacement="A.I."),
                VocabRule(pattern=re.compile(r"\bgonna\b"), replacement="going to"),
            ]
        )
        result = process("AI is gonna change everything.", identity)
        assert result.spoken_text == "A.I. is going to change everything."

    def test_rule_applies_globally(self):
        identity = IdentityConfig(
            vocab_rules=[
                VocabRule(pattern=re.compile(r"\bAI\b"), replacement="A.I."),
            ]
        )
        result = process("AI helps AI researchers.", identity)
        assert result.spoken_text == "A.I. helps A.I. researchers."

    def test_vocab_applied_after_tag_stripping(self):
        """Vocab rules see clean text, not raw tags."""
        identity = IdentityConfig(
            vocab_rules=[
                VocabRule(pattern=re.compile(r"\bAI\b"), replacement="A.I."),
            ]
        )
        result = process("[EMOTION:joy] AI is great!", identity)
        assert result.spoken_text == "A.I. is great!"

    def test_no_rules_no_change(self):
        result = process("AI is here.", IdentityConfig())
        assert result.spoken_text == "AI is here."

    def test_regex_pattern_in_rule(self):
        identity = IdentityConfig(
            vocab_rules=[
                VocabRule(pattern=re.compile(r"\d+"), replacement="NUMBER"),
            ]
        )
        result = process("I have 42 items.", identity)
        assert result.spoken_text == "I have NUMBER items."


# ── Category 9: parse_identity_config ────────────────────────


class TestParseIdentityConfig:
    """Parsing IDENTITY.md markdown into IdentityConfig."""

    def test_basic_parsing(self):
        md = """## Vocabulary
- /\\bAI\\b/ → A.I.
- /gonna/ → going to
"""
        config = parse_identity_config(md)
        assert len(config.vocab_rules) == 2
        assert config.vocab_rules[0].pattern.pattern == r"\bAI\b"
        assert config.vocab_rules[0].replacement == "A.I."
        assert config.vocab_rules[1].replacement == "going to"

    def test_empty_string(self):
        config = parse_identity_config("")
        assert config.vocab_rules == []

    def test_no_vocabulary_section(self):
        md = """## Personality
Warm and helpful.
"""
        config = parse_identity_config(md)
        assert config.vocab_rules == []

    def test_stops_at_next_section(self):
        md = """## Vocabulary
- /foo/ → bar

## Other Section
- /baz/ → qux
"""
        config = parse_identity_config(md)
        assert len(config.vocab_rules) == 1
        assert config.vocab_rules[0].replacement == "bar"

    def test_skips_non_rule_lines(self):
        md = """## Vocabulary
Some explanatory text here.

- /\\bAI\\b/ → A.I.

More text.
- /gonna/ → going to
"""
        config = parse_identity_config(md)
        assert len(config.vocab_rules) == 2

    def test_malformed_regex_skipped(self):
        md = """## Vocabulary
- /[invalid/ → replacement
- /\\bAI\\b/ → A.I.
"""
        config = parse_identity_config(md)
        assert len(config.vocab_rules) == 1  # bad one skipped
        assert config.vocab_rules[0].replacement == "A.I."

    def test_case_insensitive_header(self):
        md = """## vocabulary
- /foo/ → bar
"""
        config = parse_identity_config(md)
        assert len(config.vocab_rules) == 1

    def test_vocabulary_in_middle_of_doc(self):
        md = """# Identity

## Personality
Kind.

## Vocabulary
- /\\bOK\\b/ → okay

## Speech Patterns
Natural.
"""
        config = parse_identity_config(md)
        assert len(config.vocab_rules) == 1
        assert config.vocab_rules[0].replacement == "okay"

    def test_arrow_with_varying_whitespace(self):
        md = """## Vocabulary
- /foo/→bar
- /baz/  →  qux
"""
        config = parse_identity_config(md)
        assert len(config.vocab_rules) == 2
        assert config.vocab_rules[0].replacement == "bar"
        assert config.vocab_rules[1].replacement == "qux"

    def test_indented_rules(self):
        md = """## Vocabulary
  - /foo/ → bar
"""
        config = parse_identity_config(md)
        assert len(config.vocab_rules) == 1


# ── Category 10: Processing order ───────────────────────────


class TestProcessingOrder:
    """Verify extraction happens in the documented order:
    emotions → tools → delegates → hallucination strip → whitespace → vocab."""

    def test_tags_extracted_before_vocab(self):
        """Vocab rules should NOT see raw tag text."""
        identity = IdentityConfig(
            vocab_rules=[
                VocabRule(pattern=re.compile(r"EMOTION"), replacement="OOPS"),
            ]
        )
        result = process("[EMOTION:joy] Hello.", identity)
        assert result.emotions == ["joy"]
        assert "OOPS" not in result.spoken_text

    def test_whitespace_cleanup_before_vocab(self):
        """Vocab sees single-spaced text, not double-spaced gaps from tag removal."""
        identity = IdentityConfig(
            vocab_rules=[
                VocabRule(pattern=re.compile(r"Hello world"), replacement="Hi earth"),
            ]
        )
        result = process("Hello [FAKE:x] world.", identity)
        assert result.spoken_text == "Hi earth."

    def test_hallucinated_stripped_after_known_tags(self):
        """Known tags extracted first, then unknown tags stripped."""
        result = process(
            "[EMOTION:joy] [UNKNOWN:x] Hello.",
            IdentityConfig(),
        )
        assert result.emotions == ["joy"]
        assert "[" not in result.spoken_text


# ── Category 11: Edge cases ─────────────────────────────────


class TestEdgeCases:
    def test_tag_only_sentence(self):
        """Sentence is nothing but a tag — spoken text is empty."""
        result = process("[EMOTION:joy]", IdentityConfig())
        assert result.spoken_text == ""
        assert result.emotions == ["joy"]

    def test_multiple_tags_only(self):
        result = process(
            '[EMOTION:joy] [TOOL:foo({"a": 1})] [DELEGATE:do stuff]',
            IdentityConfig(),
        )
        assert result.spoken_text == ""
        assert result.emotions == ["joy"]
        assert len(result.tool_requests) == 1
        assert len(result.delegate_requests) == 1

    def test_duplicate_emotions(self):
        """Same emotion tag twice — both instances kept."""
        result = process("[EMOTION:joy] [EMOTION:joy] Yay!", IdentityConfig())
        assert result.emotions == ["joy", "joy"]

    def test_uuid_uniqueness(self):
        """Each tool/delegate gets a unique ID."""
        result = process(
            '[TOOL:a({"x": 1})] [TOOL:b({"y": 2})]',
            IdentityConfig(),
        )
        ids = [r.request_id for r in result.tool_requests]
        assert len(set(ids)) == 2  # all unique

    def test_special_chars_in_spoken_text(self):
        result = process("Cost is $100 & tax (15%).", IdentityConfig())
        assert result.spoken_text == "Cost is $100 & tax (15%)."

    def test_newline_in_input(self):
        result = process("Line one.\nLine two.", IdentityConfig())
        assert "Line one." in result.spoken_text
        assert "Line two." in result.spoken_text

    def test_bracket_in_normal_text(self):
        """Square brackets that don't match tag patterns survive."""
        result = process("Array [0] is first.", IdentityConfig())
        assert result.spoken_text == "Array [0] is first."

    def test_partial_tag_survives(self):
        """Incomplete tag syntax is not matched by any regex."""
        result = process("See [EMOTION without closing.", IdentityConfig())
        assert "[EMOTION" in result.spoken_text
