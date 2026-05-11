import type { LogEntry } from "../../types";

export type ParsedLevel = "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR";

export function LogRow({ entry }: { entry: LogEntry }) {
  const level = parseLevel(entry.line);
  const cls =
    "log-row " +
    entry.stream +
    (level === "ERROR" ? " error" : level === "WARN" ? " warn" : "");
  return (
    <div className={cls}>
      <span className="log-ts">{formatTs(entry.timestamp)}</span>
      <span className={"log-src " + sourceClass(entry.source)}>{entry.source}</span>
      <span className="log-lvl">{level ?? ""}</span>
      <span className="log-line">{entry.line}</span>
    </div>
  );
}

function sourceClass(src: string): string {
  // Designed-for sources have their own pill colors; unknown sources
  // fall through to the neutral default.
  if (src === "gateway" || src === "talker" || src === "llm_server") return src;
  return "";
}

function formatTs(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const hh = String(d.getHours()).padStart(2, "0");
  const mm = String(d.getMinutes()).padStart(2, "0");
  const ss = String(d.getSeconds()).padStart(2, "0");
  const ms = String(d.getMilliseconds()).padStart(3, "0");
  return `${hh}:${mm}:${ss}.${ms}`;
}

// ─── Level parsing (best-effort) ─────────────────────────────────────
//
// Many talker/gateway log lines embed a level token (INFO, WARN, ERROR,
// DEBUG, TRACE) but without a stable format we can't rely on it. Returns
// null when nothing matched; the filter treats null as "unknown — pass
// through" rather than fabricate a level.

const LEVEL_RE = /\b(TRACE|DEBUG|INFO|WARN|WARNING|ERROR)\b/;

export function parseLevel(line: string): ParsedLevel | null {
  const m = LEVEL_RE.exec(line);
  if (!m) return null;
  const v = m[1].toUpperCase();
  return v === "WARNING" ? "WARN" : (v as ParsedLevel);
}

const RANK = { TRACE: 0, DEBUG: 1, INFO: 2, WARN: 3, ERROR: 4 };

export function passesLevel(
  line: string,
  filter: "ALL" | "INFO+" | "WARN+" | "ERROR",
): boolean {
  if (filter === "ALL") return true;
  const level = parseLevel(line);
  if (level === null) return true;
  const min = filter === "INFO+" ? RANK.INFO : filter === "WARN+" ? RANK.WARN : RANK.ERROR;
  return RANK[level] >= min;
}
