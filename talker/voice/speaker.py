"""voice/speaker.py — RealtimeTTS wrapper with Kokoro engine.

Domain role: Receives spoken sentences (tags already stripped by soul container),
synthesizes audio via Kokoro TTS, and tracks sentence-level playback for PREPARE
token accounting.

Spoken/unspoken split strategy (conservative undercounting):
    We track how many sentences have been synthesized via the on_sentence_synthesized
    callback. Since RealtimeTTS plays sentences sequentially, if sentence N+1 has
    been synthesized, sentence N has *at minimum* finished playing. The last
    synthesized sentence may or may not have finished playing — we conservatively
    count it as unspoken.

    This means we may undercount spoken text by up to one sentence. This is the
    safe direction: undercounting means Gateway discards some text the user heard,
    rather than putting unheard text into history (which would confuse the LLM).

    Phase 2: Use RealtimeTTS's on_word callback (supported by KokoroEngine for
    English voices) for exact word-level playback tracking.
"""

import logging

from RealtimeTTS import TextToAudioStream, KokoroEngine

from config import TalkerConfig

logger = logging.getLogger(__name__)


class Speaker:
    """Wraps RealtimeTTS with Kokoro for sentence-level TTS streaming.

    Lifecycle:
        speaker = Speaker(config)
        speaker.speak("Hello there.")  # non-blocking, queues to TTS
        spoken, unspoken = speaker.stop()  # interrupt + return split
        speaker.reset()  # prepare for next turn
    """

    def __init__(self, config: TalkerConfig) -> None:
        self._engine = KokoroEngine(voice=config.kokoro_voice)
        self._stream = TextToAudioStream(self._engine)
        self._sentences: list[str] = []
        self._synthesized_count: int = 0
        self._playing: bool = False

    def _on_sentence_synthesized(self) -> None:
        """Callback from RealtimeTTS when a sentence finishes synthesis."""
        self._synthesized_count += 1

    def speak(self, text: str) -> None:
        """Feed one sentence to TTS for synthesis and playback.

        Non-blocking. The sentence is queued; TTS streams audio as it generates.
        """
        self._sentences.append(text)
        self._stream.feed(text)
        if not self._playing:
            self._stream.play_async(
                on_sentence_synthesized=self._on_sentence_synthesized,
            )
            self._playing = True

    def stop(self) -> tuple[str, str]:
        """Interrupt playback and return (spoken_text, unspoken_text).

        Uses conservative sentence-level accounting: a sentence is only counted
        as spoken if the *next* sentence has already been synthesized (proving
        the earlier one finished playing). The last synthesized sentence is
        always counted as unspoken.

        This may undercount spoken text by up to one sentence. See module
        docstring for rationale.

        If idle, returns ("", "").
        """
        if not self._playing or not self._sentences:
            return ("", "")

        self._stream.stop()
        self._playing = False

        # Conservative split: only sentences before the last synthesized one
        # are guaranteed to have finished playing.
        confirmed_played = max(0, self._synthesized_count - 1)

        spoken = " ".join(self._sentences[:confirmed_played])
        unspoken = " ".join(self._sentences[confirmed_played:])
        return (spoken, unspoken)

    def reset(self) -> None:
        """Reset state for a new turn (after PREPARE or ResponseComplete)."""
        if self._playing:
            self._stream.stop()
            self._playing = False
        self._sentences.clear()
        self._synthesized_count = 0
