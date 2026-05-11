import { useEffect, useState } from "react";
import type { Turn } from "../../types";

// Visible window width in ms (right edge = NOW, left edge = NOW - WINDOW).
// Blocks beyond the left edge scroll off and are dropped.
const WINDOW_MS = 60_000;

const LANES = [
  { id: "user",     label: "User" },
  { id: "talker",   label: "Talker" },
  { id: "tool",     label: "Tool" },
  { id: "reasoner", label: "Reasoner" },
] as const;

/**
 * Timeline strip — 4-lane network-style timeline atop the turns panel.
 * Axis row labels NOW on the right, dropping back by quarters of the
 * visible window. User and Talker lanes plot from the event log; Tool
 * and Reasoner stay empty (skeleton — see console/README.md).
 */
export function TimelineStrip({
  turns,
  onSelect,
  selectedKey,
}: {
  turns: Turn[];
  onSelect: (key: string | null) => void;
  selectedKey: string | null;
}) {
  const now = useNow();
  const totalSec = WINDOW_MS / 1000;

  return (
    <div className="net-tl">
      <div className="net-axis-row">
        <span className="lane-gutter" />
        <div className="net-axis">
          <span>−{totalSec.toFixed(0)}s</span>
          <span>−{(totalSec * 0.66).toFixed(0)}s</span>
          <span>−{(totalSec * 0.33).toFixed(0)}s</span>
          <span>now</span>
        </div>
      </div>
      <div className="net-lanes">
        {LANES.map((L) => (
          <div key={L.id} className="net-lane">
            <span className="lane-gutter">{L.label}</span>
            <div className="net-track">
              {L.id === "user" &&
                turns
                  .filter((t) => t.kind === "user")
                  .map((t, i) =>
                    t.kind === "user" ? (
                      <Block
                        key={`u${i}`}
                        kind="user"
                        startMs={t.ts}
                        endMs={t.ts + 600}
                        now={now}
                        selected={selectedKey === `user:${t.ts}`}
                        onClick={() => onSelect(`user:${t.ts}`)}
                      />
                    ) : null,
                  )}
              {L.id === "talker" &&
                turns
                  .filter((t) => t.kind === "assistant")
                  .map((t, i) =>
                    t.kind === "assistant" ? (
                      <Block
                        key={`a${i}`}
                        kind={
                          t.complete_at === null
                            ? "talker live"
                            : t.interrupted
                              ? "interrupted"
                              : "talker"
                        }
                        startMs={t.started_at}
                        endMs={t.complete_at ?? now}
                        now={now}
                        selected={selectedKey === `assistant:${t.turn_id}`}
                        label={t.turn_id.slice(0, 8)}
                        onClick={() => onSelect(`assistant:${t.turn_id}`)}
                      />
                    ) : null,
                  )}
              {/* Tool and Reasoner lanes: skeleton — no blocks until the
                  gateway forwards `tool_call` / `reasoner_step` events. */}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

function Block({
  kind,
  startMs,
  endMs,
  now,
  selected,
  label,
  onClick,
}: {
  kind: string;
  startMs: number;
  endMs: number;
  now: number;
  selected: boolean;
  label?: string;
  onClick: () => void;
}) {
  const rightPct = ((now - endMs) / WINDOW_MS) * 100;
  const widthPct = Math.max(0.6, ((endMs - startMs) / WINDOW_MS) * 100);
  if (rightPct >= 100) return null;
  const leftPct = Math.max(0, 100 - rightPct - widthPct);
  return (
    <div
      className={"net-block " + kind + (selected ? " selected" : "")}
      style={{ left: `${leftPct}%`, width: `${Math.min(widthPct, 100 - leftPct)}%` }}
      onClick={(e) => { e.stopPropagation(); onClick(); }}
      title={label ?? kind}
    >
      {widthPct > 10 && label ? <span className="bl">{label}</span> : null}
    </div>
  );
}

function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);
  return now;
}
