import type { ProcessInfo } from "../../types";

export function ProcessCard({
  info,
  onAction,
}: {
  info: ProcessInfo;
  onAction: (name: string, action: "start" | "stop" | "restart") => void;
}) {
  const status = info.status;
  const dotClass =
    "process-dot " +
    (status === "running"
      ? "running"
      : status === "errored"
        ? "exited"
        : status === "starting"
          ? "starting"
          : "");

  // Status semantics: running → Stop+Restart; otherwise → Start only.
  const isRunning = status === "running" || status === "starting";
  const blockedBy = info.blockedBy ?? [];
  const isBlocked = blockedBy.length > 0;
  const displayName = info.label || info.name;

  return (
    <div className="process-card">
      <span className={dotClass} />
      <div className="process-name">
        <span className="process-name-row">
          {displayName}
          {info.group && <span className="process-group">{info.group}</span>}
          {!info.managed && <span className="process-unmanaged">unmanaged</span>}
        </span>
        <span className="process-status-row">
          {isBlocked && !isRunning ? `blocked by ${blockedBy.join(", ")}` : status}
          {info.pid !== undefined && ` · pid ${info.pid}`}
          {info.uptimeSecs !== undefined && ` · ${info.uptimeSecs}s`}
        </span>
      </div>
      {/*
        Unmanaged processes deliberately render no buttons — the
        supervisor doesn't control their lifecycle, so any action
        would be a lie.
      */}
      {info.managed && (
        <div className="process-actions">
          {isRunning ? (
            <button
              type="button"
              className="process-action"
              onClick={() => onAction(info.name, "stop")}
            >
              Stop
            </button>
          ) : (
            <button
              type="button"
              className="process-action"
              onClick={() => onAction(info.name, "start")}
              disabled={isBlocked}
              title={
                isBlocked
                  ? `Stop ${blockedBy.join(", ")} before starting ${displayName}`
                  : `Start ${displayName}`
              }
            >
              Start
            </button>
          )}
          <button
            type="button"
            className="process-action"
            onClick={() => onAction(info.name, "restart")}
            disabled={!isRunning}
            title={isRunning ? "Stop + start" : "Start the process first"}
          >
            Restart
          </button>
        </div>
      )}
      {info.children && info.children.length > 0 && (
        <div className="process-children">
          {info.children.map((child) => (
            <div
              key={`${child.kind}:${child.name}`}
              className="process-child-row"
            >
              <span className={`process-child-dot ${child.status}`} />
              <span className="process-child-name">{child.label}</span>
              <span className="process-child-kind">{child.kind}</span>
              <span className="process-child-status">
                {child.readiness ?? child.status}
                {child.pid !== undefined && child.pid !== null && ` · pid ${child.pid}`}
                {child.exitCode !== undefined &&
                  child.exitCode !== null &&
                  ` · exit ${child.exitCode}`}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
