"""Tests for inference/sentence_detector.py.

Organized by failure category so a failing test name immediately tells you
*what class* of edge case regressed.
"""

from inference.sentence_detector import SentenceDetector


# ── Helpers ──────────────────────────────────────────────────

def _feed_all(text: str) -> list[str]:
    """Feed an entire string char-by-char and return all emitted sentences
    (including flush)."""
    sd = SentenceDetector()
    sentences: list[str] = []
    for ch in text:
        r = sd.feed(ch)
        if r is not None:
            sentences.append(r)
    remainder = sd.flush()
    if remainder:
        sentences.append(remainder)
    return sentences


def _feed_tokens(tokens: list[str]) -> list[str]:
    """Feed a list of token strings and return all emitted sentences
    (including flush)."""
    sd = SentenceDetector()
    sentences: list[str] = []
    for token in tokens:
        r = sd.feed(token)
        if r is not None:
            sentences.append(r)
    remainder = sd.flush()
    if remainder:
        sentences.append(remainder)
    return sentences


# ── Category 1: Basic boundary detection ─────────────────────

class TestBasicBoundaries:
    """Core sentence splitting on `.` `?` `!` followed by whitespace + uppercase."""

    def test_period_boundary(self):
        sentences = _feed_all("Hello there. How are you?")
        assert sentences[0] == "Hello there."

    def test_question_mark_boundary(self):
        sentences = _feed_all("Are you sure? Yes I am.")
        assert sentences[0] == "Are you sure?"

    def test_exclamation_boundary(self):
        sentences = _feed_all("That's great! Tell me more.")
        assert sentences[0] == "That's great!"

    def test_two_sentences(self):
        sentences = _feed_all("First. Second.")
        assert sentences == ["First.", "Second."]

    def test_three_sentences(self):
        sentences = _feed_all("One. Two. Three.")
        assert len(sentences) == 3
        assert sentences[0] == "One."
        assert sentences[1] == "Two."
        assert sentences[2] == "Three."

    def test_mixed_punctuation(self):
        sentences = _feed_all("Really? Yes! Okay.")
        assert sentences == ["Really?", "Yes!", "Okay."]


# ── Category 2: flush() behavior ────────────────────────────

class TestFlush:
    """flush() emits whatever remains when the LLM generation ends."""

    def test_flush_emits_remainder(self):
        sd = SentenceDetector()
        sd.feed("Hello there")
        assert sd.flush() == "Hello there"

    def test_flush_empty_returns_none(self):
        sd = SentenceDetector()
        assert sd.flush() is None

    def test_flush_whitespace_only_returns_none(self):
        sd = SentenceDetector()
        sd.feed("   ")
        assert sd.flush() is None

    def test_flush_strips_whitespace(self):
        sd = SentenceDetector()
        sd.feed("  hello  ")
        assert sd.flush() == "hello"

    def test_flush_after_complete_sentence(self):
        """If buffer has a trailing sentence with no successor, flush emits it."""
        sd = SentenceDetector()
        sd.feed("First. ")
        # No uppercase follows — nothing emitted yet.
        assert sd.flush() == "First."

    def test_double_flush_returns_none(self):
        sd = SentenceDetector()
        sd.feed("Hello")
        sd.flush()
        assert sd.flush() is None


# ── Category 3: Abbreviations (false-positive suppression) ──

class TestAbbreviations:
    """Abbreviations with periods must NOT trigger sentence boundaries."""

    def test_dr(self):
        sentences = _feed_all("Dr. Smith is here. He said hello.")
        assert sentences[0] == "Dr. Smith is here."

    def test_mr(self):
        sentences = _feed_all("Mr. Jones left. She stayed.")
        assert sentences[0] == "Mr. Jones left."

    def test_mrs(self):
        sentences = _feed_all("Mrs. Adams called. Nobody answered.")
        assert sentences[0] == "Mrs. Adams called."

    def test_ms(self):
        sentences = _feed_all("Ms. Taylor arrived. We greeted her.")
        assert sentences[0] == "Ms. Taylor arrived."

    def test_prof(self):
        sentences = _feed_all("Prof. Lee teaches math. Students love it.")
        assert sentences[0] == "Prof. Lee teaches math."

    def test_jr(self):
        sentences = _feed_all("Robert Jr. Was there. He spoke.")
        # "Jr." should not split, but ". W" after "Jr." would match _BOUNDARY.
        # The abbreviation guard should suppress it.
        assert "Jr." in sentences[0]

    def test_vs(self):
        sentences = _feed_all("It was us vs. Them in the game. We won.")
        assert "vs." in sentences[0]

    def test_etc(self):
        sentences = _feed_all("Cats, dogs, etc. Animals are great.")
        assert "etc." in sentences[0]

    def test_govt(self):
        sentences = _feed_all("The govt. Agency sent a letter. It arrived.")
        assert "govt." in sentences[0]

    def test_inc(self):
        sentences = _feed_all("Acme Inc. Sells widgets. They expanded.")
        assert "Inc." in sentences[0]

    def test_corp(self):
        sentences = _feed_all("Mega Corp. Hired new staff. Growth is fast.")
        assert "Corp." in sentences[0]

    def test_fig(self):
        sentences = _feed_all("See fig. Three for details. It shows the data.")
        assert "fig." in sentences[0]

    def test_vol(self):
        sentences = _feed_all("Published in vol. Five of the journal. It was cited.")
        assert "vol." in sentences[0]

    def test_gen(self):
        sentences = _feed_all("Gen. Patton led the army. Victory followed.")
        assert "Gen." in sentences[0]

    def test_no(self):
        sentences = _feed_all("Item no. Five is missing. Please check.")
        assert "no." in sentences[0]

    def test_ch(self):
        sentences = _feed_all("Read ch. Seven tonight. We discuss tomorrow.")
        assert "ch." in sentences[0]

    def test_sec(self):
        sentences = _feed_all("See sec. Four for details. It covers the topic.")
        assert "sec." in sentences[0]

    def test_approx(self):
        sentences = _feed_all("The value is approx. Twelve units. That is close.")
        assert "approx." in sentences[0]

    def test_multiple_abbreviations_in_one_sentence(self):
        sentences = _feed_all("Dr. Smith and Prof. Lee met. They talked.")
        assert sentences[0] == "Dr. Smith and Prof. Lee met."

    def test_abbreviation_at_real_boundary(self):
        """Abbreviation at end of a real sentence — the period serves double duty.
        The abbreviation suppresses the split, so flush must emit."""
        sentences = _feed_all("He met Dr. Smith.")
        # Only one sentence, emitted by flush.
        assert len(sentences) == 1
        assert sentences[0] == "He met Dr. Smith."


# ── Category 4: Initials and dotted acronyms ────────────────

class TestInitialsAndAcronyms:
    """Single-capital-letter abbreviations: U.S., initials like J. K. Rowling."""

    def test_us_acronym(self):
        sentences = _feed_all("The U.S. Army deployed. Forces moved out.")
        assert "U.S." in sentences[0]

    def test_single_initial(self):
        sentences = _feed_all("J. Smith is here. He arrived early.")
        assert "J. Smith" in sentences[0]

    def test_double_initials(self):
        sentences = _feed_all("J. K. Rowling wrote it. The book sold well.")
        assert "J. K. Rowling" in sentences[0]

    def test_initial_mid_sentence(self):
        sentences = _feed_all("I met A. Lincoln at the event. He was tall.")
        assert "A. Lincoln" in sentences[0]


# ── Category 5: Decimal numbers ─────────────────────────────

class TestDecimals:
    """Decimal periods (3.14) never trigger _BOUNDARY because the char after
    the period is a digit, not whitespace — structural impossibility."""

    def test_simple_decimal(self):
        sentences = _feed_all("Pi is 3.14 roughly. That is enough.")
        assert "3.14" in sentences[0]

    def test_decimal_at_end(self):
        sentences = _feed_all("The result is 2.718.")
        assert sentences == ["The result is 2.718."]

    def test_multiple_decimals(self):
        sentences = _feed_all("Values are 1.5 and 2.7 units. More data follows.")
        assert "1.5" in sentences[0]
        assert "2.7" in sentences[0]

    def test_version_number(self):
        sentences = _feed_all("Use version 2.0.1 please. It has fixes.")
        assert "2.0.1" in sentences[0]

    def test_money_amount(self):
        sentences = _feed_all("It costs $9.99 total. Please pay now.")
        assert "$9.99" in sentences[0]

    def test_percentage(self):
        sentences = _feed_all("Growth was 3.5% this quarter. Investors rejoiced.")
        assert "3.5%" in sentences[0]

    def test_decimal_with_comma_thousands(self):
        """1,234.56 — comma in the integer part, period as decimal separator."""
        sentences = _feed_all("Revenue hit 1,234.56 million. Records were set.")
        assert "1,234.56" in sentences[0]

    def test_negative_decimal(self):
        sentences = _feed_all("Temperature dropped to -3.5 degrees. It was cold.")
        assert "-3.5" in sentences[0]


# ── Category 6: URLs ────────────────────────────────────────

class TestURLs:
    """URLs with periods must NOT trigger sentence boundaries."""

    def test_http_url(self):
        sentences = _feed_all("Visit http://example.com for info. It is free.")
        assert "http://example.com" in sentences[0]

    def test_https_url(self):
        sentences = _feed_all("See https://docs.python.org for details. Start there.")
        assert "https://docs.python.org" in sentences[0]

    def test_url_with_path(self):
        sentences = _feed_all("Go to https://api.example.com/v1/users for docs. It helps.")
        assert "https://api.example.com/v1/users" in sentences[0]

    def test_url_mid_sentence(self):
        sentences = _feed_all("The site https://foo.bar.com hosts data. Check it out.")
        assert "https://foo.bar.com" in sentences[0]


# ── Category 7: Quoted speech and closing punctuation ───────

class TestQuotedSpeech:
    """Punctuation inside quotes/parens: the boundary regex allows optional
    closing quotes/parens between punctuation and whitespace."""

    def test_double_quoted_sentence(self):
        sentences = _feed_all('She said "Hello." Then she left.')
        assert len(sentences) == 2
        assert 'Hello."' in sentences[0]

    def test_single_quoted_sentence(self):
        sentences = _feed_all("He said 'Goodbye.' And he walked away.")
        assert len(sentences) == 2
        assert "Goodbye.'" in sentences[0]

    def test_smart_double_quote(self):
        sentences = _feed_all("She whispered \u201cRun.\u201d Then she hid.")
        assert len(sentences) == 2

    def test_smart_single_quote(self):
        sentences = _feed_all("He said \u2018Stop.\u2019 Nobody listened.")
        assert len(sentences) == 2

    def test_parenthetical_period(self):
        sentences = _feed_all("It ended (finally.) The crowd cheered.")
        assert len(sentences) == 2
        assert "finally.)" in sentences[0]

    def test_question_in_quotes(self):
        sentences = _feed_all('She asked "Really?" He nodded.')
        assert len(sentences) == 2
        assert 'Really?"' in sentences[0]

    def test_exclamation_in_quotes(self):
        sentences = _feed_all('He yelled "Run!" She bolted.')
        assert len(sentences) == 2
        assert 'Run!"' in sentences[0]


# ── Category 8: Ellipsis ────────────────────────────────────

class TestEllipsis:
    """Ellipsis (...) — only the last dot could trigger _BOUNDARY, and only
    if followed by whitespace + uppercase."""

    def test_ellipsis_mid_sentence(self):
        """Trailing ellipsis with no following uppercase — no split, flush emits."""
        sentences = _feed_all("I was thinking...")
        assert sentences == ["I was thinking..."]

    def test_ellipsis_then_new_sentence(self):
        """Ellipsis followed by space + uppercase — the last dot triggers."""
        sentences = _feed_all("I was thinking... Maybe we should go.")
        assert len(sentences) == 2
        assert sentences[0] == "I was thinking..."
        assert sentences[1] == "Maybe we should go."

    def test_ellipsis_then_lowercase(self):
        """Ellipsis followed by lowercase — no boundary (common in casual text)."""
        sentences = _feed_all("So... yeah that happened.")
        assert len(sentences) == 1

    def test_unicode_ellipsis(self):
        """Unicode ellipsis character (…) is NOT a period — no split."""
        sentences = _feed_all("Hmm\u2026 Well that happened.")
        # \u2026 is not in [.?!], so no boundary detected.
        assert len(sentences) == 1


# ── Category 9: No boundary (lowercase after punctuation) ───

class TestNoBoundaryLowercase:
    """_BOUNDARY requires uppercase after whitespace. Lowercase → no split."""

    def test_period_then_lowercase(self):
        sentences = _feed_all("end. but not a new sentence.")
        assert len(sentences) == 1

    def test_question_then_lowercase(self):
        sentences = _feed_all("why? because reasons.")
        assert len(sentences) == 1

    def test_exclamation_then_lowercase(self):
        sentences = _feed_all("wow! that was cool.")
        assert len(sentences) == 1


# ── Category 10: Token-by-token streaming fidelity ──────────

class TestTokenStreaming:
    """Verify correct behavior when input arrives as LLM-like token chunks,
    not character-by-character."""

    def test_boundary_split_across_tokens(self):
        """The period and the next uppercase arrive in different tokens."""
        sentences = _feed_tokens(["Hello.", " ", "World."])
        assert sentences[0] == "Hello."

    def test_sentence_in_single_token(self):
        """Two sentences in one token — first should be emitted."""
        sd = SentenceDetector()
        result = sd.feed("Done. Next.")
        assert result == "Done."
        remainder = sd.flush()
        assert remainder == "Next."

    def test_punctuation_and_space_same_token(self):
        sentences = _feed_tokens(["First. ", "Second."])
        # "First. " has no uppercase yet — nothing emitted until "S" arrives.
        assert sentences[0] == "First."
        assert sentences[1] == "Second."

    def test_gradual_token_buildup(self):
        """Simulate realistic multi-char token stream."""
        sentences = _feed_tokens([
            "The ", "cat ", "sat. ", "The ", "dog ", "ran."
        ])
        assert sentences[0] == "The cat sat."
        assert sentences[1] == "The dog ran."

    def test_single_char_tokens(self):
        """Worst case: every character is a separate token."""
        text = "Hi. Go."
        sentences = _feed_tokens(list(text))
        assert sentences == ["Hi.", "Go."]


# ── Category 11: Edge cases and unusual inputs ──────────────

class TestEdgeCases:

    def test_empty_string_token(self):
        sd = SentenceDetector()
        assert sd.feed("") is None
        sd.feed("Hello.")
        assert sd.flush() == "Hello."

    def test_only_punctuation(self):
        sentences = _feed_all("...")
        assert sentences == ["..."]

    def test_only_whitespace_tokens(self):
        sd = SentenceDetector()
        sd.feed("   ")
        sd.feed("  ")
        assert sd.flush() is None

    def test_newline_as_whitespace(self):
        """Newline counts as whitespace in \\s+ — boundary fires if uppercase follows."""
        sentences = _feed_all("Done.\nNext.")
        assert sentences[0] == "Done."

    def test_tab_as_whitespace(self):
        sentences = _feed_all("Done.\tNext.")
        assert sentences[0] == "Done."

    def test_multiple_spaces_between_sentences(self):
        sentences = _feed_all("First.   Second.")
        assert sentences[0] == "First."

    def test_no_space_between_sentences(self):
        """No whitespace between sentences → no boundary (requires \\s+)."""
        sentences = _feed_all("First.Second.")
        assert len(sentences) == 1  # no split, flushed as one

    def test_number_after_period(self):
        """Digit after period → no boundary (not uppercase)."""
        sentences = _feed_all("Step 1. 2 comes next.")
        # "1. 2" — '2' is not uppercase, so no split here.
        assert len(sentences) == 1

    def test_all_caps_sentence(self):
        sentences = _feed_all("STOP. GO NOW.")
        assert sentences[0] == "STOP."

    def test_emoji_after_period(self):
        """Emoji is not uppercase — no boundary."""
        sentences = _feed_all("Done. 😊 Great.")
        # The "😊" after "Done. " is not [A-Z], so no split at first period.
        # But "Great" IS uppercase — does ". 😊 Great" match _BOUNDARY?
        # _BOUNDARY is [.?!] \s+ (?=[A-Z]) — the lookahead is for the NEXT char
        # after whitespace. "😊 " is not whitespace, so it depends on exact position.
        # Actually: "Done. 😊 Great." — after "Done." there's " 😊 " which is
        # space + emoji + space, then "G". The regex \s+ matches " " then (?=[A-Z])
        # would need to lookahead at "😊" which is not [A-Z]. So no split at "Done."
        # "Great." ends the string — flush emits.
        assert len(sentences) == 1

    def test_sentence_with_numbers_and_punctuation(self):
        sentences = _feed_all("I scored 98.5% on the test. That was great.")
        assert "98.5%" in sentences[0]

    def test_abbreviation_then_real_boundary(self):
        """Two periods: abbreviation period then sentence-ending period."""
        sentences = _feed_all("Talk to Dr. Smith about it. He knows the answer.")
        assert sentences[0] == "Talk to Dr. Smith about it."

    def test_consecutive_abbreviations(self):
        """Multiple abbreviations in a row."""
        sentences = _feed_all("Gen. Lt. Harris reported in. The briefing began.")
        assert "Gen." in sentences[0]
        assert "Lt." in sentences[0]
        assert "Harris reported in." in sentences[0]

    def test_very_long_sentence(self):
        """Stress test: a long sentence followed by a short one."""
        long_part = "Word " * 200 + "end."
        text = long_part + " Next."
        sentences = _feed_all(text)
        assert len(sentences) == 2
        assert sentences[0].endswith("end.")
        assert sentences[1] == "Next."

    def test_interleaved_feed_and_flush(self):
        """Reuse detector across multiple generations."""
        sd = SentenceDetector()
        sd.feed("First gen.")
        assert sd.flush() == "First gen."

        # Second generation — detector is reusable after flush.
        sd.feed("Second gen.")
        assert sd.flush() == "Second gen."

    def test_mid_word_period_no_split(self):
        """File extension in prose — no whitespace after period."""
        sentences = _feed_all("Open main.py to edit. Then run it.")
        assert "main.py" in sentences[0]

    def test_ip_address(self):
        """IP addresses have periods but digits follow them."""
        sentences = _feed_all("Connect to 192.168.1.1 now. It is ready.")
        assert "192.168.1.1" in sentences[0]

    def test_time_with_period(self):
        """Some locales use period for time: 3.30 PM."""
        sentences = _feed_all("Meet at 3.30 PM please. Don't be late.")
        assert "3.30" in sentences[0]
