"""voice/listener.py — Listener as gRPC SERVER + raw audio socket.

Gateway connects to us:
  - gRPC bidi (ListenerService.Stream): we send ASR events, Gateway sends control
  - Raw TCP socket: Gateway forwards audio bytes (bypasses gRPC serialization)
"""

import asyncio
import logging
import struct
import time

import grpc

from binding import AUDIO_MAGIC, InstanceSessionBinding
from config import TalkerConfig
from proto import kaguya_pb2, kaguya_pb2_grpc  # type: ignore[import]
from voice.opus_decoder import OpusDecoder
from voice.turn_detector import TurnDetector

logger = logging.getLogger(__name__)


class ListenerServiceImpl(kaguya_pb2_grpc.ListenerServiceServicer):
    """gRPC server: yields ASR events to Gateway, reads control signals.

    Single-active-client policy. The shared `_event_queue` is drained
    point-to-point — `asyncio.Queue.get()` delivers each enqueued event
    to exactly one waiting consumer, not broadcast. If two `Stream`
    handlers run concurrently (e.g. Gateway crashed and reconnected
    before the old TCP connection FIN'd), ASR events would be load-
    balanced between them, splitting the transcript across two
    connections.

    Pattern B fix: when a new `Stream` arrives, signal the prior handler
    to terminate and take ownership of the queue. The new (live) Gateway
    wins; the stale handler exits cleanly with EOF. Combined with the
    Gateway-side reconnect loop (planned in #17), this gives smooth
    handoff during Gateway restarts.
    """

    def __init__(
        self, event_queue: asyncio.Queue, binding: InstanceSessionBinding
    ) -> None:
        self._event_queue = event_queue
        self._binding = binding
        # Set whenever a new Stream takes over; the prior handler watches
        # this and exits when it fires.
        self._active_terminate: asyncio.Event | None = None

    async def Stream(self, request_iterator, context):
        request_iterator = aiter(request_iterator)
        try:
            first = await anext(request_iterator)
        except StopAsyncIteration:
            await context.abort(
                grpc.StatusCode.INVALID_ARGUMENT, "Listener requires init first"
            )
            return
        if first.WhichOneof("payload") != "init":
            await context.abort(
                grpc.StatusCode.INVALID_ARGUMENT, "Listener requires init first"
            )
        await self._binding.require(first.init.session, context)

        # Only a fully validated replacement may take the queue from a live
        # client. Returning headers now means initialization has been accepted.
        if self._active_terminate is not None:
            self._active_terminate.set()
        terminate = asyncio.Event()
        self._active_terminate = terminate
        await context.send_initial_metadata(())
        input_failure = asyncio.get_running_loop().create_future()

        async def read_control():
            try:
                async for msg in request_iterator:
                    if msg.WhichOneof("payload") != "control":
                        await context.abort(
                            grpc.StatusCode.INVALID_ARGUMENT,
                            "Listener permits exactly one init",
                        )
                    ctrl = msg.control
                    if ctrl.WhichOneof("signal") is None:
                        await context.abort(
                            grpc.StatusCode.INVALID_ARGUMENT,
                            "Listener requires a control signal",
                        )
                    if ctrl.HasField("reset"):
                        logger.info("Listener reset signal received")
            except Exception as exc:  # noqa: BLE001 - forwarded to the stream handler
                input_failure.set_result(exc)

        control_task = asyncio.create_task(read_control())
        get_task = None
        term_task = None
        try:
            while True:
                get_task = asyncio.create_task(self._event_queue.get())
                term_task = asyncio.create_task(terminate.wait())
                done, _ = await asyncio.wait(
                    {get_task, term_task, input_failure},
                    return_when=asyncio.FIRST_COMPLETED,
                )
                for task in (get_task, term_task):
                    if task not in done:
                        task.cancel()
                await asyncio.gather(get_task, term_task, return_exceptions=True)
                if input_failure in done or term_task in done:
                    # Cancellation/takeover must not consume an event intended
                    # for the live connection when both wake up together.
                    if not get_task.cancelled() and get_task.exception() is None:
                        self._event_queue.put_nowait(get_task.result())
                    if input_failure in done:
                        raise input_failure.result()
                    logger.info("Listener Stream replaced by newer client; exiting")
                    return
                yield get_task.result()
        except asyncio.CancelledError:
            pass
        finally:
            control_task.cancel()
            pending = [task for task in (get_task, term_task) if task is not None]
            for task in pending:
                task.cancel()
            await asyncio.gather(control_task, *pending, return_exceptions=True)
            if self._active_terminate is terminate:
                self._active_terminate = None


class Listener:
    """Owns RealtimeSTT, turn detection, audio socket, and ASR event queue."""

    def __init__(self, config: TalkerConfig, binding: InstanceSessionBinding) -> None:
        self._config = config
        self._binding = binding
        self._opus = OpusDecoder()
        self._turn_detector = TurnDetector(config)
        self._event_queue: asyncio.Queue[kaguya_pb2.ListenerOutput] = asyncio.Queue()
        self._recorder = None
        self._loop: asyncio.AbstractEventLoop | None = None

    @property
    def event_queue(self) -> asyncio.Queue:
        return self._event_queue

    async def run(self) -> None:
        self._loop = asyncio.get_running_loop()
        recorder_task = asyncio.create_task(self._run_recorder())
        audio_task = asyncio.create_task(self._run_audio_server())
        await asyncio.gather(recorder_task, audio_task)

    # ── Raw TCP audio socket server ──

    async def _run_audio_server(self) -> None:
        server = await asyncio.start_server(
            self._handle_audio_client,
            self._config.listener_audio_addr,
            self._config.listener_audio_port,
        )
        logger.info(
            "Audio socket listening on %s:%d",
            self._config.listener_audio_addr,
            self._config.listener_audio_port,
        )
        async with server:
            await server.serve_forever()

    async def _handle_audio_client(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        codec = self._config.audio_input_codec

        try:
            header = await reader.readexactly(len(AUDIO_MAGIC) + 32)
        except asyncio.IncompleteReadError:
            writer.close()
            await writer.wait_closed()
            return
        if not self._binding.matches_audio_header(header):
            logger.warning("Rejected audio socket with mismatched binding")
            writer.close()
            await writer.wait_closed()
            return
        logger.info("Audio client connected (codec=%s)", codec)
        try:
            while True:
                length_bytes = await reader.readexactly(4)
                length = struct.unpack(">I", length_bytes)[0]
                if length == 0:
                    break
                frame = await reader.readexactly(length)
                if codec == "opus":
                    pcm = self._opus.decode(frame)
                else:
                    # Raw PCM int16 LE 16kHz mono — feed directly.
                    pcm = frame
                if pcm and self._recorder is not None:
                    self._recorder.feed_audio(pcm)
        except (asyncio.IncompleteReadError, ConnectionResetError):
            logger.info("Audio client disconnected")

    # ── RealtimeSTT (blocking thread) ──

    async def _run_recorder(self) -> None:
        await asyncio.to_thread(self._recorder_thread)

    def _recorder_thread(self) -> None:
        from RealtimeSTT import AudioToTextRecorder

        recorder = AudioToTextRecorder(
            model=self._config.whisper_model,
            compute_type=self._config.whisper_compute_type,
            language=self._config.whisper_language,
            use_microphone=False,
            # `on_vad_start` / `on_vad_stop` are the actual voice-activity edge
            # callbacks. `on_vad_detect_start` / `on_vad_detect_stop` (despite
            # the name) fire on state-machine transitions in/out of the
            # "listening" state — i.e. when the recorder is ready to listen,
            # not when speech actually starts. Wiring those to the gateway
            # inverts the semantics and produces a deterministic phantom
            # `vad_speech_start` ~1ms after every final_transcript that
            # cancels the just-dispatched turn (B7). See RealtimeSTT
            # audio_recorder.py:457-464 for the docstring and 2076/2218 for
            # the real call sites.
            on_vad_start=self._on_vad_start,
            on_vad_stop=self._on_vad_stop,
            on_realtime_transcription_update=self._on_partial,
        )
        self._recorder = recorder
        # `recorder.text(cb)` drives RealtimeSTT's state machine
        # (listen → VAD-detect → record → transcribe → callback → repeat).
        # Calling listen()/start() once doesn't advance the pipeline — only
        # text() in a loop does. See RealtimeSTT_server/stt_server.py:599.
        logger.info("RealtimeSTT recorder ready (text-loop mode, feed_audio)")
        while True:
            try:
                recorder.text(self._on_full_transcript)
            except Exception:
                logger.exception("recorder.text() raised")
                # Brief pause so we don't tight-loop on a persistent failure.
                time.sleep(0.5)

    def _on_full_transcript(self, text: str) -> None:
        """Callback from recorder.text() with a complete utterance."""
        if not text:
            return
        self._emit_final(text)

    # ── Callbacks (from recorder thread) ──

    def _enqueue(self, event: kaguya_pb2.ListenerOutput) -> None:
        if self._loop is not None:
            self._loop.call_soon_threadsafe(self._event_queue.put_nowait, event)

    def _on_vad_start(self) -> None:
        self._turn_detector.on_speech_start()
        self._enqueue(
            kaguya_pb2.ListenerOutput(
                timestamp_ms=_now_ms(),
                vad_speech_start=kaguya_pb2.VadSpeechStart(),
            )
        )

    def _on_vad_stop(self, silence_duration_ms: float = 0.0) -> None:
        self._turn_detector.on_vad_stop()
        self._enqueue(
            kaguya_pb2.ListenerOutput(
                timestamp_ms=_now_ms(),
                vad_speech_end=kaguya_pb2.VadSpeechEnd(
                    silence_duration_ms=float(silence_duration_ms)
                ),
            )
        )
        if self._loop is not None:
            utterance_epoch = self._turn_detector.utterance_epoch
            asyncio.run_coroutine_threadsafe(
                self._silence_tick_loop(utterance_epoch), self._loop
            )

    def _on_partial(self, text: str) -> None:
        self._enqueue(
            kaguya_pb2.ListenerOutput(
                timestamp_ms=_now_ms(),
                partial_transcript=kaguya_pb2.PartialTranscript(text=text),
            )
        )
        final = self._turn_detector.on_partial(text)
        if final is not None:
            self._emit_final(final)

    async def _silence_tick_loop(self, started_utterance_epoch: int) -> None:
        tick_s = self._config.silence_tick_interval_ms / 1000.0
        while True:
            await asyncio.sleep(tick_s)
            if self._turn_detector.utterance_epoch != started_utterance_epoch:
                return
            final = self._turn_detector.on_silence_tick()
            if final is not None:
                self._emit_final(final)
                return
            if self._turn_detector.has_emitted:
                return

    def _emit_final(self, text: str) -> None:
        self._enqueue(
            kaguya_pb2.ListenerOutput(
                timestamp_ms=_now_ms(),
                final_transcript=kaguya_pb2.FinalTranscript(text=text, confidence=0.0),
            )
        )


def _now_ms() -> int:
    return int(time.time() * 1000)
