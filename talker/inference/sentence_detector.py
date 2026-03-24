"""inference/sentence_detector.py — Token buffer to sentence boundary detection.

Accumulates tokens from the LLM stream and yields complete sentences at
boundary points. Regex-based detection with false-positive suppression
for abbreviations and URLs.

Design principle (implementation plan §2.3): Sentence is the streaming
granularity. The soul container processes complete sentences, never
individual tokens. This matches Kokoro's minimum stable synthesis window.

Limitation: _BOUNDARY requires whitespace + uppercase after punctuation,
so it only detects boundaries *between* sentences. The final sentence in
a generation has no following text — flush() handles that case.

Phase 2: Replace regex heuristics with a proper sentence segmentation model
(e.g., PySBD, spaCy sentencizer) or leverage server-side sentence chunking
once we migrate to /v1/chat/completions.
"""

import re


# Abbreviations that should NOT trigger a sentence boundary.
_ABBREVS = re.compile(
    r"\b(?:Dr|Mr|Mrs|Ms|Prof|Sr|Jr|vs|etc|approx|dept|est|govt"
    r"|inc|corp|ltd|assn|bros|vol|rev|gen|sgt|cpl|pvt|capt"
    r"|cmdr|lt|col|maj"
    r"|fig|eq|no|nos|op|ch|sec|para"
    r"|[A-Z])\.\s*$",  # single capital letter: "U." in "U.S.", initials
    re.IGNORECASE,
)

# URL mid-sentence: "https://..." or "http://..."
_URL = re.compile(r"https?://\S*$", re.IGNORECASE)

# Sentence-ending punctuation followed by whitespace + uppercase.
# Only fires *between* sentences — the final sentence is handled by flush().
# Decimal periods (3.14) never trigger this because the character after the
# period is a digit, not whitespace — so no false-positive guard is needed.
_BOUNDARY = re.compile(r"([.?!][\"'\u201d\u2019)]*)\s+(?=[A-Z])")


class SentenceDetector:
    """Accumulates tokens and yields complete sentences.

    Usage:
        sd = SentenceDetector()
        for token in llm_stream:
            sentence = sd.feed(token)
            if sentence:
                process(sentence)
        # End of generation:
        remainder = sd.flush()
        if remainder:
            process(remainder)
    """

    def __init__(self) -> None:
        self._buffer: str = ""

    def feed(self, token: str) -> str | None:
        """Append a token and return a complete sentence if a boundary is found.

        Returns None if no boundary detected yet.
        """
        self._buffer += token

        # Scan all potential boundaries; use the first non-false-positive.
        for match in _BOUNDARY.finditer(self._buffer):
            candidate_end = match.end(
                1
            )  # position after punctuation + closing quotes/parens
            candidate = self._buffer[:candidate_end].rstrip()

            if self._is_false_positive(candidate):
                continue  # skip abbreviations, URLs

            # Emit the sentence; keep the remainder in the buffer.
            self._buffer = self._buffer[match.end() :]
            return candidate

        return None

    def flush(self) -> str | None:
        """Force-emit whatever remains in the buffer (end of generation).

        Returns None if buffer is empty.
        """
        text = self._buffer.strip()
        self._buffer = ""
        return text or None

    @staticmethod
    def _is_false_positive(text: str) -> bool:
        """Return True if the boundary match is a known false positive."""
        if _ABBREVS.search(text):
            return True
        if _URL.search(text):
            return True
        return False
