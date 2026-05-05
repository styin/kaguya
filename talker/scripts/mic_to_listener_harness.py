"""Temporary harness: microphone -> ListenerService stream.

================================================================================
DEPRECATED — broken after the Listener/Gateway role flip on this branch.
The Listener is now a gRPC server (in the Talker process); the Gateway pushes
audio over a raw TCP socket. This harness still dials the Gateway as a gRPC
client and uses removed proto symbols (`ListenerEvent`, `StreamEvents`).

Pending rewrite — should become either:
  (a) a Gateway-side audio-injection harness that pushes Opus frames to the
      Listener's raw TCP socket on `listener_audio_addr:listener_audio_port`,
      OR
  (b) a standalone audio source that connects to the Gateway's WS endpoint
      (`/ws`) and sends binary frames the same way the dev-GUI does.

Not imported by any production code path. Do not run as-is — it will fail at
import time on `kaguya_pb2.ListenerEvent` and at connect time on the wrong
gRPC role. Kept in tree as a starting point for the rewrite.
================================================================================
"""

from __future__ import annotations

import argparse
import asyncio
import json
import logging
import sys
import time
from pathlib import Path

import grpc
from RealtimeSTT import AudioToTextRecorder

# Ensure the talker/ directory is importable when this script is run from scripts/.
ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from config import TalkerConfig  # noqa: E402
from proto import kaguya_pb2, kaguya_pb2_grpc  # type: ignore[import]  # noqa: E402
from voice.turn_detector import TurnDetector  # noqa: E402

logger = logging.getLogger("mic-harness")


def _normalize_target(raw: str) -> str:
    """Map config target to a grpc.aio.insecure_channel-compatible endpoint.

    Temporary behavior: prefer TCP on Windows/local setups.
    - host:port is used as-is
    - explicit unix://... or path-like values fall back to 127.0.0.1:50051
    """
    value = raw.strip()
    if "://" in value and not value.startswith(("http://", "https://", "dns://")):
        logger.warning(
            "Non-TCP target '%s' detected; temporary fallback to TCP 127.0.0.1:50051",
            value,
        )
        return "127.0.0.1:50051"

    if "/" in value or "\\" in value:
        logger.warning(
            "Path-like target '%s' detected; temporary fallback to TCP 127.0.0.1:50051",
            value,
        )
        return "127.0.0.1:50051"

    return value


def _now_ms() -> int:
    return int(time.time() * 1000)


class MicHarness:
    def __init__(
        self,
        config: TalkerConfig,
        target: str,
        input_device_index: int | None,
        events_log_file: str | None,
    ) -> None:
        self._config = config
        self._target = _normalize_target(target)
        self._input_device_index = input_device_index
        self._events_log_file = events_log_file
        self._turn_detector = TurnDetector(config)
        self._event_queue: asyncio.Queue[kaguya_pb2.ListenerEvent] = asyncio.Queue()
        self._recorder: AudioToTextRecorder | None = None
        self._loop: asyncio.AbstractEventLoop | None = None
        self._stats = {
            "recording_start": 0,
            "recording_stop": 0,
            "vad_start": 0,
            "vad_stop": 0,
            "vad_detect_start": 0,
            "vad_detect_stop": 0,
            "partial": 0,
            "final": 0,
        }

    def _trace(self, event: str, **fields: object) -> None:
        self._stats[event] = self._stats.get(event, 0) + 1
        payload = {
            "ts_ms": _now_ms(),
            "event": event,
            **fields,
            "counts": self._stats,
        }
        line = json.dumps(payload, ensure_ascii=True)
        # Print is intentional so callback events are visible even when logger output
        # is noisy due to third-party spinner/progress text.
        print(f"[HARNESS] {line}", flush=True)
        logger.info("%s", line)
        if self._events_log_file:
            try:
                with open(self._events_log_file, "a", encoding="utf-8") as fp:
                    fp.write(line + "\n")
            except OSError as exc:
                logger.warning(
                    "Failed writing events log '%s': %s", self._events_log_file, exc
                )

    async def run(self) -> None:
        self._loop = asyncio.get_running_loop()
        recorder_task = asyncio.create_task(self._run_recorder())
        stream_task = asyncio.create_task(self._stream_to_gateway())
        heartbeat_task = asyncio.create_task(self._heartbeat())
        try:
            await asyncio.gather(recorder_task, stream_task, heartbeat_task)
        except asyncio.CancelledError:
            recorder_task.cancel()
            stream_task.cancel()
            heartbeat_task.cancel()
            if self._recorder is not None:
                self._recorder.shutdown()
            raise

    async def _heartbeat(self) -> None:
        while True:
            await asyncio.sleep(2.0)
            self._trace("heartbeat", queue_size=self._event_queue.qsize())

    async def _run_recorder(self) -> None:
        await asyncio.to_thread(self._recorder_thread)

    def _recorder_thread(self) -> None:
        if self._input_device_index is None:
            recorder = AudioToTextRecorder(
                model=self._config.whisper_model,
                compute_type=self._config.whisper_compute_type,
                language=self._config.whisper_language,
                use_microphone=True,
                enable_realtime_transcription=True,
                on_recording_start=self._on_recording_start,
                on_recording_stop=self._on_recording_stop,
                # RealtimeSTT callback compatibility:
                # - on_vad_start/on_vad_stop fire during active recording mode
                # - on_vad_detect_start/on_vad_detect_stop are tied to listening-state transitions
                on_vad_start=self._on_vad_start,
                on_vad_stop=self._on_vad_stop,
                on_vad_detect_start=self._on_vad_detect_start,
                on_vad_detect_stop=self._on_vad_detect_stop,
                on_realtime_transcription_update=self._on_partial,
                level=logging.INFO,
            )
        else:
            recorder = AudioToTextRecorder(
                model=self._config.whisper_model,
                compute_type=self._config.whisper_compute_type,
                language=self._config.whisper_language,
                use_microphone=True,
                input_device_index=self._input_device_index,
                enable_realtime_transcription=True,
                on_recording_start=self._on_recording_start,
                on_recording_stop=self._on_recording_stop,
                # RealtimeSTT callback compatibility:
                # - on_vad_start/on_vad_stop fire during active recording mode
                # - on_vad_detect_start/on_vad_detect_stop are tied to listening-state transitions
                on_vad_start=self._on_vad_start,
                on_vad_stop=self._on_vad_stop,
                on_vad_detect_start=self._on_vad_detect_start,
                on_vad_detect_stop=self._on_vad_detect_stop,
                on_realtime_transcription_update=self._on_partial,
                level=logging.INFO,
            )
        self._recorder = recorder
        logger.info("Microphone recorder started (VAD-managed mode)")

        # Use the library's text() loop so it controls listen -> recording -> stop
        # transitions. This path enables on_vad_start/on_vad_stop callbacks.
        while True:
            text = recorder.text()
            if text is None:
                continue
            text = text.strip()
            if not text:
                continue
            self._trace("text_complete", text=text)
            # Fallback: if our turn detector did not emit final for any reason,
            # publish the completed text turn returned by RealtimeSTT.
            if not self._turn_detector.has_emitted:
                self._emit_final(text)

    def _enqueue(self, event: kaguya_pb2.ListenerEvent) -> None:
        if self._loop is not None:
            self._loop.call_soon_threadsafe(self._event_queue.put_nowait, event)

    def _on_vad_start(self) -> None:
        self._trace("vad_start")
        self._turn_detector.on_speech_start()
        self._enqueue(
            kaguya_pb2.ListenerEvent(
                timestamp_ms=_now_ms(),
                vad_speech_start=kaguya_pb2.VadSpeechStart(),
            )
        )

    def _on_vad_stop(self, silence_duration_ms: float | None = None) -> None:
        silence_ms = 0.0 if silence_duration_ms is None else float(silence_duration_ms)
        self._trace("vad_stop", silence_duration_ms=silence_ms)
        self._turn_detector.on_vad_stop()
        self._enqueue(
            kaguya_pb2.ListenerEvent(
                timestamp_ms=_now_ms(),
                vad_speech_end=kaguya_pb2.VadSpeechEnd(silence_duration_ms=silence_ms),
            )
        )
        if self._loop is not None:
            turn_id = self._turn_detector.turn_id
            asyncio.run_coroutine_threadsafe(
                self._silence_tick_loop(turn_id), self._loop
            )

    def _on_vad_detect_start(self) -> None:
        # Keep detect events for observability, but do not treat them as turn boundaries.
        self._trace("vad_detect_start")

    def _on_vad_detect_stop(self, _silence_duration_ms: float | None = None) -> None:
        # Signature is version-dependent; ignore payload and keep this informational only.
        self._trace("vad_detect_stop")

    def _on_partial(self, text: str) -> None:
        self._trace("partial", text=text)
        self._enqueue(
            kaguya_pb2.ListenerEvent(
                timestamp_ms=_now_ms(),
                partial_transcript=kaguya_pb2.PartialTranscript(text=text),
            )
        )
        final = self._turn_detector.on_partial(text)
        if final is not None:
            self._emit_final(final)

    def _on_recording_start(self) -> None:
        self._trace("recording_start")

    def _on_recording_stop(self) -> None:
        self._trace("recording_stop")

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
        self._trace("final", text=text)
        self._enqueue(
            kaguya_pb2.ListenerEvent(
                timestamp_ms=_now_ms(),
                final_transcript=kaguya_pb2.FinalTranscript(text=text, confidence=0.0),
            )
        )

    async def _stream_to_gateway(self) -> None:
        backoff = self._config.gateway_reconnect_initial_s
        while True:
            try:
                logger.info("Connecting to ListenerService at %s", self._target)
                async with grpc.aio.insecure_channel(self._target) as channel:
                    stub = kaguya_pb2_grpc.ListenerServiceStub(channel)
                    await stub.StreamEvents(self._event_generator())
                    backoff = self._config.gateway_reconnect_initial_s
            except grpc.aio.AioRpcError as exc:
                logger.warning(
                    "Gateway connection lost (%s). Reconnecting in %.1fs...",
                    exc.code(),
                    backoff,
                )
                await asyncio.sleep(backoff)
                backoff = min(
                    backoff * self._config.gateway_reconnect_multiplier,
                    self._config.gateway_reconnect_max_s,
                )
            except asyncio.CancelledError:
                return

    async def _event_generator(self):
        while True:
            yield await self._event_queue.get()


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Temporary mic -> ListenerService harness"
    )
    parser.add_argument(
        "--target",
        default=None,
        help="Gateway ListenerService target (default: TalkerConfig.gateway_socket)",
    )
    parser.add_argument(
        "--log-level",
        default="INFO",
        choices=["DEBUG", "INFO", "WARNING", "ERROR"],
        help="Logging level",
    )
    parser.add_argument(
        "--input-device-index",
        type=int,
        default=None,
        help="Microphone input device index for RealtimeSTT/PyAudio",
    )
    parser.add_argument(
        "--events-log-file",
        default=None,
        help="Optional newline-delimited JSON log file for VAD/transcript events",
    )
    return parser.parse_args()


async def _main() -> None:
    args = _parse_args()
    logging.basicConfig(
        level=getattr(logging, args.log_level),
        format="%(asctime)s [%(name)s] %(levelname)s: %(message)s",
    )

    config = TalkerConfig()
    target = args.target or config.gateway_socket

    logger.info("Mic harness starting")
    logger.info("Configured gateway target: %s", target)

    if args.input_device_index is not None:
        logger.info("Using input_device_index=%d", args.input_device_index)
    if args.events_log_file:
        logger.info("Writing event trace to %s", args.events_log_file)

    harness = MicHarness(config, target, args.input_device_index, args.events_log_file)
    await harness.run()


if __name__ == "__main__":
    try:
        asyncio.run(_main())
    except KeyboardInterrupt:
        logger.info("Mic harness stopped")
