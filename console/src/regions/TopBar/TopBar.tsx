import { useEffect, useMemo, useState } from "react";
import {
  selectWsInCount,
  selectWsOutCount,
  useStore,
} from "../../store";
import "./topbar.css";

/**
 * Top bar.
 *
 * Wired today: brand + version chip, WS status pill, WS uptime,
 * message counters, Reconnect (calls `ws.reconnect()`), Start App /
 * Shutdown (call Supervisor app lifecycle endpoints).
 *
 * Session identity comes from the Gateway's `session_init` bootstrap event.
 */
export function TopBar({
  onReconnect,
}: {
  onReconnect: () => void;
}) {
  const wsStatus = useStore((s) => s.wsStatus);
  const wsOpenedAt = useStore((s) => s.wsOpenedAt);
  const events = useStore((s) => s.events);

  const wsInCount = useMemo(() => selectWsInCount(events), [events]);
  const wsOutCount = useMemo(() => selectWsOutCount(events), [events]);
  const session = useStore((s) => s.sessionIdentity);

  return (
    <div className="topbar">
      <div className="brand">
        <span className="brand-moon" />
        Kaguya · dev console
      </div>
      <span className="version-chip">v{__APP_VERSION__}</span>
      <span className="topbar-divider" />
      <Uptime openedAt={wsOpenedAt} />
      {session && (
        <span className="session-chip" title={`Instance ${session.instance_id}\nSession ${session.session_id}`}>
          session {session.session_id.slice(0, 8)}
        </span>
      )}
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
        className="top-btn"
        onClick={() => void fetch("/api/app/start", { method: "POST" })}
        title="Start the supervised app runtime"
      >
        Start App
      </button>
      <button
        type="button"
        className="top-btn danger"
        onClick={() => void fetch("/api/app/shutdown", { method: "POST" })}
        title="Shutdown the supervised app runtime"
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
