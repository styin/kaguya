"""Stream readiness follows validation of the request scope.

Listener and Talker validate the first stream message.
Headers must arrive before ASR/inference output. Invalid scope cannot take over a
live Listener connection, and malformed Talker streams fail promptly.
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


def _generation(session):
    return kaguya_pb2.GenerationStart(
        session=session,
        generation_id="generation-1",
        prompt_context=kaguya_pb2.PromptContext(),
        user_input=kaguya_pb2.UserInput(text="Hello"),
    )


async def _listener_inputs(session, *messages):
    yield kaguya_pb2.ListenerInput(init=kaguya_pb2.ListenerInit(session=session))
    for message in messages:
        yield message


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
            first = stub.Stream()
            await first.write(
                kaguya_pb2.ListenerInput(
                    init=kaguya_pb2.ListenerInit(session=session_ref)
                )
            )
            await asyncio.wait_for(first.initial_metadata(), 2)
            replacement = stub.Stream()
            await replacement.write(
                kaguya_pb2.ListenerInput(
                    init=kaguya_pb2.ListenerInit(session=session_ref)
                )
            )
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
async def test_listener_rejects_missing_scope(binding):
    servicer = ListenerServiceImpl(asyncio.Queue(), binding)
    server, port = await _start_server(
        lambda server: kaguya_pb2_grpc.add_ListenerServiceServicer_to_server(
            servicer, server
        )
    )

    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:
            call = kaguya_pb2_grpc.ListenerServiceStub(channel).Stream(
                _listener_inputs(None), timeout=2
            )
            with pytest.raises(grpc.aio.AioRpcError) as error:
                await call.read()
            assert error.value.code() == grpc.StatusCode.INVALID_ARGUMENT
    finally:
        await server.stop(grace=0.5)


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["empty", "empty-payload", "control-first"])
async def test_listener_requires_init_before_accepting_subscription(binding, mode):
    servicer = ListenerServiceImpl(asyncio.Queue(), binding)
    server, port = await _start_server(
        lambda server: kaguya_pb2_grpc.add_ListenerServiceServicer_to_server(
            servicer, server
        )
    )

    async def inputs():
        if mode == "empty-payload":
            yield kaguya_pb2.ListenerInput()
        elif mode == "control-first":
            yield kaguya_pb2.ListenerInput(
                control=kaguya_pb2.ListenerControl(reset=kaguya_pb2.ListenerReset())
            )

    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:
            call = kaguya_pb2_grpc.ListenerServiceStub(channel).Stream(
                inputs(), timeout=2
            )
            with pytest.raises(grpc.aio.AioRpcError) as error:
                await call.read()
            assert error.value.code() == grpc.StatusCode.INVALID_ARGUMENT
            assert servicer._active_terminate is None
    finally:
        await server.stop(grace=0.5)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "mode", ["reset", "empty-control", "empty-payload", "repeat-init"]
)
async def test_listener_controls_fail_explicitly_without_waiting_for_asr(
    binding, session_ref, mode
):
    servicer = ListenerServiceImpl(asyncio.Queue(), binding)
    server, port = await _start_server(
        lambda server: kaguya_pb2_grpc.add_ListenerServiceServicer_to_server(
            servicer, server
        )
    )
    if mode == "reset":
        message = kaguya_pb2.ListenerInput(
            control=kaguya_pb2.ListenerControl(reset=kaguya_pb2.ListenerReset())
        )
    elif mode == "empty-control":
        message = kaguya_pb2.ListenerInput(control=kaguya_pb2.ListenerControl())
    elif mode == "repeat-init":
        message = kaguya_pb2.ListenerInput(
            init=kaguya_pb2.ListenerInit(session=session_ref)
        )
    else:
        message = kaguya_pb2.ListenerInput()
    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:
            call = kaguya_pb2_grpc.ListenerServiceStub(channel).Stream()
            await call.write(
                kaguya_pb2.ListenerInput(
                    init=kaguya_pb2.ListenerInit(session=session_ref)
                )
            )
            await asyncio.wait_for(call.initial_metadata(), 2)
            await call.write(message)
            with pytest.raises(grpc.aio.AioRpcError) as error:
                await asyncio.wait_for(call.read(), 2)
            expected = (
                grpc.StatusCode.UNIMPLEMENTED
                if mode == "reset"
                else grpc.StatusCode.INVALID_ARGUMENT
            )
            assert error.value.code() == expected
    finally:
        await server.stop(grace=0.5)


@pytest.mark.asyncio
async def test_listener_request_half_close_keeps_event_subscription(
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
            call = kaguya_pb2_grpc.ListenerServiceStub(channel).Stream()
            await call.write(
                kaguya_pb2.ListenerInput(
                    init=kaguya_pb2.ListenerInit(session=session_ref)
                )
            )
            await asyncio.wait_for(call.initial_metadata(), 2)
            await call.done_writing()
            queue.put_nowait(
                kaguya_pb2.ListenerOutput(
                    partial_transcript=kaguya_pb2.PartialTranscript(
                        text="after half close"
                    )
                )
            )
            assert (
                await asyncio.wait_for(call.read(), 2)
            ).partial_transcript.text == "after half close"
            call.cancel()
    finally:
        await server.stop(grace=0.5)


@pytest.mark.asyncio
async def test_listener_cancellation_with_pending_controls_releases_queue(
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
            live = stub.Stream()
            await live.write(
                kaguya_pb2.ListenerInput(
                    init=kaguya_pb2.ListenerInit(session=session_ref)
                )
            )
            await asyncio.wait_for(live.initial_metadata(), 2)
            active = servicer._active_terminate
            pending = stub.Stream()
            pending.cancel()
            assert servicer._active_terminate is active
            live.cancel()

            async def wait_for_cleanup():
                while servicer._active_terminate is not None:
                    await asyncio.sleep(0)

            await asyncio.wait_for(wait_for_cleanup(), 2)
            queue.put_nowait(
                kaguya_pb2.ListenerOutput(
                    partial_transcript=kaguya_pb2.PartialTranscript(text="retained")
                )
            )
            replacement = stub.Stream(_listener_inputs(session_ref))
            assert (
                await asyncio.wait_for(replacement.read(), 2)
            ).partial_transcript.text == "retained"
            replacement.cancel()
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
            yield kaguya_pb2.TalkerInput(barge_in=kaguya_pb2.BargeInSignal())
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
