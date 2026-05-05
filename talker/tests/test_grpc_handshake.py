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
