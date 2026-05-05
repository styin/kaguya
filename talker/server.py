"""server.py — TalkerServiceServicer: bidi Converse + unary helpers.

Gateway opens a Converse bidi stream per turn:
  Gateway → Talker: TalkerInput.start (TalkerContext) | TalkerInput.barge_in
  Talker → Gateway: TalkerOutput stream (sentences, emotions, tools, etc.)
"""

import asyncio
import logging
from collections.abc import AsyncGenerator, Generator

import grpc

from config import TalkerConfig
from inference.llm_client import LLMClient
from inference.prompt_formatter import assemble_prompt
from inference.sentence_detector import SentenceDetector
from inference.soul_container import (
    IdentityConfig,
    SoulContainerResult,
    parse_identity_config,
    process as soul_process,
)
from voice.speaker import Speaker

from proto import kaguya_pb2, kaguya_pb2_grpc  # type: ignore[import]

logger = logging.getLogger(__name__)


class TalkerServiceServicer(kaguya_pb2_grpc.TalkerServiceServicer):
    def __init__(self, config: TalkerConfig, speaker: Speaker) -> None:
        self._config = config
        self._llm = LLMClient(config)
        self._speaker = speaker
        self._persona = kaguya_pb2.PersonaConfig()
        self._identity = IdentityConfig()

    # ──────────────────────────────────────────
    # Converse — bidi streaming, replaces ProcessPrompt + Prepare
    # ──────────────────────────────────────────

    async def Converse(
        self,
        request_iterator,
        context: grpc.aio.ServicerContext,
    ) -> AsyncGenerator[kaguya_pb2.TalkerOutput, None]:
        # Flush response headers immediately. Without this, grpcio-aio defers
        # the HTTP/2 HEADERS frame until the first `yield`, which only happens
        # after `start` arrives and a sentence is generated. The Gateway's
        # `client.converse(...).await` would otherwise block indefinitely on
        # the handshake before it can send the initial TalkerInput.start.
        await context.send_initial_metadata(())

        output_queue: asyncio.Queue[kaguya_pb2.TalkerOutput | None] = asyncio.Queue()
        cancel_event = asyncio.Event()
        generation_task: asyncio.Task | None = None

        async def read_inputs():
            nonlocal generation_task
            async for msg in request_iterator:
                which = msg.WhichOneof("payload")
                if which == "start":
                    cancel_event.clear()
                    self._speaker.reset()
                    generation_task = asyncio.create_task(
                        self._run_generation(msg.start, output_queue, cancel_event)
                    )
                elif which == "barge_in":
                    logger.debug("← BargeIn received")
                    cancel_event.set()
                    spoken, unspoken = self._speaker.stop()
                    await output_queue.put(
                        kaguya_pb2.TalkerOutput(
                            seq=0,
                            barge_in_ack=kaguya_pb2.BargeInAck(
                                spoken_text=spoken, unspoken_text=unspoken,
                            ),
                        )
                    )

        input_task = asyncio.create_task(read_inputs())

        try:
            while True:
                item = await output_queue.get()
                if item is None:
                    break
                yield item
                # Break on terminal event to avoid deadlock
                # (Talker returns → server closes stream → Gateway detects Ok(None))
                if item.HasField("response_complete"):
                    return
        except asyncio.CancelledError:
            cancel_event.set()
        finally:
            input_task.cancel()
            if generation_task and not generation_task.done():
                generation_task.cancel()

    async def _run_generation(
        self,
        ctx: kaguya_pb2.TalkerContext,
        output_queue: asyncio.Queue,
        cancel_event: asyncio.Event,
    ) -> None:
        seq = 0
        sentence_count = 0
        turn_id = ctx.turn_id
        was_interrupted = False

        seq += 1
        await output_queue.put(
            kaguya_pb2.TalkerOutput(
                seq=seq,
                response_started=kaguya_pb2.ResponseStarted(turn_id=turn_id),
            )
        )

        prompt = assemble_prompt(ctx, self._persona)
        detector = SentenceDetector()

        try:
            async for token in self._llm.stream_completion(prompt, cancel_event):
                if cancel_event.is_set():
                    was_interrupted = True
                    break

                sentence = detector.feed(token)
                if sentence is not None:
                    sentence_count += 1
                    for out in _emit_sentence(sentence, self._identity, self._speaker, seq):
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
                    for out in _emit_sentence(remainder, self._identity, self._speaker, seq):
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
                response_complete=kaguya_pb2.ResponseComplete(
                    turn_id=turn_id, was_interrupted=was_interrupted,
                ),
            )
        )

    # ──────────────────────────────────────────
    # PrefillCache + UpdatePersona — unchanged unary RPCs
    # ──────────────────────────────────────────

    async def PrefillCache(self, request, context):
        prompt = assemble_prompt(request.context, self._persona)
        await self._llm.prefill(prompt)
        return kaguya_pb2.PrefillAck()

    async def UpdatePersona(self, request, context):
        self._persona = request
        self._identity = parse_identity_config(request.identity_md or "")
        logger.info(
            "Persona updated (soul=%d, identity=%d, memory=%d bytes)",
            len(request.soul_md), len(request.identity_md), len(request.memory_md),
        )
        return kaguya_pb2.PersonaAck()

    async def close(self) -> None:
        await self._llm.close()


def _emit_sentence(
    sentence: str, identity: IdentityConfig, speaker: Speaker, seq: int,
) -> Generator[kaguya_pb2.TalkerOutput, None, None]:
    result: SoulContainerResult = soul_process(sentence, identity)
    if result.spoken_text:
        speaker.speak(result.spoken_text)
    seq += 1
    yield kaguya_pb2.TalkerOutput(
        seq=seq, sentence=kaguya_pb2.SentenceEvent(text=result.spoken_text),
    )
    for emotion in result.emotions:
        seq += 1
        yield kaguya_pb2.TalkerOutput(
            seq=seq, emotion=kaguya_pb2.EmotionEvent(emotion=emotion),
        )
    for tool_req in result.tool_requests:
        seq += 1
        yield kaguya_pb2.TalkerOutput(seq=seq, tool_request=tool_req)
    for delegate_req in result.delegate_requests:
        seq += 1
        yield kaguya_pb2.TalkerOutput(seq=seq, delegate_request=delegate_req)
