"""Streaming failures must not become successful, truncated generations."""

import asyncio
from unittest.mock import MagicMock

import httpx
import pytest

from config import TalkerConfig
from inference.llm_client import LLMClient
from proto import kaguya_pb2
from server import TalkerServiceServicer


class FailingStream(httpx.AsyncByteStream):
    async def __aiter__(self):
        yield b'data: {"choices":[{"text":"First sentence. Next","finish_reason":null}]}\n\n'
        raise httpx.ReadError("connection lost")


@pytest.mark.asyncio
async def test_partial_stream_failure_is_propagated_without_retry():
    attempts = 0

    def respond(request):
        nonlocal attempts
        attempts += 1
        return httpx.Response(200, stream=FailingStream())

    client = LLMClient(TalkerConfig())
    await client.close()
    client._client = httpx.AsyncClient(
        base_url="http://llm.test", transport=httpx.MockTransport(respond)
    )
    tokens = []
    try:
        with pytest.raises(ConnectionError, match="after yielding tokens"):
            async for token in client.stream_completion("prompt"):
                tokens.append(token)
    finally:
        await client.close()
    assert tokens == ["First sentence. Next"]
    assert attempts == 1


@pytest.mark.asyncio
@pytest.mark.parametrize("interruption", ["failure", "barge_in"])
async def test_interrupted_stream_does_not_flush_unspoken_remainder(
    binding, session_ref, interruption
):
    speaker = MagicMock()
    service = TalkerServiceServicer(TalkerConfig(), speaker, binding)
    cancel_event = asyncio.Event()

    async def completion(prompt, cancel):
        yield "First sentence. Unspoken remainder"
        if interruption == "failure":
            raise ConnectionError("stream failed")
        cancel.set()

    service._llm.stream_completion = completion
    queue = asyncio.Queue()
    try:
        await service._run_generation(
            kaguya_pb2.GenerationStart(
                session=session_ref,
                generation_id="interrupted-generation",
                prompt_context=kaguya_pb2.PromptContext(),
                user_input=kaguya_pb2.UserInput(text="Hello"),
            ),
            queue,
            cancel_event,
        )
    finally:
        await service.close()
    outputs = [queue.get_nowait() for _ in range(queue.qsize())]
    speaker.speak.assert_called_once_with("First sentence.")
    assert outputs[-1].response_complete.was_interrupted
