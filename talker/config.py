from pydantic_settings import BaseSettings


class TalkerConfig(BaseSettings):
    # Listener / STT implementation defaults.
    whisper_model: str = "distil-large-v3"
    whisper_compute_type: str = "int8"
    whisper_language: str = "en"

    # Turn detection implementation defaults.
    silence_threshold_ms: int = 800
    syntax_silence_threshold_ms: int = 300
    silence_tick_interval_ms: int = 50

    # Inference / LLM implementation defaults. Runtime launchers should
    # supply KAGUYA_LLM_BASE_URL from config/kaguya.runtime.toml.
    llm_base_url: str = "http://localhost:1234"
    llm_max_tokens: int = 256
    llm_timeout: float = 30.0
    llm_max_retries: int = 3
    llm_retry_delay: float = 1.0
    max_response_sentences: int = 4

    # Speaker / TTS implementation defaults.
    kokoro_voice: str = "af_heart"

    # Runtime bind defaults. In app/dev-console modes these are generated
    # from config/kaguya.runtime.toml [profiles.*.processes.voice_stack.bind]
    # and injected through KAGUYA_* env vars. Defaults exist only so direct
    # `python main.py` remains useful.
    talker_listen_addr: str = "0.0.0.0:50053"
    listener_grpc_addr: str = "0.0.0.0:50055"
    listener_audio_addr: str = "0.0.0.0"
    listener_audio_port: int = 50056

    # Wire format for audio frames on the listener audio socket. The dev
    # console sends raw PCM (int16 LE 16kHz mono); a future OpenPod /
    # Discord-harness sender will use Opus.
    audio_input_codec: str = "pcm"  # "pcm" | "opus"
    log_level: str = "INFO"

    model_config = {"env_prefix": "KAGUYA_", "env_file": ".env"}
