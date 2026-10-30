"""Raw audio asserts the startup scope before a frame reaches the recorder."""

import asyncio
import struct
import uuid
from unittest.mock import AsyncMock, MagicMock

import pytest

from binding import AUDIO_MAGIC, InstanceSessionBinding
from config import TalkerConfig
from voice.listener import Listener

INSTANCE_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
SESSION_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"


@pytest.mark.asyncio
@pytest.mark.parametrize("matching", [True, False])
async def test_audio_checks_preconfigured_scope_before_consuming_pcm(
    monkeypatch, matching
):
    monkeypatch.setattr("voice.listener.OpusDecoder", MagicMock())
    listener = Listener(
        TalkerConfig(audio_input_codec="pcm"),
        InstanceSessionBinding(INSTANCE_ID, SESSION_ID),
    )
    listener._recorder = MagicMock()
    session_id = SESSION_ID if matching else str(uuid.uuid4())
    preamble = AUDIO_MAGIC + uuid.UUID(INSTANCE_ID).bytes + uuid.UUID(session_id).bytes
    pcm = b"\x00\x01\x02\x03"
    reader = asyncio.StreamReader()
    reader.feed_data(
        preamble + struct.pack(">I", len(pcm)) + pcm + struct.pack(">I", 0)
    )
    reader.feed_eof()
    writer = MagicMock()
    writer.wait_closed = AsyncMock()

    await listener._handle_audio_client(reader, writer)

    if matching:
        listener._recorder.feed_audio.assert_called_once_with(pcm)
    else:
        listener._recorder.feed_audio.assert_not_called()
        writer.close.assert_called_once()
        writer.wait_closed.assert_awaited_once()
