from pydantic_settings import BaseSettings


class TalkerConfig(BaseSettings):
    # ── Listener / STT ──
    whisper_model: str = "distil-large-v3"
    whisper_compute_type: str = "int8"
    whisper_language: str = "en"
    # ── Turn detection ──
    silence_threshold_ms: int = 800
    syntax_silence_threshold_ms: int = 300
    silence_tick_interval_ms: int = 50
    # ── Inference / LLM ──
    llm_base_url: str = "http://localhost:8080"
    llm_max_tokens: int = 256
    llm_timeout: float = 30.0
    llm_max_retries: int = 3
    llm_retry_delay: float = 1.0
    max_response_sentences: int = 4
    # ── Speaker / TTS ──
    kokoro_voice: str = "af_heart"
    # ── Infrastructure ──
    gateway_socket: str = "127.0.0.1:50051"
    talker_listen_addr: str = "0.0.0.0:50053"
    # Listener is now a gRPC server (Gateway connects to us)
    listener_grpc_addr: str = "0.0.0.0:50055"
    listener_audio_addr: str = "0.0.0.0"
    listener_audio_port: int = 50056
    # Reconnect settings (for Listener→Gateway gRPC, kept for harness compat)
    gateway_reconnect_initial_s: float = 1.0
    gateway_reconnect_multiplier: float = 2.0
    gateway_reconnect_max_s: float = 30.0
    log_level: str = "INFO"

    model_config = {"env_prefix": "KAGUYA_", "env_file": ".env"}