import { useEffect, useState } from "react";
import { actions, useStore } from "../../store";
import type { AppStatusSnapshot, ProcessInfo } from "../../types";
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
        const app = await fetchAppStatus();
        if (alive) setList(app.processes);
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

async function fetchAppStatus(): Promise<AppStatusSnapshot> {
  const res = await fetch("/api/app/status");
  if (!res.ok) throw new Error("app status unavailable");
  return res.json();
}

function processAction(name: string, action: "start" | "stop" | "restart"): void {
  void fetch(`/api/process/${name}/${action}`, { method: "POST" });
}
