import { useRef, useEffect, useState } from "react";

export interface LogEntry {
  id: number;
  timestamp: string;
  source: string;
  stream: "stdout" | "stderr";
  line: string;
}

type LogLevel = "ALL" | "INFO" | "WARN" | "ERROR";

function matchesLevel(line: string, level: LogLevel): boolean {
  if (level === "ALL") return true;
  if (level === "ERROR") return /\b(ERROR|error)\b/.test(line);
  if (level === "WARN")
    return /\b(WARN|warn|WARNING|ERROR|error)\b/.test(line);
  return true; // INFO shows everything
}

export function LogPanel({ logs }: { logs: LogEntry[] }) {
  const [level, setLevel] = useState<LogLevel>("ALL");
  const bottomRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const [autoScroll, setAutoScroll] = useState(true);

  useEffect(() => {
    if (autoScroll) {
      bottomRef.current?.scrollIntoView({ behavior: "auto" });
    }
  }, [logs, autoScroll]);

  function handleScroll() {
    const el = containerRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    setAutoScroll(atBottom);
  }

  function handleSave() {
    const text = logs.map((e) => `${e.timestamp} [${e.source}] ${e.line}`).join("\n");
    const blob = new Blob([text], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `kaguya-logs-${new Date().toISOString().slice(0, 19)}.log`;
    a.click();
    URL.revokeObjectURL(url);
  }

  const filtered = logs.filter((e) => matchesLevel(e.line, level));

  return (
    <div className="log-panel">
      <div className="log-toolbar">
        <select
          value={level}
          onChange={(e) => setLevel(e.target.value as LogLevel)}
          className="log-level-select"
        >
          <option value="ALL">ALL</option>
          <option value="INFO">INFO+</option>
          <option value="WARN">WARN+</option>
          <option value="ERROR">ERROR</option>
        </select>
        <span className="log-count">{filtered.length} entries</span>
        <button className="toolbar-btn" onClick={handleSave}>
          Save
        </button>
      </div>
      <div className="log-entries" ref={containerRef} onScroll={handleScroll}>
        {filtered.map((entry) => (
          <div
            key={entry.id}
            className={`log-entry ${entry.stream === "stderr" ? "log-stderr" : ""}`}
          >
            <span className="log-ts">
              {entry.timestamp.slice(11, 23)}
            </span>
            <span className="log-source">{entry.source}</span>
            <span className="log-line">{entry.line}</span>
          </div>
        ))}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}
