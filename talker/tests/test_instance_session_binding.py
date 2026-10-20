"""The shared voice process binds on its first valid instance/session pair."""

import asyncio
import uuid
from unittest.mock import MagicMock

import grpc
import pytest

from binding import AUDIO_MAGIC, INSTANCE_HEADER, SESSION_HEADER, InstanceSessionBinding
from config import TalkerConfig
from proto import kaguya_pb2, kaguya_pb2_grpc  # type: ignore[import]
from server import TalkerServiceServicer
from voice.listener import ListenerServiceImpl


@pytest.mark.asyncio
async def test_shared_voice_binding_rejects_other_instance_or_session():
    binding = InstanceSessionBinding()
    server = grpc.aio.server()
    talker = TalkerServiceServicer(TalkerConfig(), MagicMock(), binding)
    listener = ListenerServiceImpl(asyncio.Queue(), binding)
    kaguya_pb2_grpc.add_TalkerServiceServicer_to_server(talker, server)
    kaguya_pb2_grpc.add_ListenerServiceServicer_to_server(listener, server)
    port = server.add_insecure_port("127.0.0.1:0")
    await server.start()

    instance = uuid.UUID("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
    session = uuid.uuid4()
    metadata = ((INSTANCE_HEADER, str(instance)), (SESSION_HEADER, str(session)))
    other_instance = (
        (INSTANCE_HEADER, str(uuid.uuid4())),
        (SESSION_HEADER, str(session)),
    )
    other_session = (
        (INSTANCE_HEADER, str(instance)),
        (SESSION_HEADER, str(uuid.uuid4())),
    )
    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:
            talker_stub = kaguya_pb2_grpc.TalkerServiceStub(channel)
            listener_stub = kaguya_pb2_grpc.ListenerServiceStub(channel)
            assert not binding.matches_audio_header(
                AUDIO_MAGIC + instance.bytes + session.bytes
            )
            # Invalid bootstrap attempts must not establish the shared binding.
            for invalid in (
                (),
                ((INSTANCE_HEADER, "invalid"), (SESSION_HEADER, str(session))),
                (
                    (INSTANCE_HEADER, str(uuid.UUID(int=0))),
                    (SESSION_HEADER, str(session)),
                ),
                ((INSTANCE_HEADER, str(instance)), (SESSION_HEADER, str(instance))),
                (
                    (INSTANCE_HEADER, str(instance).upper()),
                    (SESSION_HEADER, str(session)),
                ),
            ):
                with pytest.raises(grpc.aio.AioRpcError) as error:
                    await talker_stub.UpdatePersona(
                        kaguya_pb2.PersonaConfig(), metadata=invalid, timeout=5
                    )
                assert error.value.code() == grpc.StatusCode.INVALID_ARGUMENT
            await talker_stub.UpdatePersona(
                kaguya_pb2.PersonaConfig(), metadata=metadata, timeout=5
            )
            await talker_stub.UpdatePersona(
                kaguya_pb2.PersonaConfig(), metadata=metadata, timeout=5
            )

            for rejected in (other_instance, other_session):
                with pytest.raises(grpc.aio.AioRpcError) as error:
                    await talker_stub.UpdatePersona(
                        kaguya_pb2.PersonaConfig(), metadata=rejected, timeout=5
                    )
                assert error.value.code() == grpc.StatusCode.FAILED_PRECONDITION

            async def empty_inputs():
                if False:
                    yield

            with pytest.raises(grpc.aio.AioRpcError) as error:
                await listener_stub.Stream(
                    empty_inputs(), metadata=other_session, timeout=5
                ).read()
            assert error.value.code() == grpc.StatusCode.FAILED_PRECONDITION

        assert binding.matches_audio_header(
            AUDIO_MAGIC + instance.bytes + session.bytes
        )
        assert not binding.matches_audio_header(
            AUDIO_MAGIC + uuid.uuid4().bytes + session.bytes
        )
        assert not binding.matches_audio_header(
            AUDIO_MAGIC + instance.bytes + uuid.uuid4().bytes
        )
        assert not binding.matches_audio_header(
            b"INVALID" + instance.bytes + session.bytes
        )
        assert not binding.matches_audio_header(AUDIO_MAGIC)
    finally:
        await talker.close()
        await server.stop(grace=0.5)
