import { useEffect, useMemo, useRef, useState } from "react";
import { config } from "../../config";
import { actions, useStore } from "../../store";
import type { IngressMessage, LogEntry } from "../../types";
import { LogRow, passesLevel } from "./LogRow";
import { PromptBar } from "./PromptBar";
import "./logspanel.css";

const SCROLL_ANCHOR_PX = 24;
const SOURCE_OPTS = ["all", "gateway", "talker", "llm_server"] as const;
const LEVEL_OPTS = ["ALL", "INFO+", "WARN+", "ERROR"] as const;
const STATS_WINDOW_MS = 60_000;

export function LogsPanel({ onSend }: { onSend: (msg: IngressMessage) => void }) {
  const collapsed = useStore((s) => s.logsCollapsed);
  const logSrc = useStore((s) => s.logSrc);
  const logLvl = useStore((s) => s.logLvl);

  const { logs, pause, paused, clear } = useLogStream();
  const [filterText, setFilterText] = useState("");

  const { regex, regexValid } = useMemo(() => {
    if (!filterText) return { regex: null, regexValid: true };
    try {
      return { regex: new RegExp(filterText, "i"), regexValid: true };
    } catch {
      return { regex: null, regexValid: false };
    }
  }, [filterText]);

  const filtered = useMemo(() => {
    return logs.filter((e) => {
      if (logSrc !== "all" && e.source !== logSrc) return false;
      if (!passesLevel(e.line, logLvl)) return false;
      if (regex && !regex.test(e.line)) return false;
      return true;
    });
  }, [logs, logSrc, logLvl, regex]);

  const rate = useMemo(() => {
    // Entries received in the last minute. Pure computation over the
    // observed log timeline — no separate counter.
    const cutoff = Date.now() - STATS_WINDOW_MS;
    let n = 0;
    for (let i = logs.length - 1; i >= 0; i--) {
      const t = Date.parse(logs[i].timestamp);
      if (Number.isNaN(t) || t < cutoff) break;
      n++;
    }
    return n;
  }, [logs]);

  // Top-to-bottom autoscroll; pauses when scrolled away from bottom.
  const listRef = useRef<HTMLDivElement | null>(null);
  const stickToBottom = useRef(true);
  useEffect(() => {
    if (!stickToBottom.current) return;
    const el = listRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [filtered]);

  function onScroll(e: React.UIEvent<HTMLDivElement>) {
    const el = e.currentTarget;
    stickToBottom.current =
      el.scrollHeight - (el.scrollTop + el.clientHeight) < SCROLL_ANCHOR_PX;
  }

  // Ctrl+` toggles collapse.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.ctrlKey && e.key === "`") {
        e.preventDefault();
        actions.setLogsCollapsed(!collapsed);
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [collapsed]);

  function saveLog() {
    const lines = filtered
      .map((e) => `${e.timestamp}\t${e.source}\t${e.stream}\t${e.line}`)
      .join("\n");
    const blob = new Blob([lines + "\n"], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `kaguya-${new Date().toISOString().replace(/[:.]/g, "-")}.log`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }

  return (
    <div className="logspanel">
      <PromptBar onSend={onSend} />
      <div className="logs-toolbar">
        <span className="logs-toolbar-label">Logs</span>
        <div className="seg">
          {SOURCE_OPTS.map((src) => (
            <button
              key={src}
              type="button"
              className={logSrc === src ? "on" : ""}
              onClick={() => actions.setLogSrc(src)}
            >
              {src}
            </button>
          ))}
        </div>
        <div className="seg">
          {LEVEL_OPTS.map((lvl) => (
            <button
              key={lvl}
              type="button"
              className={logLvl === lvl ? "on" : ""}
              onClick={() => actions.setLogLvl(lvl)}
            >
              {lvl}
            </button>
          ))}
        </div>
        <input
          className={"logs-filter" + (regexValid ? "" : " invalid")}
          value={filterText}
          onChange={(e) => setFilterText(e.target.value)}
          placeholder="filter… (regex)"
          title={regexValid ? "Case-insensitive regex" : "Invalid regex"}
        />
        <span className="logs-spacer" />
        <span className="logs-stats">
          <b>{filtered.length}</b> / {logs.length} entries · {rate}/min
        </span>
        <button
          type="button"
          className={"logs-action" + (paused ? " on" : "")}
          onClick={pause}
          title={paused ? "Resume log intake" : "Pause log intake"}
        >
          {paused ? "Resume" : "Pause"}
        </button>
        <button type="button" className="logs-action" onClick={clear}>
          Clear
        </button>
        <button
          type="button"
          className="logs-action"
          onClick={saveLog}
          disabled={filtered.length === 0}
        >
          Save .log
        </button>
        <button
          type="button"
          className="logs-toggle"
          onClick={() => actions.setLogsCollapsed(!collapsed)}
          title={collapsed ? "Expand logs" : "Collapse logs"}
        >
          <span className="caret">▾</span>
          <kbd>{"ctrl + `"}</kbd>
        </button>
      </div>
      <div ref={listRef} className="logs-rows" onScroll={onScroll}>
        {filtered.map((e) => <LogRow key={e.id} entry={e} />)}
      </div>
    </div>
  );
}

function useLogStream(): {
  logs: LogEntry[];
  paused: boolean;
  pause: () => void;
  clear: () => void;
} {
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [paused, setPaused] = useState(false);
  const pausedRef = useRef(false);

  useEffect(() => {
    pausedRef.current = paused;
  }, [paused]);

  useEffect(() => {
    const es = new EventSource("/api/logs/stream");
    es.onmessage = (ev) => {
      if (pausedRef.current) return;
      try {
        const entry: LogEntry = JSON.parse(ev.data);
        setLogs((prev) => {
          const next = [...prev, entry];
          return next.length > config.logBufferCap
            ? next.slice(next.length - config.logBufferCap)
            : next;
        });
      } catch {
        // malformed SSE payload — ignore
      }
    };
    return () => es.close();
  }, []);

  return {
    logs,
    paused,
    pause: () => setPaused((p) => !p),
    clear: () => setLogs([]),
  };
}
