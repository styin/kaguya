import { useMemo } from "react";
import {
  actions,
  audioInBytesForTurn,
  selectTurns,
  useStore,
} from "../../store";
import { TimelineStrip } from "./TimelineStrip";
import { TurnRow, turnBody, turnKey } from "./TurnRow";
import "./turnspanel.css";

export function TurnsPanel() {
  const events = useStore((s) => s.events);
  const audioEvents = useStore((s) => s.audioEvents);
  const selTurn = useStore((s) => s.selTurn);
  const searchQuery = useStore((s) => s.searchQuery);

  const turns = useMemo(() => selectTurns(events), [events]);

  const filtered = useMemo(() => {
    const q = searchQuery.trim().toLowerCase();
    if (!q) return turns;
    return turns.filter((t) => turnBody(t).toLowerCase().includes(q));
  }, [turns, searchQuery]);

  const reversed = useMemo(() => filtered.slice().reverse(), [filtered]);

  const counts = useMemo(() => {
    let user = 0;
    let assistant = 0;
    for (const t of turns) (t.kind === "user" ? user++ : assistant++);
    return { user, assistant, total: turns.length };
  }, [turns]);

  return (
    <section className="turnspanel">
      <div className="timeline-wrap">
        <TimelineStrip
          turns={turns}
          onSelect={actions.setSelTurn}
          selectedKey={selTurn}
        />
      </div>
      <div className="turns-toolbar">
        <span className="turns-toolbar-label">Transcript</span>
        <input
          className="turns-search"
          value={searchQuery}
          onChange={(e) => actions.setSearchQuery(e.target.value)}
          placeholder="Search transcript…"
        />
        <span className="turns-summary">
          <b>{counts.total}</b> turns · <b>{counts.assistant}</b> assistant · <b>{counts.user}</b> user
        </span>
        <button
          type="button"
          className="turns-export"
          disabled
          title="Export not implemented (see console/README.md → Future work)"
        >
          Export
        </button>
      </div>
      <div className="turns-list">
        {reversed.map((t) => (
          <TurnRow
            key={turnKey(t)}
            turn={t}
            selectedKey={selTurn}
            onSelect={actions.setSelTurn}
            audioEvents={audioEvents}
            audioInBytes={t.kind === "assistant" ? audioInBytesForTurn(audioEvents, t) : 0}
          />
        ))}
      </div>
    </section>
  );
}
