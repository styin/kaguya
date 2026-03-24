"""prepare.py — PREPARE signal handler crossing voice + inference concerns.

Domain role: Handles the Gateway's Prepare RPC by cancelling any in-flight
LLM generation and TTS playback, then returning a PrepareAck with the
spoken/unspoken text split.

Idempotent: if the Talker is already idle, returns empty PrepareAck immediately.
"""

from __future__ import annotations

import asyncio

from proto import kaguya_pb2  # type: ignore[import]

from voice.speaker import Speaker


class PrepareHandler:
    """Coordinates PREPARE across inference cancellation and TTS interruption.

    Usage:
        handler = PrepareHandler(cancel_event, speaker)
        ack = handler.handle()
    """

    def __init__(
        self,
        cancel_event: asyncio.Event,
        speaker: Speaker,
    ) -> None:
        self._cancel_event = cancel_event
        self._speaker = speaker

    def handle(self) -> kaguya_pb2.PrepareAck:
        """Execute the PREPARE signal.

        1. Set the cancel event (checked by ProcessPrompt between tokens,
           between sentences, and inside the SSE parser).
        2. Stop TTS playback and get spoken/unspoken split.
        3. Return PrepareAck.

        If already idle (cancel event not needed, speaker not playing),
        returns PrepareAck with empty strings.
        """
        # Signal inference cancellation.
        self._cancel_event.set()

        # Stop TTS and get the split. Returns ("", "") if idle.
        spoken, unspoken = self._speaker.stop()

        return kaguya_pb2.PrepareAck(
            spoken_text=spoken,
            unspoken_text=unspoken,
        )
