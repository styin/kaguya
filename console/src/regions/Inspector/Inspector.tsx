import { useMemo } from "react";
import { selectStreamingTurn, selectTurns, useStore } from "../../store";
import { turnKey } from "../TurnsPanel/TurnRow";
import { LiveMonitor } from "./LiveMonitor";
import { TurnDetail } from "./TurnDetail";
import "./inspector.css";

export function Inspector() {
  const events = useStore((s) => s.events);
  const selTurn = useStore((s) => s.selTurn);

  const turns = useMemo(() => selectTurns(events), [events]);
  const streaming = useMemo(() => selectStreamingTurn(turns), [turns]);
  const latest = useMemo(() => {
    for (let i = turns.length - 1; i >= 0; i--) {
      const t = turns[i];
      if (t.kind === "assistant" && t.complete_at !== null) return t;
    }
    return null;
  }, [turns]);

  const selected = useMemo(
    () => (selTurn === null ? null : turns.find((t) => turnKey(t) === selTurn) ?? null),
    [turns, selTurn],
  );

  return selected
    ? <TurnDetail turn={selected} events={events} />
    : <LiveMonitor streaming={streaming} latest={latest} />;
}
