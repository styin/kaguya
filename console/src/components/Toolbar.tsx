import type { WsStatus } from "../ws";

export type ProcessStatus = "stopped" | "starting" | "running" | "errored";

export interface ProcessInfo {
  name: string;
  managed: boolean;
  status: ProcessStatus;
  pid?: number;
  uptimeSecs?: number;
  exitCode?: number | null;
}

const PROC_COLORS: Record<ProcessStatus, string> = {
  running: "#4ade80",
  starting: "#facc15",
  stopped: "#6b7280",
  errored: "#f87171",
};

const WS_COLORS: Record<WsStatus, string> = {
  connected: "#4ade80",
  connecting: "#facc15",
  disconnected: "#f87171",
};

export function Toolbar({
  wsStatus,
  onReconnect,
  processes,
  onProcessAction,
}: {
  wsStatus: WsStatus;
  onReconnect: () => void;
  processes: ProcessInfo[];
  onProcessAction: (name: string, action: "start" | "stop" | "restart") => void;
}) {
  return (
    <div className="toolbar">
      <span className="toolbar-title">Kaguya Dev Console</span>
      <div className="toolbar-processes">
        {processes.map((proc) => (
          <ProcessControl
            key={proc.name}
            proc={proc}
            onAction={(action) => onProcessAction(proc.name, action)}
          />
        ))}
        <span className="toolbar-status">
          WS:{" "}
          <span
            className="status-dot"
            style={{ backgroundColor: WS_COLORS[wsStatus] }}
            title={`WebSocket: ${wsStatus}`}
          />
          {wsStatus === "disconnected" && (
            <button className="toolbar-btn" onClick={onReconnect}>
              Reconnect
            </button>
          )}
        </span>
      </div>
    </div>
  );
}

function ProcessControl({
  proc,
  onAction,
}: {
  proc: ProcessInfo;
  onAction: (action: "start" | "stop" | "restart") => void;
}) {
  const isRunning = proc.status === "running" || proc.status === "starting";

  return (
    <span className="toolbar-status">
      {proc.name}:{" "}
      <span
        className="status-dot"
        style={{ backgroundColor: PROC_COLORS[proc.status] }}
        title={`${proc.status}${proc.pid ? ` (PID ${proc.pid})` : ""}${proc.uptimeSecs != null ? ` ${proc.uptimeSecs}s` : ""}`}
      />
      {proc.managed && (
        <>
          {!isRunning && (
            <button className="toolbar-btn" onClick={() => onAction("start")}>
              Start
            </button>
          )}
          {isRunning && (
            <button className="toolbar-btn" onClick={() => onAction("stop")}>
              Stop
            </button>
          )}
          {isRunning && (
            <button className="toolbar-btn" onClick={() => onAction("restart")}>
              Restart
            </button>
          )}
        </>
      )}
    </span>
  );
}
