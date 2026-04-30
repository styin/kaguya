"""prepare.py — PREPARE logic, now called inline from Converse barge-in handler.

No longer a standalone RPC handler. The Speaker.stop() + cancel_event.set()
logic is inlined in server.py's Converse handler. This module is retained
for the PrepareHandler class in case it's useful elsewhere.
"""

from __future__ import annotations

import asyncio

from proto import kaguya_pb2  # type: ignore[import]
from voice.speaker import Speaker


class PrepareHandler:
    def __init__(self, cancel_event: asyncio.Event, speaker: Speaker) -> None:
        self._cancel_event = cancel_event
        self._speaker = speaker

    def handle(self) -> kaguya_pb2.BargeInAck:
        self._cancel_event.set()
        spoken, unspoken = self._speaker.stop()
        return kaguya_pb2.BargeInAck(
            spoken_text=spoken, unspoken_text=unspoken,
        )