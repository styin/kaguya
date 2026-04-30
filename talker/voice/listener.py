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

from config import TalkerConfig
from voice.opus_decoder import OpusDecoder
from voice.turn_detector import TurnDetector

from proto import kaguya_pb2, kaguya_pb2_grpc  # type: ignore[import]

logger = logging.getLogger(__name__)


class ListenerServiceImpl(kaguya_pb2_grpc.ListenerServiceServicer):
    """gRPC server: yields ASR events to Gateway, reads control signals."""

    def __init__(self, event_queue: asyncio.Queue) -> None:
        self._event_queue = event_queue

    async def Stream(self, request_iterator, context):
        async def read_control():
            async for msg in request_iterator:
                which = msg.WhichOneof("payload")
                if which == "control":
                    ctrl = msg.control
                    if ctrl.HasField("reset"):
                        logger.info("Listener reset signal received")

        control_task = asyncio.create_task(read_control())
        try:
            while True:
                event = await self._event_queue.get()
                yield event
        except asyncio.CancelledError:
            pass
        finally:
            control_task.cancel()


class Listener:
    """Owns RealtimeSTT, turn detection, audio socket, and ASR event queue."""

    def __init__(self, config: TalkerConfig) -> None:
        self._config = config
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
        logger.info("Audio client connected")
        try:
            while True:
                length_bytes = await reader.readexactly(4)
                length = struct.unpack(">I", length_bytes)[0]
                if length == 0:
                    break
                frame = await reader.readexactly(length)
                pcm = self._opus.decode(frame)
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
            on_vad_detect_start=self._on_vad_start,
            on_vad_detect_stop=self._on_vad_stop,
            on_realtime_transcription_update=self._on_partial,
        )
        self._recorder = recorder
        logger.info("RealtimeSTT recorder started (feed_audio mode)")
        recorder.start()

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
            turn_id = self._turn_detector.turn_id
            asyncio.run_coroutine_threadsafe(
                self._silence_tick_loop(turn_id), self._loop
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

    async def _silence_tick_loop(self, started_turn_id: int) -> None:
        tick_s = self._config.silence_tick_interval_ms / 1000.0
        while True:
            await asyncio.sleep(tick_s)
            if self._turn_detector.turn_id != started_turn_id:
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