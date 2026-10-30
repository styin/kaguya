"""Tests for turn_detector.py thread safety and lifecycle correctness.

Exercises the race conditions between the recorder thread (on_partial)
and the event loop thread (on_silence_tick) that share TurnDetector state.
See REF-007 for background on the threading model.
"""

import threading
import time

from config import TalkerConfig
from voice.turn_detector import TurnDetector


def _make_detector(**overrides) -> TurnDetector:
    return TurnDetector(TalkerConfig(**overrides))


# ──────────────────────────────────────────
# Concurrent partial updates and silence ticks emit once
# ──────────────────────────────────────────


def test_concurrent_emit_only_produces_one_result():
    """Two threads race into _evaluate simultaneously. Only one should emit."""
    td = _make_detector()
    td.on_speech_start()
    td._buffer = "Hello there."
    td.on_vad_stop()
    # Push silence past the unconditional threshold so _evaluate always emits.
    td._vad_stop_ts = time.monotonic() - 1.0

    results: list[str | None] = [None, None]
    barrier = threading.Barrier(2)

    def race(index: int, method):
        barrier.wait()  # both threads start at the same instant
        results[index] = method()

    t1 = threading.Thread(
        target=race, args=(0, lambda: td.on_silence_tick(td.utterance_epoch))
    )
    t2 = threading.Thread(target=race, args=(1, lambda: td.on_partial("Hello there.")))
    t1.start()
    t2.start()
    t1.join(timeout=2)
    t2.join(timeout=2)

    # Exactly one should have emitted "Hello there.", the other should be None.
    non_none = [r for r in results if r is not None]
    assert len(non_none) == 1, (
        f"Expected exactly 1 emit, got {len(non_none)}: {results}"
    )
    assert non_none[0] == "Hello there."


def test_concurrent_emit_stress():
    """Run the race 50 times to catch intermittent failures."""
    for _ in range(50):
        td = _make_detector()
        td.on_speech_start()
        td._buffer = "Done."
        td.on_vad_stop()
        td._vad_stop_ts = time.monotonic() - 1.0

        results: list[str | None] = [None, None]
        barrier = threading.Barrier(2)

        def race(index: int, method, *, barrier=barrier, results=results):
            barrier.wait()
            results[index] = method()

        t1 = threading.Thread(
            target=race,
            args=(
                0,
                lambda detector=td: detector.on_silence_tick(detector.utterance_epoch),
            ),
        )
        t2 = threading.Thread(
            target=race,
            args=(1, lambda td=td: td.on_partial("Done.")),
        )
        t1.start()
        t2.start()
        t1.join(timeout=2)
        t2.join(timeout=2)

        non_none = [r for r in results if r is not None]
        assert len(non_none) == 1


# ──────────────────────────────────────────
# Stale tick loop: utterance_epoch prevents old loops from emitting
# ──────────────────────────────────────────


def test_utterance_epoch_increments_on_speech_start():
    td = _make_detector()
    assert td.utterance_epoch == 0
    td.on_speech_start()
    assert td.utterance_epoch == 1
    td.on_speech_start()
    assert td.utterance_epoch == 2


def test_stale_tick_loop_exits_on_utterance_epoch_mismatch():
    """An old tick cannot emit a new utterance, even if its silence is sufficient."""
    td = _make_detector()

    # Utterance 1
    td.on_speech_start()
    utterance_epoch_at_vad_stop = td.utterance_epoch  # 1
    td._buffer = "Hello."
    td.on_vad_stop()

    # Utterance 2 starts before tick fires
    td.on_speech_start()  # utterance_epoch → 2

    # The old loop can race with speech start after its external epoch check.
    # The detector must validate the epoch inside the same lock as emission.
    td._vad_stop_ts = time.monotonic() - 1.0
    td._buffer = "New text."
    assert td.on_silence_tick(utterance_epoch_at_vad_stop) is None
    assert not td.has_emitted
    assert td.on_silence_tick(td.utterance_epoch) == "New text."


def test_rapid_vad_cycling_no_stale_state():
    """Rapid vad_start → vad_stop → vad_start cycling. No stale emissions."""
    td = _make_detector()

    # Cycle 1: start, partial, stop
    td.on_speech_start()
    td.on_partial("First")
    td.on_vad_stop()

    # Cycle 2: immediately start again before any tick fires
    td.on_speech_start()
    assert td._buffer == ""
    assert td._vad_stop_ts is None
    assert td._emitted is False
    assert td.has_emitted is False

    # Cycle 2: new partial
    td.on_partial("Second utterance.")
    assert td._buffer == "Second utterance."

    # Cycle 2: vad stop + enough silence
    td.on_vad_stop()
    td._vad_stop_ts = time.monotonic() - 0.4  # in ambiguous zone
    result = td.on_partial("Second utterance.")
    assert result == "Second utterance."

    # Should not emit again
    assert td.on_silence_tick(td.utterance_epoch) is None


# ──────────────────────────────────────────
# has_emitted guard: post-emit calls are no-ops
# ──────────────────────────────────────────


def test_on_partial_after_emit_is_noop():
    td = _make_detector()
    td.on_speech_start()
    td._buffer = "Done."
    td.on_vad_stop()
    td._vad_stop_ts = time.monotonic() - 1.0

    # First call emits
    result1 = td.on_silence_tick(td.utterance_epoch)
    assert result1 == "Done."
    assert td.has_emitted is True

    # Subsequent calls are no-ops
    assert td.on_partial("Updated text.") is None
    assert td.on_silence_tick(td.utterance_epoch) is None

    # Buffer should NOT have been updated (check is before assignment)
    assert td._buffer == "Done."


def test_on_silence_tick_after_emit_is_noop():
    td = _make_detector()
    td.on_speech_start()
    td.on_vad_stop()
    td._vad_stop_ts = time.monotonic() - 0.4

    # Emit via on_partial
    result = td.on_partial("Complete sentence.")
    assert result == "Complete sentence."

    # Tick should be no-op
    assert td.on_silence_tick(td.utterance_epoch) is None
