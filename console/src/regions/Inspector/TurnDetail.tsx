import type { AudioEvent, Turn } from "../../types";
import { firstAudioLatencyMs, turnDurationMs } from "../../store";

/**
 * Turn detail view: full sentence list with per-sentence relative
 * timestamps, plus turn-level metadata (id, duration, time-to-first-
 * sound). Tool / Reasoner sections are intentionally absent — those
 * events aren't on the wire yet (see console/README.md → Future work).
 */
export function TurnDetail({ turn, audioEvents }: { turn: Turn; audioEvents: AudioEvent[] }) {
  if (turn.kind === "user") {
    return (
      <div className="inspector">
        <div className="inspector-card">
          <div className="turn-detail-header">
            <span className="turn-detail-id">user · {turn.source}</span>
            <span className="turn-detail-meta">{formatAbs(turn.ts)}</span>
          </div>
          <div className="live-body">{turn.text}</div>
        </div>
      </div>
    );
  }

  const duration = turnDurationMs(turn);
  const ttfs = firstAudioLatencyMs(audioEvents, turn);

  return (
    <div className="inspector">
      <div className="inspector-card">
        <div className="turn-detail-header">
          <span className="turn-detail-id">{turn.turn_id}</span>
          <span className="turn-detail-meta">
            {duration === null ? "in flight" : `${(duration / 1000).toFixed(2)}s`}
            {ttfs !== null && ` · TTFS ${ttfs}ms`}
          </span>
        </div>
        <div className="inspector-section-label">Sentences</div>
        <div className="sentence-list">
          {turn.sentences.length === 0 ? (
            <div className="inspector-empty">no sentences yet</div>
          ) : (
            turn.sentences.map((s, i) => (
              <div className="sentence-row" key={i}>
                <span className="sentence-ts">+{((s.ts - turn.started_at) / 1000).toFixed(2)}s</span>
                <span>{s.text}</span>
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  );
}

function formatAbs(ms: number): string {
  return new Date(ms).toLocaleTimeString("en-US", { hour12: false });
}
