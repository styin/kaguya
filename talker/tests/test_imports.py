"""Smoke test: verify all written modules import and instantiate correctly.

Run from talker/: python -m pytest tests/test_imports.py -v
Requires: pydantic-settings, opuslib, grpcio. Proto stubs are committed — grpcio-tools
not needed to run tests. Does NOT require RealtimeSTT, RealtimeTTS, or llama.cpp.
"""


def test_config_defaults():
    from config import TalkerConfig
    c = TalkerConfig()
    assert c.gateway_socket == "/tmp/kaguya-gateway.sock"
    assert c.silence_threshold_ms == 800
    assert c.syntax_silence_threshold_ms == 300
    assert c.llm_base_url == "http://localhost:8080"


def test_turn_detector_instantiates():
    from config import TalkerConfig
    from voice.turn_detector import TurnDetector
    td = TurnDetector(TalkerConfig())
    assert td._silence_ms == 800
    assert td._syntax_ms == 300


def test_turn_detector_no_emit_before_vad_stop():
    from config import TalkerConfig
    from voice.turn_detector import TurnDetector
    td = TurnDetector(TalkerConfig())
    td.on_speech_start()
    # No vad_stop yet — should never emit regardless of text
    result = td.on_partial("Hello, how are you?")
    assert result is None


def test_turn_detector_emits_after_vad_stop_complete_sentence():
    import time
    from config import TalkerConfig
    from voice.turn_detector import TurnDetector

    cfg = TalkerConfig()
    td = TurnDetector(cfg)
    td.on_speech_start()
    td.on_vad_stop()

    # Simulate 400ms silence (above 300ms syntax threshold)
    td._vad_stop_ts = time.monotonic() - 0.4

    result = td.on_partial("Hello, how are you?")
    assert result == "Hello, how are you?"


def test_turn_detector_waits_for_incomplete_sentence():
    import time
    from config import TalkerConfig
    from voice.turn_detector import TurnDetector

    td = TurnDetector(TalkerConfig())
    td.on_speech_start()
    td.on_vad_stop()

    # 400ms silence — in ambiguous zone, but sentence is incomplete (ends with "and")
    td._vad_stop_ts = time.monotonic() - 0.4

    result = td.on_partial("I want to go and")
    assert result is None


def test_turn_detector_unconditional_emit_at_800ms():
    import time
    from config import TalkerConfig
    from voice.turn_detector import TurnDetector

    td = TurnDetector(TalkerConfig())
    td.on_speech_start()
    td.on_vad_stop()

    # Simulate 900ms silence — above unconditional threshold
    td._vad_stop_ts = time.monotonic() - 0.9

    result = td.on_silence_tick()
    assert result is None  # buffer is empty — on_silence_tick guards with `not self._buffer`

    # Now with buffer populated
    td2 = TurnDetector(TalkerConfig())
    td2.on_speech_start()
    td2._buffer = "I want to go and"
    td2.on_vad_stop()
    td2._vad_stop_ts = time.monotonic() - 0.9

    result2 = td2.on_silence_tick()
    assert result2 == "I want to go and"


def test_opus_decoder_instantiates():
    from voice.opus_decoder import OpusDecoder
    dec = OpusDecoder()
    assert dec._decoder is not None


def test_proto_stubs_importable():
    from proto import kaguya_pb2  # type: ignore[import]
    # Instantiate a few key message types
    evt = kaguya_pb2.ListenerEvent()
    assert evt is not None

    ctx = kaguya_pb2.TalkerContext()
    assert ctx is not None

    out = kaguya_pb2.TalkerOutput()
    assert out is not None


def test_proto_stubs_services_exist():
    from proto import kaguya_pb2_grpc  # type: ignore[import]
    assert hasattr(kaguya_pb2_grpc, "ListenerServiceStub")
    assert hasattr(kaguya_pb2_grpc, "TalkerServiceServicer")
    assert hasattr(kaguya_pb2_grpc, "ReasonerServiceServicer")
