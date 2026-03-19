"""server.py — TalkerServiceServicer: wires Gateway ↔ inference ↔ voice.

gRPC role: SERVER — receives ProcessPrompt, Prepare, PrefillCache, UpdatePersona
from the Gateway.

Domain role: Orchestrates the per-turn pipeline: prompt formatting → LLM streaming
→ sentence detection → soul container → TTS + gRPC output. Handles PREPARE
cancellation and persona updates.
"""

import asyncio
import logging

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
from prepare import PrepareHandler
from voice.speaker import Speaker

from proto import kaguya_pb2, kaguya_pb2_grpc  # type: ignore[import]

logger = logging.getLogger(__name__)


class TalkerServiceServicer(kaguya_pb2_grpc.TalkerServiceServicer):
    """Implements all four TalkerService RPCs.

    State:
        - _persona: cached PersonaConfig (updated via UpdatePersona)
        - _identity: parsed IdentityConfig from identity_md
        - _cancel_event: asyncio.Event, set by Prepare to interrupt ProcessPrompt
    """

    def __init__(self, config: TalkerConfig, speaker: Speaker) -> None:
        self._config = config  # config.py
        self._llm = LLMClient(config)  #
        self._speaker = speaker  # voice.speaker.Speaker
        self._persona = (
            kaguya_pb2.PersonaConfig()
        )  # str: SOUL.md, str: IDENTITY.md, str: MEMORY.md
        self._identity = IdentityConfig()  # inference.soul_container.IdentityConfig
        self._cancel_event = asyncio.Event()  #

    @property
    def cancel_event(self) -> asyncio.Event:
        """Exposed for PrepareHandler."""
        return self._cancel_event

    # ──────────────────────────────────────────
    # ProcessPrompt — main inference pipeline
    # ──────────────────────────────────────────

    async def ProcessPrompt(
        self,
        request: kaguya_pb2.TalkerContext,
        context: grpc.aio.ServicerContext,
    ):
        """
        Format prompt → stream tokens → sentences → soul container → yield TalkerOutput.

        Args:
            request: kaguya_pb2.TalkerContext (full turn context)
            context: grpc.aio.ServicerContext (gRPC context)

        Yields:
            kaguya_pb2.TalkerOutput (one per sentence)

        Raises:
            ConnectionError: if LLM connection fails
            Exception: if unexpected error occurs
        """
        self._cancel_event.clear()  # clears from previous PREPARE event
        self._speaker.reset()

        seq: int = 0  # increments with each yield, used for ... TODO
        sentence_count: int = 0  # increments with each sentence, used for ... TODO
        turn_id = request.turn_id  # unique per ProcessPrompt call

        # Yield `ResponseStarted`
        seq += 1
        yield kaguya_pb2.TalkerOutput(
            seq=seq,
            response_started=kaguya_pb2.ResponseStarted(turn_id=turn_id),
        )

        prompt = assemble_prompt(request, self._persona)
        detector = SentenceDetector()
        was_interrupted = False

        # LLM streaming task
        try:
            async for token in self._llm.stream_completion(
                prompt, cancel_event=self._cancel_event
            ):
                # Check for PREPARE cancellation between tokens.
                # Note: cancel_event is also checked inside stream_completion's
                # SSE parser, so cancellation is responsive even when the LLM
                # is slow to produce the next token.
                if self._cancel_event.is_set():
                    was_interrupted = True
                    break

                sentence = detector.feed(
                    token
                )  # Returns `None` until a sentence is detected
                if sentence is not None:
                    sentence_count += 1

                    # Emit sentence for post-processing -> TalkerOutput + TTS
                    for out in _emit_sentence(
                        sentence, self._identity, self._speaker, seq
                    ):
                        yield out
                    seq = out.seq

                    # Truncation of over-long responses
                    if sentence_count >= self._config.max_response_sentences:
                        was_interrupted = True
                        break

                    # Check cancellation between sentences.
                    if self._cancel_event.is_set():
                        was_interrupted = True
                        break

            # Flush remaining buffer on clean completion.
            # Note: sentence detection boundaries only detect up to the penultimate sentence.
            if not was_interrupted:
                remainder = detector.flush()
                if remainder:
                    sentence_count += 1
                    if sentence_count <= self._config.max_response_sentences:
                        for out in _emit_sentence(
                            remainder, self._identity, self._speaker, seq
                        ):
                            yield out
                        seq = out.seq

        except ConnectionError as exc:
            logger.error("LLM connection failed during generation: %s", exc)
            was_interrupted = True
        except Exception as exc:
            logger.error("Unexpected error during generation: %s", exc)
            was_interrupted = True

        # Yield `ResponseComplete` as the final message.
        seq += 1
        yield kaguya_pb2.TalkerOutput(
            seq=seq,
            response_complete=kaguya_pb2.ResponseComplete(
                turn_id=turn_id,
                was_interrupted=was_interrupted,
            ),
        )

    # ──────────────────────────────────────────
    # Prepare — interrupt / warm-up signal
    # ──────────────────────────────────────────

    async def Prepare(
        self,
        request: kaguya_pb2.PrepareSignal,
        context: grpc.aio.ServicerContext,
    ) -> kaguya_pb2.PrepareAck:
        """Cancel in-flight generation and TTS; return spoken/unspoken split."""
        handler = PrepareHandler(self._cancel_event, self._speaker)
        return handler.handle()

    # ──────────────────────────────────────────
    # PrefillCache — KV cache warming
    # ──────────────────────────────────────────

    async def PrefillCache(
        self,
        request: kaguya_pb2.PrefillRequest,
        context: grpc.aio.ServicerContext,
    ) -> kaguya_pb2.PrefillAck:
        """Forward prefill request to LLM server (n_predict=0)."""
        prompt = assemble_prompt(request.context, self._persona)
        await self._llm.prefill(prompt)
        return kaguya_pb2.PrefillAck()

    # ──────────────────────────────────────────
    # UpdatePersona — persona config reload
    # ──────────────────────────────────────────

    async def UpdatePersona(
        self,
        request: kaguya_pb2.PersonaConfig,
        context: grpc.aio.ServicerContext,
    ) -> kaguya_pb2.PersonaAck:
        """Update cached persona and re-parse identity config."""
        self._persona = request
        self._identity = parse_identity_config(request.identity_md or "")
        logger.info(
            "Persona updated (soul=%d, identity=%d, memory=%d bytes)",
            len(request.soul_md),
            len(request.identity_md),
            len(request.memory_md),
        )
        return kaguya_pb2.PersonaAck()

    async def close(self) -> None:
        """Cleanup resources."""
        await self._llm.close()


def _emit_sentence(
    sentence: str,
    identity: IdentityConfig,
    speaker: Speaker,
    seq: int,
) -> int:
    """Process a sentence through the soul container and yield TalkerOutput messages.

    Returns the updated sequence number.

    Yield order per spec: SentenceEvent first, then EmotionEvent/ToolRequest/
    DelegateRequest extracted from that sentence.
    """
    result: SoulContainerResult = soul_process(sentence, identity)

    # Feed spoken text to TTS.
    if result.spoken_text:
        speaker.speak(result.spoken_text)

    # Yield SentenceEvent.
    seq += 1
    yield kaguya_pb2.TalkerOutput(
        seq=seq,
        sentence=kaguya_pb2.SentenceEvent(text=result.spoken_text),
    )

    # Yield emotion events.
    for emotion in result.emotions:
        seq += 1
        yield kaguya_pb2.TalkerOutput(
            seq=seq,
            emotion=kaguya_pb2.EmotionEvent(emotion=emotion),
        )

    # Yield tool requests.
    for tool_req in result.tool_requests:
        seq += 1
        yield kaguya_pb2.TalkerOutput(
            seq=seq,
            tool_request=tool_req,
        )

    # Yield delegate requests.
    for delegate_req in result.delegate_requests:
        seq += 1
        yield kaguya_pb2.TalkerOutput(
            seq=seq,
            delegate_request=delegate_req,
        )

    return seq
