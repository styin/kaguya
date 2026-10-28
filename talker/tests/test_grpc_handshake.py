"""Stream readiness follows validation of the prequeued first message.

Clients enqueue init/start before waiting for response headers. The server must
then flush those headers before waiting on ASR/inference, and invalid or duplicate
initialization must terminate promptly rather than leaving a background task hung.
"""

import asyncio
import uuid
from unittest.mock import MagicMock

import grpc
import pytest

from config import TalkerConfig
from proto import kaguya_pb2, kaguya_pb2_grpc
from server import TalkerServiceServicer
from voice.listener import ListenerServiceImpl


async def _start_server(register):
    server = grpc.aio.server()
    register(server)
    port = server.add_insecure_port("127.0.0.1:0")
    await server.start()
    return server, port


async def _listener_inputs(session):
    yield kaguya_pb2.ListenerInput(init=kaguya_pb2.ListenerInit(session=session))


def _generation(session):
    return kaguya_pb2.GenerationStart(
        session=session,
        generation_id="generation-1",
        prompt_context=kaguya_pb2.PromptContext(),
        user_input=kaguya_pb2.UserInput(text="Hello"),
    )


async def _blocked_completion(*args):
    await asyncio.Event().wait()
    if False:
        yield ""


@pytest.mark.asyncio
async def test_listener_headers_follow_valid_init_without_waiting_for_asr(
    binding, session_ref
):
    servicer = ListenerServiceImpl(asyncio.Queue(), binding)
    server, port = await _start_server(
        lambda server: kaguya_pb2_grpc.add_ListenerServiceServicer_to_server(
            servicer, server
        )
    )
    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:
            call = kaguya_pb2_grpc.ListenerServiceStub(channel).Stream(
                _listener_inputs(session_ref)
            )
            assert await asyncio.wait_for(call.initial_metadata(), 2) is not None
            call.cancel()
    finally:
        await server.stop(grace=0.5)


@pytest.mark.asyncio
async def test_talker_headers_follow_valid_start_without_waiting_for_inference(
    binding, session_ref
):
    servicer = TalkerServiceServicer(TalkerConfig(), MagicMock(), binding)
    servicer._llm.stream_completion = _blocked_completion
    server, port = await _start_server(
        lambda server: kaguya_pb2_grpc.add_TalkerServiceServicer_to_server(
            servicer, server
        )
    )
    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:

            async def inputs():
                yield kaguya_pb2.TalkerInput(start=_generation(session_ref))

            call = kaguya_pb2_grpc.TalkerServiceStub(channel).Converse(inputs())
            assert await asyncio.wait_for(call.initial_metadata(), 2) is not None
            call.cancel()
    finally:
        await servicer.close()
        await server.stop(grace=0.5)


@pytest.mark.asyncio
async def test_valid_listener_replacement_receives_all_future_events(
    binding, session_ref
):
    queue = asyncio.Queue()
    servicer = ListenerServiceImpl(queue, binding)
    server, port = await _start_server(
        lambda server: kaguya_pb2_grpc.add_ListenerServiceServicer_to_server(
            servicer, server
        )
    )
    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:
            stub = kaguya_pb2_grpc.ListenerServiceStub(channel)
            first = stub.Stream(_listener_inputs(session_ref))
            await asyncio.wait_for(first.initial_metadata(), 2)
            replacement = stub.Stream(_listener_inputs(session_ref))
            await asyncio.wait_for(replacement.initial_metadata(), 2)
            assert await asyncio.wait_for(first.read(), 2) == grpc.aio.EOF
            for i in range(3):
                queue.put_nowait(
                    kaguya_pb2.ListenerOutput(
                        partial_transcript=kaguya_pb2.PartialTranscript(
                            text=f"event-{i}"
                        )
                    )
                )
            observed = []
            for _ in range(3):
                observed.append(
                    (
                        await asyncio.wait_for(replacement.read(), 2)
                    ).partial_transcript.text
                )
            assert observed == ["event-0", "event-1", "event-2"]
            replacement.cancel()
    finally:
        await server.stop(grace=0.5)


@pytest.mark.asyncio
async def test_invalid_listener_does_not_take_over_live_connection(
    binding, session_ref, session_ref_factory
):
    queue = asyncio.Queue()
    servicer = ListenerServiceImpl(queue, binding)
    server, port = await _start_server(
        lambda server: kaguya_pb2_grpc.add_ListenerServiceServicer_to_server(
            servicer, server
        )
    )
    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:
            stub = kaguya_pb2_grpc.ListenerServiceStub(channel)
            live = stub.Stream(_listener_inputs(session_ref))
            await asyncio.wait_for(live.initial_metadata(), 2)
            wrong = session_ref_factory(session_id=str(uuid.uuid4()))
            with pytest.raises(grpc.aio.AioRpcError) as error:
                await stub.Stream(_listener_inputs(wrong), timeout=2).read()
            assert error.value.code() == grpc.StatusCode.FAILED_PRECONDITION
            queue.put_nowait(
                kaguya_pb2.ListenerOutput(
                    partial_transcript=kaguya_pb2.PartialTranscript(text="still live")
                )
            )
            assert (
                await asyncio.wait_for(live.read(), 2)
            ).partial_transcript.text == "still live"
            live.cancel()
    finally:
        await server.stop(grace=0.5)


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["empty", "control-first", "duplicate-init"])
async def test_listener_rejects_missing_misordered_or_duplicate_init(
    binding, session_ref, mode
):
    servicer = ListenerServiceImpl(asyncio.Queue(), binding)
    server, port = await _start_server(
        lambda server: kaguya_pb2_grpc.add_ListenerServiceServicer_to_server(
            servicer, server
        )
    )

    async def inputs():
        if mode == "control-first":
            yield kaguya_pb2.ListenerInput(
                control=kaguya_pb2.ListenerControl(reset=kaguya_pb2.ListenerReset())
            )
        elif mode == "duplicate-init":
            async for item in _listener_inputs(session_ref):
                yield item
                yield item

    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:
            call = kaguya_pb2_grpc.ListenerServiceStub(channel).Stream(
                inputs(), timeout=2
            )
            with pytest.raises(grpc.aio.AioRpcError) as error:
                await call.read()
            assert error.value.code() == grpc.StatusCode.INVALID_ARGUMENT
    finally:
        await server.stop(grace=0.5)


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["empty", "barge-in-first", "duplicate-start"])
async def test_talker_rejects_missing_misordered_or_duplicate_start(
    binding, session_ref, mode
):
    speaker = MagicMock()
    servicer = TalkerServiceServicer(TalkerConfig(), speaker, binding)
    servicer._llm.stream_completion = _blocked_completion
    server, port = await _start_server(
        lambda server: kaguya_pb2_grpc.add_TalkerServiceServicer_to_server(
            servicer, server
        )
    )

    async def inputs():
        if mode == "barge-in-first":
            yield kaguya_pb2.TalkerInput(
                barge_in=kaguya_pb2.BargeInSignal(session=session_ref)
            )
        elif mode == "duplicate-start":
            yield kaguya_pb2.TalkerInput(start=_generation(session_ref))
            yield kaguya_pb2.TalkerInput(start=_generation(session_ref))

    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:
            call = kaguya_pb2_grpc.TalkerServiceStub(channel).Converse(
                inputs(), timeout=2
            )
            with pytest.raises(grpc.aio.AioRpcError) as error:
                async for _ in call:
                    pass
            assert error.value.code() == grpc.StatusCode.INVALID_ARGUMENT
            if mode != "duplicate-start":
                speaker.reset.assert_not_called()
            speaker.stop.assert_not_called()
    finally:
        await servicer.close()
        await server.stop(grace=0.5)
