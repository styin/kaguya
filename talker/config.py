from pydantic_settings import BaseSettings


class TalkerConfig(BaseSettings):
    # ── Listener / STT ──
    whisper_model: str = "distil-large-v3"
    whisper_compute_type: str = "int8"
    whisper_language: str = "en"
    # ── Turn detection ──
    silence_threshold_ms: int = 800
    syntax_silence_threshold_ms: int = 300
    silence_tick_interval_ms: int = 50  # polling granularity
    # ── Inference / LLM ──
    llm_base_url: str = "http://localhost:8080"
    llm_max_tokens: int = 256  # safety cap passed to llama.cpp n_predict
    llm_timeout: float = 30.0  # httpx timeout for LLM requests (seconds)
    llm_max_retries: int = 3
    llm_retry_delay: float = 1.0
    max_response_sentences: int = 4  # REF-006: voice brevity (~2-4 sentences)
    # ── Speaker / TTS ──
    # [OPEN] Voice selection requires listening tests after M3.6 lands (see OQ2).
    kokoro_voice: str = "af_heart"
    # ── Infrastructure ──
    gateway_socket: str = "127.0.0.1:50051"
    talker_listen_addr: str = "0.0.0.0:50053"
    gateway_reconnect_initial_s: float = 1.0
    gateway_reconnect_multiplier: float = 2.0
    gateway_reconnect_max_s: float = 30.0
    log_level: str = "INFO"

    model_config = {"env_prefix": "KAGUYA_", "env_file": ".env"}
