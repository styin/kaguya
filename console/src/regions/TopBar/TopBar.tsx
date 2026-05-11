import { useEffect, useMemo, useState } from "react";
import {
  selectWsInCount,
  selectWsOutCount,
  useStore,
} from "../../store";
import type { IngressMessage } from "../../types";
import "./topbar.css";

/**
 * Top bar.
 *
 * Wired today: brand + version chip, WS status pill, WS uptime, ↓/↑
 * message counters, Reconnect (calls `ws.reconnect()`), Shutdown
 * (sends `{type:"control", command:"shutdown"}`).
 *
 * Deliberately omitted: server-authoritative session ID. The gateway
 * mints `conversation_id` at startup but never sends it; rather than
 * fabricate a local proxy id, we leave the slot empty and show WS
 * uptime in its place. Bind a real chip when `session_init` egress
 * lands (see console/README.md → Future work).
 */
export function TopBar({
  onSend,
  onReconnect,
}: {
  onSend: (msg: IngressMessage) => void;
  onReconnect: () => void;
}) {
  const wsStatus = useStore((s) => s.wsStatus);
  const wsOpenedAt = useStore((s) => s.wsOpenedAt);
  const events = useStore((s) => s.events);

  const wsInCount = useMemo(() => selectWsInCount(events), [events]);
  const wsOutCount = useMemo(() => selectWsOutCount(events), [events]);

  return (
    <div className="topbar">
      <div className="brand">
        <span className="brand-moon" />
        Kaguya · dev console
      </div>
      <span className="version-chip">v{__APP_VERSION__}</span>
      <span className="topbar-divider" />
      <Uptime openedAt={wsOpenedAt} />
      <span className={"ws-pill " + wsStatus}>
        <span className="ws-pill-dot" />
        ws {wsStatus}
      </span>
      <span className="msg-counters">
        <span>↓ <b>{wsInCount}</b></span>
        <span>↑ <b>{wsOutCount}</b></span>
      </span>
      <span className="top-spacer" />
      <button
        type="button"
        className="top-btn"
        onClick={onReconnect}
        title="Reset attempt counter and reconnect immediately"
        disabled={wsStatus === "connected" || wsStatus === "connecting"}
      >
        Reconnect
      </button>
      <button
        type="button"
        className="top-btn danger"
        onClick={() => onSend({ type: "control", command: "shutdown" })}
        title="Send shutdown control message"
      >
        Shutdown
      </button>
    </div>
  );
}

function Uptime({ openedAt }: { openedAt: number | null }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);
  if (openedAt === null) return null;
  return (
    <span className="uptime-chip">
      <span className="uptime-chip-label">ws uptime</span>
      {formatDuration(now - openedAt)}
    </span>
  );
}

function formatDuration(ms: number): string {
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${r}s`;
  return `${r}s`;
}
