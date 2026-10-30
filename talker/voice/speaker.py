"""Kokoro playback with conservative, completed-batch barge-in accounting.

Synthesis can run ahead of playback. Only a naturally completed playback batch
is acknowledged as spoken; an interrupted batch remains entirely unspoken.
This can undercount what the user heard within that batch. Audio-device latency
and exact word-level delivery are not measured by this wrapper.
"""

import asyncio
from dataclasses import dataclass

from RealtimeTTS import KokoroEngine, TextToAudioStream

from config import TalkerConfig


@dataclass(frozen=True)
class _PlaybackBatch:
    batch_id: int
    start: int
    end: int


class Speaker:
    """Queue spoken sentences and track completed playback batches.

    Methods run on the voice asyncio loop. RealtimeTTS callbacks only schedule
    work onto that loop, so playback threads never mutate accounting state.
    """

    def __init__(self, config: TalkerConfig) -> None:
        self._loop = asyncio.get_running_loop()
        self._engine = KokoroEngine(voice=config.kokoro_voice)
        self._stream = TextToAudioStream(self._engine)
        self._sentences: list[str] = []
        self._confirmed_count = 0
        self._queued_from = 0
        self._batch: _PlaybackBatch | None = None
        self._next_batch_id = 0

    def speak(self, text: str) -> None:
        """Queue a sentence without extending an already-running batch."""
        self._sentences.append(text)
        if self._batch is None:
            self._start_batch()

    def _start_batch(self) -> None:
        if self._queued_from == len(self._sentences):
            return
        self._next_batch_id += 1
        batch = _PlaybackBatch(
            self._next_batch_id, self._queued_from, len(self._sentences)
        )
        self._batch = batch
        self._queued_from = batch.end
        self._stream.on_audio_stream_stop = lambda: self._loop.call_soon_threadsafe(
            self._finish_batch, batch
        )
        self._stream.feed(" ".join(self._sentences[batch.start : batch.end]))
        self._stream.play_async()

    def _finish_batch(self, batch: _PlaybackBatch) -> None:
        if self._batch != batch:
            return
        succeeded = not self._stream.error_flag
        # The callback fires just before the playback thread exits. Join it and
        # reset the text iterator before feeding another batch. This runs on the
        # asyncio loop, never on the playback thread itself.
        self._stream.stop()
        self._batch = None
        if succeeded and self._confirmed_count == batch.start:
            self._confirmed_count = batch.end
        self._start_batch()

    def stop(self) -> tuple[str, str]:
        """Interrupt playback and split at the last completed batch boundary."""
        # Invalidate first: stop() also invokes the completion callback, which
        # must not acknowledge the interrupted batch or any queued sentences.
        self._batch = None
        self._queued_from = len(self._sentences)
        self._stream.stop()
        return (
            " ".join(self._sentences[: self._confirmed_count]),
            " ".join(self._sentences[self._confirmed_count :]),
        )

    def reset(self) -> None:
        """Stop playback and clear accounting for a new generation."""
        self.stop()
        self._sentences.clear()
        self._confirmed_count = 0
        self._queued_from = 0
