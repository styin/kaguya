import { useEffect, useState } from "react";
import { actions, useStore } from "../../store";
import type {
  ProcessInfo,
  ProcessStatus,
  RuntimeChildInfo,
  RuntimeReadiness,
  RuntimeStatusSnapshot,
} from "../../types";
import { AudioStrip } from "./AudioStrip";
import { ProcessCard } from "./ProcessCard";
import "./leftrail.css";

const POLL_MS = 1000;

export function LeftRail() {
  const micActive = useStore((s) => s.micActive);
  const processes = useProcesses();

  return (
    <div className="leftrail">
      <div className="leftrail-label">Audio</div>
      <div className="audio-card">
        <AudioStrip kind="mic" name="mic" detail={micActive ? "live" : "idle"} />
        <AudioStrip kind="tts" name="tts" detail="default output" />
      </div>

      <div className="mic-toggle">
        <button
          type="button"
          className={"mic-toggle-btn" + (micActive ? " active" : "")}
          onClick={() => actions.setMicActive(!micActive)}
        >
          {micActive ? (
            <>
              <span className="mic-live-dot" />
              open mic live
            </>
          ) : (
            "turn mic on"
          )}
        </button>
      </div>

      <div className="leftrail-label">
        <span>Processes</span>
        <span className="leftrail-count">{processes.length}</span>
      </div>
      <div className="process-list">
        {processes.map((p) => (
          <ProcessCard key={p.name} info={p} onAction={processAction} />
        ))}
      </div>
    </div>
  );
}

function useProcesses(): ProcessInfo[] {
  const [list, setList] = useState<ProcessInfo[]>([]);

  useEffect(() => {
    let alive = true;

    async function poll() {
      try {
        const [processes, runtime] = await Promise.all([
          fetchProcessStatus(),
          fetchRuntimeStatus(),
        ]);
        if (alive) setList(attachRuntimeChildren(processes, runtime));
      } catch {
        // Supervisor not ready: leave the last good list visible.
      }
    }

    poll();
    const id = setInterval(poll, POLL_MS);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, []);

  return list;
}

async function fetchProcessStatus(): Promise<ProcessInfo[]> {
  const res = await fetch("/api/process/status");
  if (!res.ok) throw new Error("process status unavailable");
  return res.json();
}

async function fetchRuntimeStatus(): Promise<RuntimeStatusSnapshot | null> {
  try {
    const res = await fetch("/runtime/status");
    if (!res.ok) return null;
    return res.json();
  } catch {
    return null;
  }
}

function attachRuntimeChildren(
  processes: ProcessInfo[],
  runtime: RuntimeStatusSnapshot | null,
): ProcessInfo[] {
  const app = processes.find((process) => process.name === "kaguya_app");
  if (!app || app.status === "stopped" || !runtime) {
    return processes;
  }

  const children = runtimeChildren(runtime);
  return processes.map((process) =>
    process.name === "kaguya_app" ? { ...process, children } : process,
  );
}

function runtimeChildren(runtime: RuntimeStatusSnapshot): RuntimeChildInfo[] {
  const connectionChildren = runtime.lifecycle.connections.map((connection) => ({
    name: connection.name,
    label: displayRuntimeName(connection.name),
    kind: "connection" as const,
    status: processStatusFromReadiness(connection.readiness),
    readiness: connection.readiness,
  }));

  return connectionChildren;
}

function processStatusFromReadiness(readiness: RuntimeReadiness): ProcessStatus {
  if (readiness === "ready") return "running";
  if (readiness === "starting") return "starting";
  if (readiness === "degraded") return "errored";
  return "stopped";
}

function displayRuntimeName(name: string): string {
  return name
    .split("_")
    .filter(Boolean)
    .map((part) => part[0]?.toUpperCase() + part.slice(1))
    .join(" ");
}

function processAction(name: string, action: "start" | "stop" | "restart"): void {
  void fetch(`/api/process/${name}/${action}`, { method: "POST" });
}
