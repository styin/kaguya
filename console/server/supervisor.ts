import { spawn, type ChildProcess } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";

// Split a child stream into log entries. Unlike `readline.createInterface`,
// this also breaks on bare `\r` so TTY-style in-place updates (RealtimeSTT's
// `⠋ recording` spinner) surface as discrete frames rather than accumulating
// in readline's buffer or — worse — flooding as one entry per tick under a
// single growing line. Spinner frames are then filtered downstream.
function attachLineSplitter(
  stream: NodeJS.ReadableStream,
  onLine: (line: string) => void,
) {
  let buffer = "";
  stream.setEncoding("utf-8");
  stream.on("data", (chunk: string | Buffer) => {
    buffer += typeof chunk === "string" ? chunk : chunk.toString("utf-8");
    const parts = buffer.split(/\r\n|\r|\n/);
    buffer = parts.pop() ?? "";
    for (const p of parts) {
      if (p.length > 0) onLine(p);
    }
  });
  stream.on("end", () => {
    if (buffer.length > 0) onLine(buffer);
    buffer = "";
  });
}

// ── Types ──

export type ProcessStatus = "stopped" | "starting" | "running" | "errored";

export interface ProcessInfo {
  name: string;
  managed: boolean;
  status: ProcessStatus;
  pid?: number;
  uptimeSecs?: number;
  exitCode?: number | null;
}

export interface LogEntry {
  id: number;
  timestamp: string;
  source: string;
  stream: "stdout" | "stderr";
  line: string;
}

interface ManagedProcessConfig {
  command: string;
  command_win32?: string;
  cwd: string;
  env?: Record<string, string>;
  managed?: true;
}

interface UnmanagedProcessConfig {
  managed: false;
  health_url: string;
  poll_interval_ms: number;
}

type ProcessConfig = ManagedProcessConfig | UnmanagedProcessConfig;

interface SupervisorConfig {
  processes: Record<string, ProcessConfig>;
}

// ── Supervisor ──

const MAX_LOG_ENTRIES = 10_000;
// eslint-disable-next-line no-control-regex
const ANSI_RE = /\x1b\[[0-?]*[ -/]*[@-~]/g;
// Drop TTY spinners (e.g. RealtimeSTT's `⠋ recording`) — Braille block U+2800–U+28FF
// followed by whitespace is a near-unambiguous signature for a progress glyph.
const SPINNER_RE = /^[⠀-⣿]\s/;
const VOICE_STATUS_SPINNER_RE = /^(?:[\\|/-]\s*)?(?:recording|speak now)\s*$/i;

function isTransientStatusLine(line: string): boolean {
  const trimmed = line.trim();
  return SPINNER_RE.test(trimmed) || VOICE_STATUS_SPINNER_RE.test(trimmed);
}

interface ManagedProcess {
  config: ManagedProcessConfig;
  child: ChildProcess | null;
  status: ProcessStatus;
  startedAt: number | null;
  exitCode: number | null;
}

interface UnmanagedProcess {
  config: UnmanagedProcessConfig;
  status: ProcessStatus;
  pollTimer: ReturnType<typeof setInterval> | null;
}

export class Supervisor {
  private managed = new Map<string, ManagedProcess>();
  private unmanaged = new Map<string, UnmanagedProcess>();
  private logs: LogEntry[] = [];
  private logId = 0;
  private baseDir: string;

  constructor(configPath: string) {
    this.baseDir = path.dirname(configPath);
  }

  async init(): Promise<void> {
    const raw = await readFile(
      path.join(this.baseDir, "supervisor.json"),
      "utf-8"
    );
    const config: SupervisorConfig = JSON.parse(raw);

    for (const [name, proc] of Object.entries(config.processes)) {
      if (proc.managed === false) {
        const up: UnmanagedProcess = {
          config: proc,
          status: "stopped",
          pollTimer: null,
        };
        this.unmanaged.set(name, up);
        this.startHealthPoll(name, up);
      } else {
        this.managed.set(name, {
          config: proc as ManagedProcessConfig,
          child: null,
          status: "stopped",
          startedAt: null,
          exitCode: null,
        });
      }
    }
  }

  // ── Process control ──

  start(name: string): { ok: boolean; error?: string } {
    const proc = this.managed.get(name);
    if (!proc) return { ok: false, error: `unknown process: ${name}` };
    if (proc.child) return { ok: false, error: `${name} already running` };

    const cwd = path.resolve(this.baseDir, proc.config.cwd);
    const env = { ...process.env, ...proc.config.env };

    proc.status = "starting";
    proc.exitCode = null;
    proc.startedAt = Date.now();

    const command = this.commandForPlatform(proc.config);
    const child = spawn(command, {
      cwd,
      env,
      shell: process.platform === "win32" ? true : "/bin/bash",
      stdio: ["ignore", "pipe", "pipe"],
    });

    proc.child = child;
    proc.status = "running";

    this.pushLog(name, "stdout", `[supervisor] started PID ${child.pid}`);

    if (child.stdout) {
      attachLineSplitter(child.stdout, (line) =>
        this.pushLog(name, "stdout", line),
      );
    }
    if (child.stderr) {
      attachLineSplitter(child.stderr, (line) =>
        this.pushLog(name, "stderr", line),
      );
    }

    child.on("exit", (code, signal) => {
      proc.child = null;
      proc.exitCode = code;
      proc.status = code === 0 ? "stopped" : "errored";
      this.pushLog(
        name,
        "stderr",
        `[supervisor] exited code=${code} signal=${signal}`
      );
    });

    child.on("error", (err) => {
      proc.child = null;
      proc.status = "errored";
      this.pushLog(name, "stderr", `[supervisor] spawn error: ${err.message}`);
    });

    return { ok: true };
  }

  stop(name: string): { ok: boolean; error?: string } {
    const proc = this.managed.get(name);
    if (!proc) return { ok: false, error: `unknown process: ${name}` };
    if (!proc.child) return { ok: false, error: `${name} not running` };

    this.pushLog(name, "stdout", `[supervisor] stopping PID ${proc.child.pid}`);
    this.stopChild(proc.child);

    // Force kill after 5s if still alive
    const pid = proc.child.pid;
    setTimeout(() => {
      if (proc.child && proc.child.pid === pid) {
        this.forceKillChild(proc.child);
        this.pushLog(name, "stderr", `[supervisor] force-killed PID ${pid}`);
      }
    }, 5000);

    return { ok: true };
  }

  restart(name: string): { ok: boolean; error?: string } {
    const proc = this.managed.get(name);
    if (!proc) return { ok: false, error: `unknown process: ${name}` };

    if (proc.child) {
      this.stopChild(proc.child);
      // Wait for exit, then start
      proc.child.once("exit", () => {
        setTimeout(() => this.start(name), 200);
      });
      // Force kill fallback
      const pid = proc.child.pid;
      setTimeout(() => {
        if (proc.child && proc.child.pid === pid) {
          this.forceKillChild(proc.child);
        }
      }, 5000);
    } else {
      this.start(name);
    }

    return { ok: true };
  }

  // ── Status ──

  status(): ProcessInfo[] {
    const result: ProcessInfo[] = [];

    for (const [name, proc] of this.managed) {
      const info: ProcessInfo = {
        name,
        managed: true,
        status: proc.status,
        pid: proc.child?.pid,
        exitCode: proc.exitCode,
      };
      if (proc.startedAt && proc.child) {
        info.uptimeSecs = Math.floor((Date.now() - proc.startedAt) / 1000);
      }
      result.push(info);
    }

    for (const [name, proc] of this.unmanaged) {
      result.push({ name, managed: false, status: proc.status });
    }

    return result;
  }

  // ── Logs ──

  private logSubscribers = new Set<(entry: LogEntry) => void>();

  getLogsSince(sinceId: number): LogEntry[] {
    if (sinceId <= 0) {
      return this.logs.slice(-200);
    }
    const idx = this.logs.findIndex((e) => e.id > sinceId);
    if (idx === -1) return [];
    return this.logs.slice(idx);
  }

  subscribeLogs(cb: (entry: LogEntry) => void): () => void {
    this.logSubscribers.add(cb);
    return () => this.logSubscribers.delete(cb);
  }

  private pushLog(source: string, stream: "stdout" | "stderr", line: string) {
    const cleaned = line.replace(ANSI_RE, "");
    if (isTransientStatusLine(cleaned)) return;
    this.logId++;
    const entry: LogEntry = {
      id: this.logId,
      timestamp: new Date().toISOString(),
      source,
      stream,
      line: cleaned,
    };
    this.logs.push(entry);
    if (this.logs.length > MAX_LOG_ENTRIES) {
      this.logs = this.logs.slice(-MAX_LOG_ENTRIES);
    }
    for (const cb of this.logSubscribers) {
      cb(entry);
    }
  }

  // ── Health polling (unmanaged) ──

  private startHealthPoll(name: string, proc: UnmanagedProcess) {
    proc.pollTimer = setInterval(async () => {
      try {
        const resp = await fetch(proc.config.health_url, {
          signal: AbortSignal.timeout(3000),
        });
        proc.status = resp.ok ? "running" : "errored";
      } catch {
        proc.status = "stopped";
      }
    }, proc.config.poll_interval_ms);
  }

  // ── Cleanup ──

  private commandForPlatform(config: ManagedProcessConfig): string {
    if (process.platform === "win32" && config.command_win32) {
      return config.command_win32;
    }
    return config.command;
  }

  private stopChild(child: ChildProcess): void {
    if (process.platform === "win32") {
      this.killWindowsProcessTree(child);
      return;
    }
    child.kill("SIGTERM");
  }

  private forceKillChild(child: ChildProcess): void {
    if (process.platform === "win32") {
      this.killWindowsProcessTree(child);
      return;
    }
    child.kill("SIGKILL");
  }

  private killWindowsProcessTree(child: ChildProcess): void {
    if (child.pid === undefined) return;
    const killer = spawn("taskkill", ["/PID", String(child.pid), "/T", "/F"], {
      stdio: "ignore",
      windowsHide: true,
    });
    killer.on("error", () => {
      child.kill();
    });
  }

  shutdown() {
    for (const [, proc] of this.managed) {
      if (proc.child) this.stopChild(proc.child);
    }
    for (const [, proc] of this.unmanaged) {
      if (proc.pollTimer) clearInterval(proc.pollTimer);
    }
  }
}
