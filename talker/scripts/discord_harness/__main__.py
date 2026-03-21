"""discord_harness.py — Discord voice bot test harness for the Talker pipeline.

Exercises the full M1-M3 pipeline (OpusDecoder, TurnDetector, LLM, TTS) through
a Discord voice channel. Discord sends per-user opus frames; we decode, transcribe,
run inference, and send TTS audio back.

Single-user mode: all audio fed to one shared recorder, transcripts attributed
to last active speaker.

Usage:
    conda activate kaguya-discord
    cd talker/scripts/discord_harness
    DISCORD_BOT_TOKEN=... KAGUYA_LLM_BASE_URL=http://... uv run python .

Commands (in any text channel):
    !join   — bot joins your voice channel
    !leave  — bot disconnects

Requires:
    - Discord bot token in DISCORD_BOT_TOKEN env var
    - llama.cpp or compatible server at KAGUYA_LLM_BASE_URL
    - System deps: ffmpeg, libopus-dev, espeak-ng
"""

# ruff: noqa: E402 — sys.path manipulation must precede project imports

from __future__ import annotations

import argparse
import asyncio
import logging
import os
import sys
import threading
import time
import uuid
from pathlib import Path
from typing import Any

import numpy as np
from dotenv import dotenv_values

# Load .env from the discord_harness directory as a dict (not into os.environ).
# TalkerConfig's pydantic-settings reads os.environ with KAGUYA_ prefix and also
# reads any .env in cwd — we selectively forward only KAGUYA_-prefixed vars to
# os.environ so TalkerConfig picks them up, and keep DISCORD_BOT_TOKEN separate.
_dotenv_path = Path(__file__).resolve().parent / ".env"
_dotenv = dotenv_values(_dotenv_path)
_DISCORD_BOT_TOKEN: str | None = _dotenv.get("DISCORD_BOT_TOKEN") or os.environ.get("DISCORD_BOT_TOKEN")
for _k, _v in _dotenv.items():
    if _v and _k.startswith("KAGUYA_") and _k not in os.environ:
        os.environ[_k] = _v

# Ensure talker/ is on sys.path so imports resolve as in production.
# __main__.py lives at talker/scripts/discord_harness/__main__.py
_talker_dir = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(_talker_dir))

from config import TalkerConfig
from server import TalkerServiceServicer
from voice.turn_detector import TurnDetector
from proto import kaguya_pb2

import discord
import discord.ext.voice_recv as voice_recv

logger = logging.getLogger("discord_harness")

# ── Repo paths ──────────────────────────────────────────────────────────────
_REPO_ROOT = _talker_dir.parent
_SOUL_PATH = _REPO_ROOT / "config" / "SOUL.md"
_IDENTITY_PATH = _REPO_ROOT / "config" / "IDENTITY.md"
_MEMORY_PATH = _REPO_ROOT / "config" / "MEMORY.md"

# Discord audio constants
_DISCORD_FRAME_SIZE = 3840  # 20ms of 48kHz stereo int16 = 960 samples * 2 ch * 2 bytes
_SILENCE_FRAME = b"\x00" * _DISCORD_FRAME_SIZE


# ── Helpers (copied from test_harness.py) ───────────────────────────────────


def _read_optional(path: Path) -> str:
    """Read a file if it exists, otherwise return empty string."""
    if path.is_file():
        return path.read_text(encoding="utf-8")
    return ""


def _load_persona() -> kaguya_pb2.PersonaConfig:
    """Load persona config from repo config/ directory."""
    return kaguya_pb2.PersonaConfig(
        soul_md=_read_optional(_SOUL_PATH),
        identity_md=_read_optional(_IDENTITY_PATH),
        memory_md=_read_optional(_MEMORY_PATH),
    )


def _build_context(
    user_input: str,
    history: list[kaguya_pb2.ChatMessage],
    conversation_id: str,
) -> kaguya_pb2.TalkerContext:
    """Build a TalkerContext as the Gateway would."""
    return kaguya_pb2.TalkerContext(
        conversation_id=conversation_id,
        turn_id=str(uuid.uuid4()),
        user_input=user_input,
        history=history,
        memory_contents=_read_optional(_MEMORY_PATH),
        timestamp_ms=int(time.time() * 1000),
    )


class _FakeServicerContext:
    """Minimal stand-in for grpc.aio.ServicerContext."""

    def set_code(self, code: Any) -> None: pass
    def set_details(self, details: Any) -> None: pass
    def abort(self, code: Any, details: Any) -> None: pass
    def add_callback(self, cb: Any) -> None: pass
    def cancel(self) -> None: pass
    def is_active(self) -> bool: return True


# ── Audio resampling ────────────────────────────────────────────────────────


def _resample_24k_to_48k_stereo(pcm_24k_mono: bytes) -> bytes:
    """Convert 24kHz mono int16 PCM to 48kHz stereo int16 PCM.

    Strategy: 2x upsample via sample duplication, then duplicate mono to stereo.
    This is the simplest approach; quality is sufficient for voice.
    """
    samples = np.frombuffer(pcm_24k_mono, dtype=np.int16)
    # 2x upsample: duplicate each sample
    upsampled = np.repeat(samples, 2)
    # Mono to stereo: interleave L=R
    stereo = np.empty(len(upsampled) * 2, dtype=np.int16)
    stereo[0::2] = upsampled
    stereo[1::2] = upsampled
    return stereo.tobytes()


# ── _DiscordPCMSource ──────────────────────────────────────────────────────


class _DiscordPCMSource(discord.AudioSource):
    """Custom AudioSource that reads TTS audio from a queue.

    Discord expects read() to return 20ms of 48kHz stereo int16 PCM (3840 bytes).
    Returns silence when the queue is empty.
    """

    def __init__(self) -> None:
        self._queue: asyncio.Queue[bytes] = asyncio.Queue()
        self._buffer = bytearray()
        self._loop: asyncio.AbstractEventLoop | None = None

    def set_loop(self, loop: asyncio.AbstractEventLoop) -> None:
        self._loop = loop

    def push(self, data: bytes) -> None:
        """Push resampled 48kHz stereo PCM into the queue (thread-safe)."""
        if self._loop is not None:
            self._loop.call_soon_threadsafe(self._queue.put_nowait, data)
        else:
            self._queue.put_nowait(data)

    def read(self) -> bytes:
        """Return one 20ms frame (3840 bytes) of 48kHz stereo int16 PCM."""
        # Drain queue into buffer
        while not self._queue.empty():
            try:
                self._buffer.extend(self._queue.get_nowait())
            except asyncio.QueueEmpty:
                break

        if len(self._buffer) >= _DISCORD_FRAME_SIZE:
            frame = bytes(self._buffer[:_DISCORD_FRAME_SIZE])
            del self._buffer[:_DISCORD_FRAME_SIZE]
            return frame

        return _SILENCE_FRAME

    def is_opus(self) -> bool:
        return False

    def cleanup(self) -> None:
        self._buffer.clear()


# ── _DiscordSpeaker ────────────────────────────────────────────────────────


class _DiscordSpeaker:
    """Duck-types as Speaker — wraps KokoroEngine with muted output.

    TTS audio is intercepted via on_audio_chunk callback, resampled to
    48kHz stereo, and pushed to the _DiscordPCMSource for Discord playback.
    """

    def __init__(self, config: TalkerConfig, pcm_source: _DiscordPCMSource) -> None:
        from RealtimeTTS import TextToAudioStream, KokoroEngine

        self._pcm_source = pcm_source
        self._engine = KokoroEngine(voice=config.kokoro_voice)
        self._stream = TextToAudioStream(self._engine, muted=True)
        self._sentences: list[str] = []
        self._synthesized_count: int = 0
        self._playing: bool = False

    def _on_audio_chunk(self, chunk: bytes) -> None:
        """Called by RealtimeTTS for each audio chunk (24kHz mono int16).

        Resample to 48kHz stereo and push to the Discord PCM source.
        """
        if chunk:
            resampled = _resample_24k_to_48k_stereo(chunk)
            self._pcm_source.push(resampled)

    def _on_sentence_synthesized(self, _sentence: str = "") -> None:
        self._synthesized_count += 1

    def speak(self, text: str) -> None:
        """Feed one sentence to TTS for synthesis. Non-blocking."""
        self._sentences.append(text)
        self._stream.feed(text)
        if not self._playing or not self._stream.is_playing():
            self._stream.play_async(
                on_sentence_synthesized=self._on_sentence_synthesized,
                on_audio_chunk=self._on_audio_chunk,
            )
            self._playing = True

    def stop(self) -> tuple[str, str]:
        """Interrupt playback and return (spoken_text, unspoken_text)."""
        if not self._playing or not self._sentences:
            return ("", "")

        self._stream.stop()
        self._playing = False

        confirmed_played = max(0, self._synthesized_count - 1)
        spoken = " ".join(self._sentences[:confirmed_played])
        unspoken = " ".join(self._sentences[confirmed_played:])
        return (spoken, unspoken)

    def reset(self) -> None:
        """Reset state for a new turn."""
        if self._playing:
            self._stream.stop()
            self._playing = False
        self._sentences.clear()
        self._synthesized_count = 0


# ── _DiscordListener ───────────────────────────────────────────────────────


class _DiscordListener:
    """Receives opus frames from Discord, decodes, feeds RealtimeSTT + TurnDetector.

    Single shared recorder — tracks _active_user_name for transcript attribution.
    Follows _OpusFileListener pattern from test_harness.py.
    """

    def __init__(self, config: TalkerConfig, voice_client: Any) -> None:
        self._config = config
        self._voice_client = voice_client
        self._turn_detector = TurnDetector(config)
        self._transcript_queue: asyncio.Queue[str] = asyncio.Queue()
        self._loop: asyncio.AbstractEventLoop | None = None
        self._recorder: Any = None
        self._active_user_name: str = "Unknown"
        self._frame_count: int = 0
        self._dave_ok: int = 0
        self._dave_fail: int = 0
        self._decode_ok: int = 0
        self._decode_fail: int = 0

        # Debug: write decoded PCM to WAV for verification
        import wave
        self._debug_wav_path = Path(__file__).resolve().parent / "debug_audio.wav"
        self._debug_wav: Any = wave.open(str(self._debug_wav_path), "wb")
        self._debug_wav.setnchannels(1)
        self._debug_wav.setsampwidth(2)  # int16
        self._debug_wav.setframerate(16000)
        self._debug_wav_frames: int = 0
        self._debug_wav_max: int = 16000 * 10  # 10 seconds
        logger.info("Debug WAV: %s (first 10s of decoded audio)", self._debug_wav_path)

        # DAVE decryption — Discord E2E encryption (mandatory since March 2026).
        # voice_recv only does transport-level decryption; we must apply DAVE
        # decryption before opus decode.
        self._dave_session: Any = None

        # 48kHz stereo decoder for Discord's opus format.
        import opuslib
        self._opus_48k = opuslib.Decoder(fs=48000, channels=2)
        logger.info("OpusDecoder (48kHz stereo) initialized")

        # Silence injection — Discord uses DTX (no packets during silence).
        # RealtimeSTT needs silence frames to detect speech end.
        # 640 bytes = 320 samples * 2 bytes = 20ms of 16kHz mono silence.
        self._silence_frame = b"\x00" * 640
        self._last_audio_time: float = 0.0
        self._silence_feeder_task: Any = None

    @property
    def transcript_queue(self) -> asyncio.Queue[str]:
        return self._transcript_queue

    def feed_opus(self, user_id: int, user_name: str, opus_bytes: bytes) -> None:
        """Called from voice receive callback with raw opus data."""
        self._active_user_name = user_name
        self._frame_count += 1
        self._last_audio_time = time.monotonic()

        # Lazily grab the DAVE session from the voice client.
        if self._dave_session is None:
            conn = getattr(self._voice_client, "_connection", None)
            if conn is not None:
                self._dave_session = getattr(conn, "dave_session", None)
                if self._dave_session is not None:
                    logger.info("DAVE session acquired (ready=%s)", self._dave_session.ready)

        # DAVE decryption — strip E2E encryption before opus decode.
        if self._dave_session is not None:
            try:
                import davey
                opus_bytes = self._dave_session.decrypt(user_id, davey.MediaType.audio, opus_bytes)
                self._dave_ok += 1
            except Exception as e:
                self._dave_fail += 1
                if self._frame_count <= 10 or self._frame_count % 200 == 0:
                    logger.debug("DAVE decrypt failed: frame=%d size=%dB error=%s (ok=%d fail=%d)",
                                 self._frame_count, len(opus_bytes), e, self._dave_ok, self._dave_fail)
                return

        try:
            pcm = self._decode_opus(opus_bytes)
            self._decode_ok += 1
        except Exception:
            self._decode_fail += 1
            if self._frame_count <= 5:
                logger.exception("decode error on frame %d", self._frame_count)
            return

        if pcm and self._recorder is not None:
            if self._frame_count <= 5 or self._frame_count % 100 == 0:
                peak = int(np.max(np.abs(np.frombuffer(pcm, dtype=np.int16))))
                logger.info(
                    "audio stats: frame=%d pcm=%dB peak=%d dave_ok=%d dave_fail=%d decode_ok=%d",
                    self._frame_count, len(pcm), peak, self._dave_ok, self._dave_fail, self._decode_ok,
                )
            self._recorder.feed_audio(pcm)

            # Write to debug WAV
            if self._debug_wav is not None and self._debug_wav_frames < self._debug_wav_max:
                samples = np.frombuffer(pcm, dtype=np.int16)
                self._debug_wav.writeframes(pcm)
                self._debug_wav_frames += len(samples)
                if self._debug_wav_frames >= self._debug_wav_max:
                    self._debug_wav.close()
                    self._debug_wav = None
                    logger.info("Debug WAV complete: %s", self._debug_wav_path)

    def _decode_opus(self, opus_bytes: bytes) -> bytes:
        """Decode 48kHz stereo opus to 16kHz mono PCM for RealtimeSTT.

        Uses scipy.signal.resample_poly for fast anti-aliased downsampling
        (polyphase filter, O(N) — much faster than scipy.signal.resample).
        """
        import opuslib
        from scipy.signal import resample_poly

        try:
            # 48kHz * 0.02s = 960 samples per channel
            pcm_48k_stereo = self._opus_48k.decode(opus_bytes, frame_size=960)
        except opuslib.OpusError:
            if self._frame_count <= 10:
                logger.warning(
                    "Opus decode failed: frame=%d size=%dB first_bytes=%s",
                    self._frame_count, len(opus_bytes), opus_bytes[:16].hex(),
                )
            else:
                logger.debug("Opus decode failed for %d byte frame", len(opus_bytes))
            # Opus is stateful — a failed decode corrupts internal state.
            # Reset so it can recover on the next valid frame.
            self._opus_48k = opuslib.Decoder(fs=48000, channels=2)
            return b""

        # Stereo to mono: average L and R channels
        samples = np.frombuffer(pcm_48k_stereo, dtype=np.int16)
        left = samples[0::2].astype(np.int32)
        right = samples[1::2].astype(np.int32)
        mono = ((left + right) // 2).astype(np.int16)
        # 48kHz → 16kHz: resample_poly with up=1, down=3 (fast polyphase filter)
        downsampled = resample_poly(mono, 1, 3).astype(np.int16)
        return downsampled.tobytes()

    async def start(self) -> None:
        """Start the recorder thread and wait for it to be ready."""
        self._loop = asyncio.get_running_loop()
        recorder_ready = asyncio.Event()
        self._recorder_task = asyncio.create_task(
            asyncio.to_thread(self._recorder_thread, recorder_ready)
        )
        await recorder_ready.wait()
        # Start silence feeder — Discord uses DTX (no packets during silence),
        # but RealtimeSTT needs continuous audio to detect speech end.
        self._silence_feeder_task = asyncio.create_task(self._silence_feeder())
        logger.info("Discord listener started")

    def _recorder_thread(self, ready_event: asyncio.Event) -> None:
        from RealtimeSTT import AudioToTextRecorder

        recorder = AudioToTextRecorder(
            model=self._config.whisper_model,
            compute_type=self._config.whisper_compute_type,
            language=self._config.whisper_language,
            use_microphone=False,
            on_vad_detect_start=self._on_vad_start,
            on_vad_detect_stop=self._on_vad_stop,
            on_realtime_transcription_update=self._on_partial,
            use_extended_logging=True,
        )
        self._recorder = recorder
        if self._loop is not None:
            self._loop.call_soon_threadsafe(ready_event.set)
        # Use listen() not start() — start() puts the recorder in "recording"
        # state which skips VAD speech detection. listen() waits for VAD to
        # detect speech, then transitions to recording automatically.
        recorder.listen()
        # Keep thread alive — _recording_worker is a daemon thread that
        # needs the process to stay running.
        self._shutdown_event = threading.Event()
        self._shutdown_event.wait()

    async def _silence_feeder(self) -> None:
        """Feed silence frames when Discord stops sending audio (DTX).

        Discord doesn't send packets during silence. RealtimeSTT's VAD needs
        dense continuous audio to detect speech→silence transitions. We inject
        a burst of silence frames (1 second worth = 50 frames) when no real
        audio has arrived for more than 100ms, then wait before checking again.
        """
        while True:
            await asyncio.sleep(0.1)
            if self._recorder is None or self._last_audio_time == 0:
                continue
            gap = time.monotonic() - self._last_audio_time
            if gap > 0.1:  # 100ms gap → inject 1 second of silence as a burst
                for _ in range(50):  # 50 frames * 20ms = 1 second
                    self._recorder.feed_audio(self._silence_frame)
                logger.debug("silence burst injected: gap=%.1fs (50 frames)", gap)
                await asyncio.sleep(1.0)  # wait before next burst

    def _on_vad_start(self) -> None:
        logger.debug("VAD start (speaker: %s)", self._active_user_name)
        self._turn_detector.on_speech_start()

    def _on_vad_stop(self, silence_duration_ms: float = 0.0) -> None:
        logger.debug("VAD stop (silence: %.0fms)", silence_duration_ms)
        self._turn_detector.on_vad_stop()
        if self._loop is not None:
            turn_id = self._turn_detector.turn_id
            asyncio.run_coroutine_threadsafe(
                self._silence_tick_loop(turn_id), self._loop
            )

    def _on_partial(self, text: str) -> None:
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
        attributed = f"[{self._active_user_name}] {text}"
        logger.info("Final transcript: %s", attributed)
        if self._loop is not None:
            self._loop.call_soon_threadsafe(
                self._transcript_queue.put_nowait, attributed
            )

    async def stop(self) -> None:
        if self._silence_feeder_task is not None:
            self._silence_feeder_task.cancel()
        if self._debug_wav is not None:
            self._debug_wav.close()
            self._debug_wav = None
            logger.info("Debug WAV saved: %s (%d samples)", self._debug_wav_path, self._debug_wav_frames)
        if self._recorder is not None:
            self._recorder.shutdown()
        if hasattr(self, "_recorder_task"):
            self._recorder_task.cancel()


# ── _GuildSession ──────────────────────────────────────────────────────────


class _GuildSession:
    """Per-guild state: owns listener, speaker, servicer, and process loop."""

    def __init__(self, config: TalkerConfig, voice_client: discord.VoiceClient) -> None:
        self._config = config
        self._voice_client = voice_client
        self._pcm_source = _DiscordPCMSource()
        self._speaker = _DiscordSpeaker(config, self._pcm_source)
        self._listener = _DiscordListener(config, voice_client)
        self._servicer = TalkerServiceServicer(config, self._speaker)  # type: ignore[arg-type]
        self._history: list[kaguya_pb2.ChatMessage] = []
        self._conversation_id = str(uuid.uuid4())
        self._process_task: asyncio.Task[None] | None = None

    @property
    def listener(self) -> _DiscordListener:
        return self._listener

    async def start(self) -> None:
        """Initialize all components and start the process loop."""
        # Load persona
        persona = _load_persona()
        fake_ctx: Any = _FakeServicerContext()
        await self._servicer.UpdatePersona(persona, fake_ctx)
        logger.info(
            "Persona loaded (soul=%d, identity=%d bytes)",
            len(persona.soul_md),
            len(persona.identity_md),
        )

        # Start listener (recorder thread)
        await self._listener.start()

        # Prepare Discord audio output (start playback lazily to avoid
        # concurrent dave_session access — the audio player thread checks
        # can_encrypt which borrows the dave_session).
        self._pcm_source.set_loop(asyncio.get_running_loop())
        self._audio_started = False

        # Start process loop
        self._process_task = asyncio.create_task(self._process_loop())
        logger.info("Guild session started (conversation=%s)", self._conversation_id[:8])

    async def _process_loop(self) -> None:
        """Consume FinalTranscripts and drive ProcessPrompt."""
        fake_ctx: Any = _FakeServicerContext()

        while True:
            user_input = await self._listener.transcript_queue.get()
            print(f"\n{'─' * 60}")
            print(f"  USER: {user_input}")
            print(f"{'─' * 60}")

            ctx = _build_context(user_input, list(self._history), self._conversation_id)

            self._history.append(
                kaguya_pb2.ChatMessage(
                    role=kaguya_pb2.ROLE_USER,
                    content=user_input,
                    timestamp_ms=int(time.time() * 1000),
                )
            )

            assistant_text_parts: list[str] = []
            async for output in self._servicer.ProcessPrompt(ctx, fake_ctx):
                payload = output.WhichOneof("payload")
                if payload == "response_started":
                    print(f"  [seq={output.seq}] ResponseStarted (turn={output.response_started.turn_id[:8]}...)")
                elif payload == "sentence":
                    text = output.sentence.text
                    print(f"  [seq={output.seq}] Sentence: \"{text}\"")
                    if text:
                        assistant_text_parts.append(text)
                elif payload == "emotion":
                    print(f"  [seq={output.seq}] Emotion: {output.emotion.emotion}")
                elif payload == "tool_request":
                    tr = output.tool_request
                    print(f"  [seq={output.seq}] ToolRequest: {tr.tool_name}({tr.args_json})")
                elif payload == "delegate_request":
                    dr = output.delegate_request
                    print(f"  [seq={output.seq}] DelegateRequest: {dr.description}")
                elif payload == "response_complete":
                    rc = output.response_complete
                    interrupted = " (interrupted)" if rc.was_interrupted else ""
                    print(f"  [seq={output.seq}] ResponseComplete{interrupted}")

            full_response = " ".join(assistant_text_parts)
            if full_response:
                self._history.append(
                    kaguya_pb2.ChatMessage(
                        role=kaguya_pb2.ROLE_ASSISTANT,
                        content=full_response,
                        timestamp_ms=int(time.time() * 1000),
                    )
                )

            if len(self._history) > 20:
                del self._history[: len(self._history) - 20]

    async def stop(self) -> None:
        """Tear down all components."""
        if self._process_task is not None:
            self._process_task.cancel()
        await self._listener.stop()
        if self._voice_client.is_playing():
            self._voice_client.stop()
        self._pcm_source.cleanup()
        await self._servicer.close()
        logger.info("Guild session stopped")


# ── Discord Bot ────────────────────────────────────────────────────────────


class KaguyaBot(discord.Client):
    """Discord bot with !join and !leave commands for voice channel testing."""

    def __init__(self, config: TalkerConfig, **kwargs: Any) -> None:
        intents = discord.Intents.default()
        intents.message_content = True
        intents.voice_states = True
        super().__init__(intents=intents, **kwargs)
        self._config = config
        self._sessions: dict[int, _GuildSession] = {}  # guild_id → session

    async def on_ready(self) -> None:
        logger.info("Bot ready: %s (id=%s)", self.user, self.user.id if self.user else "?")
        print(f"\n  Bot is online as {self.user}. Use !join in a text channel.\n")

    async def on_message(self, message: discord.Message) -> None:
        if message.author == self.user or message.author.bot:
            return

        if message.content == "!join":
            await self._handle_join(message)
        elif message.content == "!leave":
            await self._handle_leave(message)

    async def _handle_join(self, message: discord.Message) -> None:
        member = message.guild.get_member(message.author.id) if message.guild else None
        if member is None or member.voice is None or member.voice.channel is None:
            await message.channel.send("You need to be in a voice channel first.")
            return

        guild = message.guild
        if guild is None:
            return

        voice_channel = member.voice.channel

        # Disconnect existing session if any
        if guild.id in self._sessions:
            await self._sessions[guild.id].stop()
            del self._sessions[guild.id]

        # Connect to voice channel using voice_recv for receiving audio
        vc = await voice_channel.connect(cls=voice_recv.VoiceRecvClient)  # type: ignore[arg-type]

        # Create and start session
        session = _GuildSession(self._config, vc)
        self._sessions[guild.id] = session
        await session.start()

        # Register voice receive callback
        def on_voice_packet(member: discord.Member | discord.User | None, data: voice_recv.VoiceData) -> None:
            if member is not None and data.opus is not None:
                session.listener.feed_opus(
                    user_id=member.id,
                    user_name=member.display_name,
                    opus_bytes=data.opus,
                )

        vc.listen(voice_recv.BasicSink(on_voice_packet, decode=False))  # type: ignore[arg-type]

        await message.channel.send(f"Joined **{voice_channel.name}**. Listening...")
        logger.info("Joined voice channel: %s (guild: %s)", voice_channel.name, guild.name)

    async def _handle_leave(self, message: discord.Message) -> None:
        guild = message.guild
        if guild is None:
            return

        if guild.id not in self._sessions:
            await message.channel.send("I'm not in a voice channel.")
            return

        session = self._sessions.pop(guild.id)
        await session.stop()

        if guild.voice_client is not None:
            await guild.voice_client.disconnect(force=False)

        await message.channel.send("Disconnected.")
        logger.info("Left voice channel (guild: %s)", guild.name)

    async def close(self) -> None:
        for session in self._sessions.values():
            await session.stop()
        self._sessions.clear()
        await super().close()


# ── Entry point ────────────────────────────────────────────────────────────


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Discord voice bot test harness for the Talker pipeline.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""\
examples:
  # Run the bot (from talker/scripts/discord_harness/):
  DISCORD_BOT_TOKEN=... uv run python .

  # With custom LLM endpoint:
  DISCORD_BOT_TOKEN=... KAGUYA_LLM_BASE_URL=http://... uv run python .

  # Verbose logging:
  DISCORD_BOT_TOKEN=... uv run python . -v
""",
    )
    parser.add_argument(
        "-v", "--verbose", action="store_true",
        help="Enable debug logging",
    )
    args = parser.parse_args()

    # Force logging config — libraries may have already called basicConfig.
    root = logging.getLogger()
    root.setLevel(logging.DEBUG if args.verbose else logging.INFO)
    handler = logging.StreamHandler()
    handler.setFormatter(logging.Formatter("%(asctime)s [%(name)s] %(levelname)s: %(message)s"))
    root.handlers = [handler]
    # Suppress noisy loggers
    for name in ("httpx", "httpcore", "grpc", "hpack", "discord.gateway", "discord.client"):
        logging.getLogger(name).setLevel(logging.WARNING)

    if not _DISCORD_BOT_TOKEN:
        print("ERROR: Set DISCORD_BOT_TOKEN in .env or environment.")
        sys.exit(1)

    # Change cwd to repo root so TalkerConfig's env_file=".env" doesn't
    # pick up discord_harness/.env (which has non-KAGUYA_ vars).
    os.chdir(_REPO_ROOT)
    config = TalkerConfig()
    bot = KaguyaBot(config)
    bot.run(_DISCORD_BOT_TOKEN, log_handler=None)


if __name__ == "__main__":
    main()
