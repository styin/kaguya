"""Playback callbacks acknowledge only completed, uninterrupted batches."""

import asyncio
import importlib
import sys
from types import ModuleType
from unittest.mock import MagicMock

import pytest

from config import TalkerConfig


@pytest.fixture
def speaker_module(monkeypatch):
    tts = ModuleType("RealtimeTTS")
    tts.KokoroEngine = MagicMock()
    tts.TextToAudioStream = MagicMock()
    monkeypatch.setitem(sys.modules, "RealtimeTTS", tts)
    # Keep the inference-only suite independent of GPU/audio dependencies.
    sys.modules.pop("voice.speaker", None)
    module = importlib.import_module("voice.speaker")
    yield module
    sys.modules.pop("voice.speaker", None)


@pytest.mark.asyncio
async def test_synthesis_or_interrupted_stop_cannot_acknowledge_playback(
    speaker_module,
):
    speaker = speaker_module.Speaker(TalkerConfig())
    stream = speaker._stream
    stream.error_flag = False
    speaker.speak("First sentence.")
    speaker.speak("Queued sentence.")
    stream.stop.side_effect = lambda: stream.on_audio_stream_stop()

    assert speaker.stop() == ("", "First sentence. Queued sentence.")
    await asyncio.sleep(0)
    assert speaker._confirmed_count == 0
    stream.play_async.assert_called_once()


@pytest.mark.asyncio
async def test_completed_batch_excludes_future_queued_sentences(speaker_module):
    speaker = speaker_module.Speaker(TalkerConfig())
    stream = speaker._stream
    stream.error_flag = False
    speaker.speak("First sentence.")
    first_callback = stream.on_audio_stream_stop
    speaker.speak("Second sentence.")
    speaker.speak("Third sentence.")
    await asyncio.to_thread(first_callback)
    await asyncio.sleep(0)

    assert speaker._confirmed_count == 1
    assert stream.feed.call_args.args == ("Second sentence. Third sentence.",)
    assert speaker.stop() == ("First sentence.", "Second sentence. Third sentence.")


@pytest.mark.asyncio
async def test_completed_batches_advance_acknowledged_prefix(speaker_module):
    speaker = speaker_module.Speaker(TalkerConfig())
    stream = speaker._stream
    stream.error_flag = False
    speaker.speak("First sentence.")
    speaker.speak("Second sentence.")
    stream.on_audio_stream_stop()
    await asyncio.sleep(0)
    stream.on_audio_stream_stop()
    await asyncio.sleep(0)
    assert speaker.stop() == ("First sentence. Second sentence.", "")


@pytest.mark.asyncio
async def test_old_callback_cannot_confirm_new_generation(speaker_module):
    speaker = speaker_module.Speaker(TalkerConfig())
    stream = speaker._stream
    stream.error_flag = False
    speaker.speak("Old sentence.")
    old_callback = stream.on_audio_stream_stop
    speaker.reset()
    speaker.speak("New sentence.")
    old_callback()
    await asyncio.sleep(0)
    assert speaker.stop() == ("", "New sentence.")


@pytest.mark.asyncio
async def test_failed_playback_does_not_acknowledge_text(speaker_module):
    speaker = speaker_module.Speaker(TalkerConfig())
    stream = speaker._stream
    stream.error_flag = True
    speaker.speak("Failed sentence.")
    stream.on_audio_stream_stop()
    await asyncio.sleep(0)
    assert speaker.stop() == ("", "Failed sentence.")
