"""Voice entry points validate one immutable deployment scope before mutation."""

import asyncio
import uuid
from dataclasses import FrozenInstanceError
from unittest.mock import AsyncMock, MagicMock

import grpc
import pytest

from binding import AUDIO_MAGIC, InstanceSessionBinding
from config import TalkerConfig
from proto import kaguya_pb2, kaguya_pb2_grpc
from server import TalkerServiceServicer

INSTANCE_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
SESSION_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"


def test_binding_is_preconfigured_immutable_and_accepts_audio_before_rpc():
    binding = InstanceSessionBinding(INSTANCE_ID, SESSION_ID)
    instance = uuid.UUID(INSTANCE_ID)
    session = uuid.UUID(SESSION_ID)
    assert binding.matches_audio_header(AUDIO_MAGIC + instance.bytes + session.bytes)
    with pytest.raises(FrozenInstanceError):
        binding.session_id = str(uuid.uuid4())
    for header in (
        AUDIO_MAGIC + uuid.uuid4().bytes + session.bytes,
        AUDIO_MAGIC + instance.bytes + uuid.uuid4().bytes,
        b"INVALID" + instance.bytes + session.bytes,
        AUDIO_MAGIC,
        AUDIO_MAGIC + instance.bytes + session.bytes + b"extra",
    ):
        assert not binding.matches_audio_header(header)


@pytest.mark.asyncio
async def test_payload_scope_is_required_before_persona_prefill_or_generation_mutation(
    binding, session_ref, session_ref_factory
):
    speaker = MagicMock()
    talker = TalkerServiceServicer(TalkerConfig(), speaker, binding)
    talker._llm.prefill = AsyncMock()
    server = grpc.aio.server()
    kaguya_pb2_grpc.add_TalkerServiceServicer_to_server(talker, server)
    port = server.add_insecure_port("127.0.0.1:0")
    await server.start()
    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:
            stub = kaguya_pb2_grpc.TalkerServiceStub(channel)
            # No custom metadata is needed; the typed request is authoritative.
            await stub.UpdatePersona(
                kaguya_pb2.UpdatePersonaRequest(
                    session=session_ref,
                    persona=kaguya_pb2.PersonaConfig(soul_md="Original persona"),
                ),
                timeout=2,
            )
            invalid_scopes = [
                (kaguya_pb2.SessionRef(), grpc.StatusCode.INVALID_ARGUMENT),
                (session_ref_factory(instance_id=""), grpc.StatusCode.INVALID_ARGUMENT),
                (session_ref_factory(session_id=""), grpc.StatusCode.INVALID_ARGUMENT),
                (
                    kaguya_pb2.SessionRef(
                        instance_id=kaguya_pb2.InstanceId(value=INSTANCE_ID)
                    ),
                    grpc.StatusCode.INVALID_ARGUMENT,
                ),
                (
                    kaguya_pb2.SessionRef(
                        session_id=kaguya_pb2.SessionId(value=SESSION_ID)
                    ),
                    grpc.StatusCode.INVALID_ARGUMENT,
                ),
                (
                    session_ref_factory(instance_id="invalid"),
                    grpc.StatusCode.INVALID_ARGUMENT,
                ),
                (
                    session_ref_factory(instance_id=INSTANCE_ID.upper()),
                    grpc.StatusCode.INVALID_ARGUMENT,
                ),
                (
                    session_ref_factory(session_id=str(uuid.UUID(int=0))),
                    grpc.StatusCode.INVALID_ARGUMENT,
                ),
                (
                    session_ref_factory(session_id=INSTANCE_ID),
                    grpc.StatusCode.INVALID_ARGUMENT,
                ),
                (
                    session_ref_factory(instance_id=str(uuid.uuid4())),
                    grpc.StatusCode.FAILED_PRECONDITION,
                ),
                (
                    session_ref_factory(session_id=str(uuid.uuid4())),
                    grpc.StatusCode.FAILED_PRECONDITION,
                ),
            ]
            for rejected, code in invalid_scopes:
                with pytest.raises(grpc.aio.AioRpcError) as error:
                    await stub.UpdatePersona(
                        kaguya_pb2.UpdatePersonaRequest(
                            session=rejected,
                            persona=kaguya_pb2.PersonaConfig(soul_md="Wrong persona"),
                        ),
                        timeout=2,
                    )
                assert error.value.code() == code
                assert talker._persona.soul_md == "Original persona"

                with pytest.raises(grpc.aio.AioRpcError) as error:
                    await stub.PrefillCache(
                        kaguya_pb2.PrefillRequest(
                            session=rejected, prompt_context=kaguya_pb2.PromptContext()
                        ),
                        timeout=2,
                    )
                assert error.value.code() == code

                async def inputs(rejected=rejected):
                    yield kaguya_pb2.TalkerInput(
                        start=kaguya_pb2.GenerationStart(
                            session=rejected,
                            generation_id="generation-1",
                            prompt_context=kaguya_pb2.PromptContext(),
                            user_input=kaguya_pb2.UserInput(text="Hello"),
                        )
                    )

                with pytest.raises(grpc.aio.AioRpcError) as error:
                    await stub.Converse(inputs(), timeout=2).read()
                assert error.value.code() == code

            talker._llm.prefill.assert_not_awaited()
            speaker.reset.assert_not_called()
            speaker.stop.assert_not_called()
            await stub.PrefillCache(
                kaguya_pb2.PrefillRequest(
                    session=session_ref, prompt_context=kaguya_pb2.PromptContext()
                ),
                timeout=2,
            )
            talker._llm.prefill.assert_awaited_once()
            assert binding.matches_audio_header(
                AUDIO_MAGIC + uuid.UUID(INSTANCE_ID).bytes + uuid.UUID(SESSION_ID).bytes
            )
    finally:
        await talker.close()
        await server.stop(grace=0.5)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "invalid",
    ["generation-id", "trigger", "prompt", "tool-id", "task-id", "progress-id"],
)
async def test_invalid_generation_rejected_before_speaker_reset(
    binding, session_ref, invalid
):
    speaker = MagicMock()
    talker = TalkerServiceServicer(TalkerConfig(), speaker, binding)
    start = kaguya_pb2.GenerationStart(
        session=session_ref,
        generation_id="generation-1",
        prompt_context=kaguya_pb2.PromptContext(),
        user_input=kaguya_pb2.UserInput(text="Hello"),
    )
    if invalid == "generation-id":
        start.generation_id = ""
    elif invalid == "trigger":
        start.ClearField("user_input")
    elif invalid == "prompt":
        start.ClearField("prompt_context")
    elif invalid == "tool-id":
        start.tool_result.CopyFrom(
            kaguya_pb2.ToolResult(content="Result without reference")
        )
    elif invalid == "task-id":
        start.reasoner_result.CopyFrom(
            kaguya_pb2.ReasonerResult(summary="Result without reference")
        )
    else:
        start.reasoner_progress.CopyFrom(
            kaguya_pb2.ReasonerProgress(description="Progress without reference")
        )
    server = grpc.aio.server()
    kaguya_pb2_grpc.add_TalkerServiceServicer_to_server(talker, server)
    port = server.add_insecure_port("127.0.0.1:0")
    await server.start()
    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:

            async def inputs():
                yield kaguya_pb2.TalkerInput(start=start)

            with pytest.raises(grpc.aio.AioRpcError) as error:
                await (
                    kaguya_pb2_grpc.TalkerServiceStub(channel)
                    .Converse(inputs(), timeout=2)
                    .read()
                )
            assert error.value.code() == grpc.StatusCode.INVALID_ARGUMENT
            speaker.reset.assert_not_called()
    finally:
        await talker.close()
        await server.stop(grace=0.5)


@pytest.mark.asyncio
async def test_generation_keeps_cached_persona_and_generation_identity(
    binding, session_ref
):
    speaker = MagicMock()
    talker = TalkerServiceServicer(TalkerConfig(), speaker, binding)
    prompts = []

    async def completion(prompt, cancel):
        prompts.append(prompt)
        yield 'Hello [EMOTION:joy] [TOOL:read_file({"path":"notes"})] [DELEGATE:research]. '

    talker._llm.stream_completion = completion
    server = grpc.aio.server()
    kaguya_pb2_grpc.add_TalkerServiceServicer_to_server(talker, server)
    port = server.add_insecure_port("127.0.0.1:0")
    await server.start()
    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:
            stub = kaguya_pb2_grpc.TalkerServiceStub(channel)
            await stub.UpdatePersona(
                kaguya_pb2.UpdatePersonaRequest(
                    session=session_ref,
                    persona=kaguya_pb2.PersonaConfig(soul_md="Cached persona"),
                ),
                timeout=2,
            )
            for generation_id in ("generation-1", "generation-2"):

                async def inputs(generation_id=generation_id):
                    yield kaguya_pb2.TalkerInput(
                        start=kaguya_pb2.GenerationStart(
                            session=session_ref,
                            generation_id=generation_id,
                            prompt_context=kaguya_pb2.PromptContext(),
                            user_input=kaguya_pb2.UserInput(text="Hello"),
                        )
                    )

                outputs = [
                    output async for output in stub.Converse(inputs(), timeout=2)
                ]
                assert all(output.generation_id == generation_id for output in outputs)
                assert outputs[0].HasField("response_started")
                assert outputs[-1].HasField("response_complete")
                assert not outputs[-1].response_complete.was_interrupted
                assert {output.WhichOneof("payload") for output in outputs} == {
                    "response_started",
                    "sentence",
                    "emotion",
                    "tool_request",
                    "delegate_request",
                    "response_complete",
                }
            assert len(prompts) == 2
            assert all("Cached persona" in prompt for prompt in prompts)
    finally:
        await talker.close()
        await server.stop(grace=0.5)


@pytest.mark.asyncio
async def test_barge_in_ack_and_interrupted_completion_keep_generation_identity(
    binding, session_ref
):
    speaker = MagicMock()
    speaker.stop.return_value = ("spoken", "unspoken")
    talker = TalkerServiceServicer(TalkerConfig(), speaker, binding)
    ready = asyncio.Event()

    async def completion(prompt, cancel):
        await cancel.wait()
        yield "unspoken"

    talker._llm.stream_completion = completion
    server = grpc.aio.server()
    kaguya_pb2_grpc.add_TalkerServiceServicer_to_server(talker, server)
    port = server.add_insecure_port("127.0.0.1:0")
    await server.start()
    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:

            async def inputs():
                yield kaguya_pb2.TalkerInput(
                    start=kaguya_pb2.GenerationStart(
                        session=session_ref,
                        generation_id="interrupted-generation",
                        prompt_context=kaguya_pb2.PromptContext(),
                        user_input=kaguya_pb2.UserInput(text="Hello"),
                    )
                )
                await ready.wait()
                yield kaguya_pb2.TalkerInput(
                    barge_in=kaguya_pb2.BargeInSignal(session=session_ref)
                )

            call = kaguya_pb2_grpc.TalkerServiceStub(channel).Converse(
                inputs(), timeout=2
            )
            first = await call.read()
            assert first.HasField("response_started")
            ready.set()
            outputs = [first]
            while True:
                output = await call.read()
                if output == grpc.aio.EOF:
                    break
                outputs.append(output)
            assert all(
                output.generation_id == "interrupted-generation" for output in outputs
            )
            assert [output.WhichOneof("payload") for output in outputs] == [
                "response_started",
                "barge_in_ack",
                "response_complete",
            ]
            assert outputs[1].barge_in_ack.spoken_text == "spoken"
            assert outputs[-1].response_complete.was_interrupted
            speaker.stop.assert_called_once()
    finally:
        await talker.close()
        await server.stop(grace=0.5)


@pytest.mark.asyncio
async def test_barge_in_assertion_cannot_stop_another_scope(
    binding, session_ref, session_ref_factory
):
    speaker = MagicMock()
    talker = TalkerServiceServicer(TalkerConfig(), speaker, binding)
    ready = asyncio.Event()

    async def blocked_completion(*args):
        await asyncio.Event().wait()
        if False:
            yield ""

    talker._llm.stream_completion = blocked_completion
    server = grpc.aio.server()
    kaguya_pb2_grpc.add_TalkerServiceServicer_to_server(talker, server)
    port = server.add_insecure_port("127.0.0.1:0")
    await server.start()
    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as channel:

            async def inputs():
                yield kaguya_pb2.TalkerInput(
                    start=kaguya_pb2.GenerationStart(
                        session=session_ref,
                        generation_id="generation-1",
                        prompt_context=kaguya_pb2.PromptContext(),
                        user_input=kaguya_pb2.UserInput(text="Hello"),
                    )
                )
                await ready.wait()
                yield kaguya_pb2.TalkerInput(
                    barge_in=kaguya_pb2.BargeInSignal(
                        session=session_ref_factory(session_id=str(uuid.uuid4()))
                    )
                )

            call = kaguya_pb2_grpc.TalkerServiceStub(channel).Converse(
                inputs(), timeout=2
            )
            await call.initial_metadata()
            ready.set()
            with pytest.raises(grpc.aio.AioRpcError) as error:
                async for _ in call:
                    pass
            assert error.value.code() == grpc.StatusCode.FAILED_PRECONDITION
            speaker.reset.assert_called_once()
            speaker.stop.assert_not_called()
    finally:
        await talker.close()
        await server.stop(grace=0.5)
