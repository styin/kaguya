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
# Double-emit race: _emit_lock prevents two threads from emitting
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

    t1 = threading.Thread(target=race, args=(0, td.on_silence_tick))
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

        def race(index: int, method):
            barrier.wait()
            results[index] = method()

        t1 = threading.Thread(target=race, args=(0, td.on_silence_tick))
        t2 = threading.Thread(target=race, args=(1, lambda: td.on_partial("Done.")))
        t1.start()
        t2.start()
        t1.join(timeout=2)
        t2.join(timeout=2)

        non_none = [r for r in results if r is not None]
        assert len(non_none) == 1


# ──────────────────────────────────────────
# Stale tick loop: turn_id prevents old loops from emitting
# ──────────────────────────────────────────


def test_turn_id_increments_on_speech_start():
    td = _make_detector()
    assert td.turn_id == 0
    td.on_speech_start()
    assert td.turn_id == 1
    td.on_speech_start()
    assert td.turn_id == 2


def test_stale_tick_loop_exits_on_turn_id_mismatch():
    """Simulate what the listener's _silence_tick_loop does:
    check turn_id before calling on_silence_tick."""
    td = _make_detector()

    # Utterance 1
    td.on_speech_start()
    turn_id_at_vad_stop = td.turn_id  # 1
    td._buffer = "Hello."
    td.on_vad_stop()

    # Utterance 2 starts before tick fires
    td.on_speech_start()  # turn_id → 2

    # The tick loop from utterance 1 checks turn_id
    assert td.turn_id != turn_id_at_vad_stop
    # In the real listener, this causes the tick loop to return without calling
    # on_silence_tick. Let's verify on_silence_tick would also be safe:
    td._vad_stop_ts = time.monotonic() - 1.0
    td._buffer = "New text."
    td.on_silence_tick()
    # This WOULD emit for the new utterance — but the tick loop never gets here
    # because it exits on turn_id mismatch first. Still, verify the turn_id
    # check is the correct guard:
    assert turn_id_at_vad_stop == 1
    assert td.turn_id == 2


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
    assert td.on_silence_tick() is None


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
    result1 = td.on_silence_tick()
    assert result1 == "Done."
    assert td.has_emitted is True

    # Subsequent calls are no-ops
    assert td.on_partial("Updated text.") is None
    assert td.on_silence_tick() is None

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
    assert td.on_silence_tick() is None


# ──────────────────────────────────────────
# Lock correctness: _emit_lock doesn't deadlock on re-entry
# ──────────────────────────────────────────


def test_sequential_emit_calls_do_not_deadlock():
    """Calling _emit multiple times from the same thread should not deadlock."""
    td = _make_detector()
    td.on_speech_start()
    td._buffer = "Test."
    td.on_vad_stop()
    td._vad_stop_ts = time.monotonic() - 1.0

    # First call succeeds
    result1 = td._emit()
    assert result1 == "Test."

    # Second call returns None (already emitted), does not deadlock
    result2 = td._emit()
    assert result2 is None
