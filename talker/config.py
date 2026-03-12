from pydantic_settings import BaseSettings


class TalkerConfig(BaseSettings):
    llama_cpp_url: str = "http://localhost:8080"
    gateway_socket: str = "/tmp/kaguya-gateway.sock"
    silence_threshold_ms: int = 800
    syntax_silence_threshold_ms: int = 300
    # [OPEN] Voice selection requires listening tests after M3.6 lands (see OQ2).
    kokoro_voice: str = "af_heart"
    log_level: str = "INFO"

    model_config = {"env_prefix": "KAGUYA_", "env_file": ".env"}
