"""voice/turn_detection.py — Rule-based end-of-turn detection.

Phase 1 logic (~50-100 lines). Two thresholds (both configurable):
  - SYNTAX_SILENCE_THRESHOLD_MS (default 300ms): entry point to the ambiguous zone.
    Below this, always wait — we are inside normal clause-boundary pause territory.
    At or above this, syntactic shape becomes a useful discriminator.
  - SILENCE_THRESHOLD_MS (default 800ms): unconditional emit regardless of syntax.

Logic (see REF-004):
    silence < 300ms          → wait
    300ms ≤ silence < 800ms  → check syntax:
                                 complete → emit final_transcript
                                 incomplete → wait
    silence ≥ 800ms          → emit unconditionally

Known limitation: slow speakers who pause > 800ms mid-sentence receive a
final_transcript for the partial utterance. This is a Phase 1 limitation;
Phase 2 replaces this with the LiveKit learned turn detection model (see REF-004).
"""

import re
import time

from config import TalkerConfig

# Patterns that signal syntactic completeness.
_TERMINAL_PUNCT = re.compile(r"[.?!]\s*$")

# Patterns that signal the utterance is syntactically incomplete:
# dangling conjunctions, prepositions, articles at end of buffer.
_INCOMPLETE_ENDINGS = re.compile(
    r"\b(and|but|or|so|yet|the|a|an|of|in|on|at|to|for|with|by|from|that|which|who|"
    r"because|although|if|when|while|as|than|though|unless|until|since|after|before|"
    r"whether|nor)\s*$",
    re.IGNORECASE,
)


class TurnDetector:
    """Tracks partial transcript + silence duration; decides when to emit final_transcript.

    Silence is measured from the VAD stop event (not from the last partial update),
    so the 300ms/800ms thresholds apply to actual post-speech silence.

    Designed to be called from the RealtimeSTT callback thread.
    Returns a final transcript string (to be enqueued as ListenerEvent) or None.
    """

    def __init__(self, config: TalkerConfig) -> None:
        self._syntax_ms = config.syntax_silence_threshold_ms
        self._silence_ms = config.silence_threshold_ms
        self._buffer: str = ""
        self._vad_stop_ts: float | None = None  # set by on_vad_stop
        self._emitted: bool = False

    @property
    def has_emitted(self) -> bool:
        """Returns True if a final_transcript has already been emitted this turn."""
        return self._emitted

    def on_speech_start(self) -> None:
        """Called on vad_speech_start. Resets state for a new utterance."""
        self._buffer = ""
        self._vad_stop_ts = None
        self._emitted = False

    def on_vad_stop(self) -> None:
        """Called on vad_speech_end. Marks when silence began."""
        self._vad_stop_ts = time.monotonic()

    def on_partial(self, text: str) -> str | None:
        """Called on each partial transcript update.

        Updates the buffer and checks whether to emit a final_transcript.
        Only evaluates thresholds once VAD has stopped (we have a stop timestamp).

        Args:
            text: Accumulated partial transcript text so far.

        Returns:
            The text to emit as final_transcript, or None if we should keep waiting.
        """
        if self._emitted:
            return None
        self._buffer = text.strip()
        if self._vad_stop_ts is None:
            return None  # VAD still active — never emit mid-speech.
        return self._evaluate(self._silence_duration_ms())

    def on_silence_tick(self) -> str | None:
        """Called periodically (~100ms) while VAD is silent.

        Triggers the unconditional 800ms emit when partial updates have stopped
        but VAD silence has exceeded the threshold.

        Returns:
            The text to emit as final_transcript, or None.
        """
        if self._emitted or not self._buffer or self._vad_stop_ts is None:
            return None
        return self._evaluate(self._silence_duration_ms())

    # ──────────────────────────────────────────
    # Internal
    # ──────────────────────────────────────────

    def _evaluate(self, silence_ms: float) -> str | None:
        if silence_ms < self._syntax_ms:
            # Firmly inside normal clause-boundary pause territory — always wait.
            return None

        if silence_ms < self._silence_ms:
            # Ambiguous zone: use syntactic shape as discriminator.
            if self._is_syntactically_complete(self._buffer):
                return self._emit()
            return None  # Incomplete — keep waiting.

        # Unconditional emit at or beyond SILENCE_THRESHOLD_MS.
        return self._emit()

    def _emit(self) -> str | None:
        if self._emitted or not self._buffer:
            return None
        self._emitted = True
        return self._buffer

    def _silence_duration_ms(self) -> float:
        if self._vad_stop_ts is None:
            return 0.0
        return (time.monotonic() - self._vad_stop_ts) * 1000

    @staticmethod
    def _is_syntactically_complete(text: str) -> bool:
        """Return True if text appears to be a complete utterance."""
        if not text:
            return False
        if _INCOMPLETE_ENDINGS.search(text):
            return False
        if _TERMINAL_PUNCT.search(text):
            return True
        return False
