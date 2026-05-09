"""Regression tests for the bidi gRPC handshake.

The Listener.Stream and Talker.Converse handlers must send initial response
metadata BEFORE awaiting on their internal queues. grpcio-aio defers the
HTTP/2 HEADERS frame until the first `yield` if metadata isn't sent
explicitly, which would deadlock the client's `stub.Stream(...)` /
`stub.Converse(...)` call: it can't send the first message because the
server's handshake never completed.

Removing the `await context.send_initial_metadata(())` lines from the
handlers will cause both tests below to hit their 2s timeout and fail.
"""

import asyncio
from unittest.mock import MagicMock

import grpc
import pytest

from proto import kaguya_pb2_grpc  # type: ignore[import]


HANDSHAKE_TIMEOUT_S = 2.0


async def _start_server(servicer_factory) -> tuple[grpc.aio.Server, int]:
    """Spin up an in-process gRPC server on an ephemeral loopback port."""
    server = grpc.aio.server()
    servicer_factory(server)
    port = server.add_insecure_port("127.0.0.1:0")
    await server.start()
    return server, port


@pytest.mark.asyncio
async def test_listener_stream_handshake_completes_without_messages():
    """Stream() must respond with initial metadata before any ASR event.

    Without `send_initial_metadata` at the top of Stream(), this test would
    hang on `call.initial_metadata()` because the server's handler is blocked
    on an empty event queue.
    """
    from voice.listener import ListenerServiceImpl

    event_queue: asyncio.Queue = asyncio.Queue()
    servicer = ListenerServiceImpl(event_queue)

    def register(server: grpc.aio.Server) -> None:
        kaguya_pb2_grpc.add_ListenerServiceServicer_to_server(servicer, server)

    server, port = await _start_server(register)
    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:
            stub = kaguya_pb2_grpc.ListenerServiceStub(channel)

            # Empty client stream — never sends any input messages.
            async def empty_inputs():
                if False:  # pragma: no cover — never yields
                    yield

            call = stub.Stream(empty_inputs())
            metadata = await asyncio.wait_for(
                call.initial_metadata(), timeout=HANDSHAKE_TIMEOUT_S
            )
            assert metadata is not None
            call.cancel()
    finally:
        await server.stop(grace=0.5)


@pytest.mark.asyncio
async def test_talker_converse_handshake_completes_without_messages():
    """Converse() must respond with initial metadata before any TalkerInput.

    Same pattern as the listener test — the gateway opens the bidi stream and
    only sends `TalkerInput.start` after the handshake completes. If the
    handler awaits on the input iterator before flushing headers, the gateway
    can't send the start message → deadlock.
    """
    from config import TalkerConfig
    from server import TalkerServiceServicer

    config = TalkerConfig()
    speaker = MagicMock()  # Converse handshake never touches the speaker
    servicer = TalkerServiceServicer(config, speaker)

    def register(server: grpc.aio.Server) -> None:
        kaguya_pb2_grpc.add_TalkerServiceServicer_to_server(servicer, server)

    server, port = await _start_server(register)
    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:
            stub = kaguya_pb2_grpc.TalkerServiceStub(channel)

            async def empty_inputs():
                if False:  # pragma: no cover
                    yield

            call = stub.Converse(empty_inputs())
            metadata = await asyncio.wait_for(
                call.initial_metadata(), timeout=HANDSHAKE_TIMEOUT_S
            )
            assert metadata is not None
            call.cancel()
    finally:
        await server.stop(grace=0.5)


# ── P0-6: ListenerServiceImpl.Stream — replace prior connection ──
#
# `_event_queue.get()` is point-to-point: when two `Stream` handlers
# concurrently await it, an enqueued event goes to ONE of them, not both.
# Symptom in real use: Gateway crash + reconnect leaves the stale handler
# competing with the live one for ASR events; ~half the words show up in
# the live Gateway, the rest go to a dead connection.
#
# Pattern B fix: a new Stream connection terminates the prior one and
# takes ownership of the queue. These tests pin that contract.


@pytest.mark.asyncio
async def test_second_stream_terminates_first():
    """Opening a second Stream while the first is live causes the first
    to be terminated. The second receives subsequent events; the first
    sees its inbound iterator close cleanly (no more events delivered)."""
    from voice.listener import ListenerServiceImpl
    from proto import kaguya_pb2  # type: ignore[import]

    event_queue: asyncio.Queue = asyncio.Queue()
    servicer = ListenerServiceImpl(event_queue)

    def register(server: grpc.aio.Server) -> None:
        kaguya_pb2_grpc.add_ListenerServiceServicer_to_server(servicer, server)

    server, port = await _start_server(register)
    try:
        async with (
            grpc.aio.insecure_channel(f"127.0.0.1:{port}") as ch_a,
            grpc.aio.insecure_channel(f"127.0.0.1:{port}") as ch_b,
        ):
            stub_a = kaguya_pb2_grpc.ListenerServiceStub(ch_a)
            stub_b = kaguya_pb2_grpc.ListenerServiceStub(ch_b)

            async def empty_inputs():
                if False:  # pragma: no cover
                    yield

            # Open client A and finish handshake.
            call_a = stub_a.Stream(empty_inputs())
            await asyncio.wait_for(call_a.initial_metadata(), timeout=2.0)

            # Open client B — this should terminate A.
            call_b = stub_b.Stream(empty_inputs())
            await asyncio.wait_for(call_b.initial_metadata(), timeout=2.0)

            # Give the server a tick to process the takeover.
            await asyncio.sleep(0.1)

            # Now enqueue an event. With Pattern B, only B should see it;
            # A's stream should be closed.
            event_queue.put_nowait(
                kaguya_pb2.ListenerOutput(
                    timestamp_ms=1,
                    final_transcript=kaguya_pb2.FinalTranscript(
                        text="hello", confidence=0.9
                    ),
                )
            )

            # B receives the event.
            received_b = await asyncio.wait_for(call_b.read(), timeout=2.0)
            assert received_b is not None and received_b != grpc.aio.EOF, (
                "client B must receive events after A is replaced"
            )
            assert received_b.final_transcript.text == "hello"

            # A's stream should be terminated. `call_a.read()` should return
            # EOF (or grpc closes), not block forever.
            try:
                a_next = await asyncio.wait_for(call_a.read(), timeout=2.0)
                # If we got something, it must be EOF — not a real event.
                assert a_next == grpc.aio.EOF or a_next is None, (
                    f"client A must be terminated; instead read: {a_next}"
                )
            except (asyncio.TimeoutError, grpc.aio.AioRpcError):
                pytest.fail("client A must be cleanly terminated, not left hanging")

            call_b.cancel()
    finally:
        await server.stop(grace=0.5)


@pytest.mark.asyncio
async def test_replaced_stream_event_goes_to_live_client_only():
    """Stronger version of the above: enqueue events between A's open and
    B's open. After B replaces A, only B should receive the post-replace
    events — not split between them."""
    from voice.listener import ListenerServiceImpl
    from proto import kaguya_pb2  # type: ignore[import]

    event_queue: asyncio.Queue = asyncio.Queue()
    servicer = ListenerServiceImpl(event_queue)

    def register(server: grpc.aio.Server) -> None:
        kaguya_pb2_grpc.add_ListenerServiceServicer_to_server(servicer, server)

    server, port = await _start_server(register)
    try:
        async with (
            grpc.aio.insecure_channel(f"127.0.0.1:{port}") as ch_a,
            grpc.aio.insecure_channel(f"127.0.0.1:{port}") as ch_b,
        ):
            stub_a = kaguya_pb2_grpc.ListenerServiceStub(ch_a)
            stub_b = kaguya_pb2_grpc.ListenerServiceStub(ch_b)

            async def empty_inputs():
                if False:  # pragma: no cover
                    yield

            call_a = stub_a.Stream(empty_inputs())
            await asyncio.wait_for(call_a.initial_metadata(), timeout=2.0)

            # Open B, which should take over.
            call_b = stub_b.Stream(empty_inputs())
            await asyncio.wait_for(call_b.initial_metadata(), timeout=2.0)
            await asyncio.sleep(0.1)

            # Enqueue 3 events. Old code: load-balanced 1-2 to A and 1-2 to
            # B, depending on scheduler. New code: all 3 to B.
            for i in range(3):
                event_queue.put_nowait(
                    kaguya_pb2.ListenerOutput(
                        timestamp_ms=i,
                        partial_transcript=kaguya_pb2.PartialTranscript(
                            text=f"event-{i}"
                        ),
                    )
                )

            received_by_b = []
            for _ in range(3):
                ev = await asyncio.wait_for(call_b.read(), timeout=2.0)
                if ev == grpc.aio.EOF or ev is None:
                    break
                received_by_b.append(ev.partial_transcript.text)

            assert received_by_b == ["event-0", "event-1", "event-2"], (
                f"client B must receive ALL post-replacement events; got {received_by_b}"
            )

            call_b.cancel()
    finally:
        await server.stop(grace=0.5)
