"""server.py — TalkerServiceServicer: bidi Converse + unary helpers.

Gateway opens a Converse bidi stream per generation:
  Gateway → Talker: TalkerInput.start (GenerationStart) | TalkerInput.barge_in
  Talker → Gateway: TalkerOutput stream (sentences, emotions, tools, etc.)
"""

from __future__ import annotations

import asyncio
import logging
from collections.abc import AsyncGenerator, Generator
from typing import TYPE_CHECKING

import grpc

from binding import InstanceSessionBinding
from config import TalkerConfig
from inference.llm_client import LLMClient
from inference.prompt_formatter import assemble_generation_prompt, assemble_prompt
from inference.sentence_detector import SentenceDetector
from inference.soul_container import (
    IdentityConfig,
    SoulContainerResult,
    parse_identity_config,
)
from inference.soul_container import (
    process as soul_process,
)
from proto import kaguya_pb2, kaguya_pb2_grpc  # type: ignore[import]

if TYPE_CHECKING:
    # `Speaker` transitively imports RealtimeTTS, which the CI test environment
    # intentionally omits (CUDA dep). Tests pass a MagicMock in its place.
    # Type-only at module load time; real callers (main.py) construct a real
    # Speaker and pass it in.
    from voice.speaker import Speaker

logger = logging.getLogger(__name__)


class TalkerServiceServicer(kaguya_pb2_grpc.TalkerServiceServicer):
    def __init__(
        self,
        config: TalkerConfig,
        speaker: Speaker,
        binding: InstanceSessionBinding,
    ) -> None:
        self._config = config
        self._llm = LLMClient(config)
        self._speaker = speaker
        self._persona = kaguya_pb2.PersonaConfig()
        self._identity = IdentityConfig()
        self._binding = binding

    # ──────────────────────────────────────────
    # Converse — bidi streaming, replaces ProcessPrompt + Prepare
    # ──────────────────────────────────────────

    async def Converse(
        self,
        request_iterator,
        context: grpc.aio.ServicerContext,
    ) -> AsyncGenerator[kaguya_pb2.TalkerOutput, None]:
        # The client prequeues start before opening the stream. Read and
        # validate it before headers: a completed handshake means its fixed
        # scope and generation trigger have been accepted.
        request_iterator = aiter(request_iterator)
        try:
            first = await anext(request_iterator)
        except StopAsyncIteration:
            await context.abort(
                grpc.StatusCode.INVALID_ARGUMENT,
                "Converse requires GenerationStart first",
            )
            return
        if first.WhichOneof("payload") != "start":
            await context.abort(
                grpc.StatusCode.INVALID_ARGUMENT,
                "Converse requires GenerationStart first",
            )
        await self._binding.require(first.start.session, context)
        await _validate_generation_start(first.start, context)
        await context.send_initial_metadata(())

        output_queue: asyncio.Queue[kaguya_pb2.TalkerOutput | Exception] = (
            asyncio.Queue()
        )
        cancel_event = asyncio.Event()
        self._speaker.reset()
        generation_task = asyncio.create_task(
            self._run_generation(first.start, output_queue, cancel_event)
        )

        async def read_inputs():
            try:
                async for msg in request_iterator:
                    if msg.WhichOneof("payload") != "barge_in":
                        await context.abort(
                            grpc.StatusCode.INVALID_ARGUMENT,
                            "Converse permits exactly one GenerationStart",
                        )
                    await self._binding.require(msg.barge_in.session, context)
                    logger.debug("<- BargeIn received")
                    cancel_event.set()
                    spoken, unspoken = self._speaker.stop()
                    await output_queue.put(
                        kaguya_pb2.TalkerOutput(
                            seq=0,
                            generation_id=first.start.generation_id,
                            barge_in_ack=kaguya_pb2.BargeInAck(
                                spoken_text=spoken,
                                unspoken_text=unspoken,
                            ),
                        )
                    )
            except Exception as exc:  # noqa: BLE001 - forwarded to the stream handler
                # Propagate validation/transport failures to the yielding
                # coroutine even when inference has produced no output yet.
                await output_queue.put(exc)

        input_task = asyncio.create_task(read_inputs())
        try:
            while True:
                item = await output_queue.get()
                if isinstance(item, Exception):
                    raise item
                yield item
                if item.HasField("response_complete"):
                    return
        except asyncio.CancelledError:
            cancel_event.set()
        finally:
            input_task.cancel()
            if not generation_task.done():
                generation_task.cancel()
            await asyncio.gather(input_task, generation_task, return_exceptions=True)

    async def _run_generation(
        self,
        ctx: kaguya_pb2.GenerationStart,
        output_queue: asyncio.Queue,
        cancel_event: asyncio.Event,
    ) -> None:
        seq = 0
        sentence_count = 0
        generation_id = ctx.generation_id
        was_interrupted = False

        seq += 1
        await output_queue.put(
            kaguya_pb2.TalkerOutput(
                seq=seq,
                generation_id=generation_id,
                response_started=kaguya_pb2.ResponseStarted(),
            )
        )

        try:
            prompt = assemble_generation_prompt(ctx, self._persona)
            detector = SentenceDetector()
            async for token in self._llm.stream_completion(prompt, cancel_event):
                if cancel_event.is_set():
                    was_interrupted = True
                    break

                sentence = detector.feed(token)
                if sentence is not None:
                    sentence_count += 1
                    for out in _emit_sentence(
                        sentence, self._identity, self._speaker, seq, generation_id
                    ):
                        seq = out.seq
                        await output_queue.put(out)
                    if sentence_count >= self._config.max_response_sentences:
                        break
                    if cancel_event.is_set():
                        was_interrupted = True
                        break

            if not was_interrupted:
                remainder = detector.flush()
                if remainder and sentence_count < self._config.max_response_sentences:
                    for out in _emit_sentence(
                        remainder, self._identity, self._speaker, seq, generation_id
                    ):
                        seq = out.seq
                        await output_queue.put(out)

        except ConnectionError as exc:
            logger.error("LLM connection failed: %s", exc)
            was_interrupted = True
        except Exception:
            logger.exception("Unexpected error during generation")
            was_interrupted = True

        seq += 1
        await output_queue.put(
            kaguya_pb2.TalkerOutput(
                seq=seq,
                generation_id=generation_id,
                response_complete=kaguya_pb2.ResponseComplete(
                    was_interrupted=was_interrupted,
                ),
            )
        )

    # ──────────────────────────────────────────
    # PrefillCache + UpdatePersona — scoped unary helpers
    # ──────────────────────────────────────────

    async def PrefillCache(self, request, context):
        await self._binding.require(request.session, context)
        if not request.HasField("prompt_context"):
            await context.abort(
                grpc.StatusCode.INVALID_ARGUMENT, "PrefillCache requires prompt_context"
            )
        await _validate_prompt_context(request.prompt_context, context)
        prompt = assemble_prompt(request.prompt_context, self._persona)
        await self._llm.prefill(prompt)
        return kaguya_pb2.PrefillAck()

    async def UpdatePersona(self, request, context):
        await self._binding.require(request.session, context)
        if not request.HasField("persona"):
            await context.abort(
                grpc.StatusCode.INVALID_ARGUMENT, "UpdatePersona requires persona"
            )
        persona = kaguya_pb2.PersonaConfig()
        persona.CopyFrom(request.persona)
        identity = parse_identity_config(persona.identity_md or "")
        self._persona = persona
        self._identity = identity
        logger.info(
            "Persona updated (soul=%d, identity=%d, memory=%d bytes)",
            len(persona.soul_md),
            len(persona.identity_md),
            len(persona.memory_md),
        )
        return kaguya_pb2.PersonaAck()

    async def close(self) -> None:
        await self._llm.close()


async def _validate_prompt_context(prompt, context) -> None:
    if any(not task.task_id.strip() for task in prompt.active_tasks):
        await context.abort(
            grpc.StatusCode.INVALID_ARGUMENT, "active task requires task_id"
        )


async def _validate_generation_start(start, context) -> None:
    if not start.generation_id.strip():
        await context.abort(
            grpc.StatusCode.INVALID_ARGUMENT, "GenerationStart requires generation_id"
        )
    if not start.HasField("prompt_context"):
        await context.abort(
            grpc.StatusCode.INVALID_ARGUMENT, "GenerationStart requires prompt_context"
        )
    await _validate_prompt_context(start.prompt_context, context)
    which = start.WhichOneof("trigger")
    if which is None:
        await context.abort(
            grpc.StatusCode.INVALID_ARGUMENT, "GenerationStart requires a trigger"
        )
    if which == "tool_result" and not start.tool_result.request_id.strip():
        await context.abort(
            grpc.StatusCode.INVALID_ARGUMENT, "tool result requires request_id"
        )
    if which == "reasoner_result" and not start.reasoner_result.task_id.strip():
        await context.abort(
            grpc.StatusCode.INVALID_ARGUMENT, "Reasoner result requires task_id"
        )
    if which == "reasoner_progress" and not start.reasoner_progress.task_id.strip():
        await context.abort(
            grpc.StatusCode.INVALID_ARGUMENT, "Reasoner progress requires task_id"
        )


def _emit_sentence(
    sentence: str,
    identity: IdentityConfig,
    speaker: Speaker,
    seq: int,
    generation_id: str,
) -> Generator[kaguya_pb2.TalkerOutput, None, None]:
    result: SoulContainerResult = soul_process(sentence, identity)
    if result.spoken_text:
        speaker.speak(result.spoken_text)
    seq += 1
    yield kaguya_pb2.TalkerOutput(
        seq=seq,
        generation_id=generation_id,
        sentence=kaguya_pb2.SentenceEvent(text=result.spoken_text),
    )
    for emotion in result.emotions:
        seq += 1
        yield kaguya_pb2.TalkerOutput(
            seq=seq,
            generation_id=generation_id,
            emotion=kaguya_pb2.EmotionEvent(emotion=emotion),
        )
    for tool_req in result.tool_requests:
        seq += 1
        yield kaguya_pb2.TalkerOutput(
            seq=seq, generation_id=generation_id, tool_request=tool_req
        )
    for delegate_req in result.delegate_requests:
        seq += 1
        yield kaguya_pb2.TalkerOutput(
            seq=seq, generation_id=generation_id, delegate_request=delegate_req
        )
