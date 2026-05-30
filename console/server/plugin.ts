import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { Plugin } from "vite";

const SUPERVISOR_ORIGIN = "http://127.0.0.1:3001";
const APP_RUNTIME_PROFILE = "app";
const DEV_STANDALONE_RUNTIME_PROFILE = "dev_standalone";
const MAX_LOG_ENTRIES = 10_000;
const LOCAL_LOG_ID_START = 1_000_000_000;
// eslint-disable-next-line no-control-regex
const ANSI_RE = /\x1b\[[0-?]*[ -/]*[@-~]/g;
const SPINNER_RE = /^[\u2800-\u28ff]\s/;
const VOICE_STATUS_SPINNER_RE = /^(?:[\\|/-]\s*)?(?:recording|speak now)\s*$/i;

type ProcessStatus = "stopped" | "starting" | "running" | "errored";
type ProcessAction = "start" | "stop" | "restart";

interface ProcessInfo {
  name: string;
  label: string;
  managed: boolean;
  status: ProcessStatus;
  group?: string;
  blockedBy?: string[];
  pid?: number;
  uptimeSecs?: number;
  exitCode?: number | null;
  children?: RuntimeChildInfo[];
}

interface RuntimeChildInfo {
  name: string;
  label: string;
  kind: "process" | "connection";
  status: ProcessStatus;
  pid?: number | null;
  exitCode?: number | null;
}

interface AppStatusSnapshot {
  state: "stopped" | "running" | "degraded" | "stopping";
  processes: ProcessInfo[];
  gateway?: unknown;
}

interface ActionResult {
  ok: boolean;
  error?: string | null;
}

interface LogEntry {
  id: number;
  timestamp: string;
  source: string;
  stream: "stdout" | "stderr";
  line: string;
}

interface StandaloneProcessConfig {
  name: string;
  label: string;
  command: string;
  command_win32?: string;
  cwd: string;
  env?: Record<string, string>;
}

interface StandaloneProcessState {
  config: StandaloneProcessConfig;
  child: ChildProcess | null;
  status: ProcessStatus;
  startedAt: number | null;
  exitCode: number | null;
  stopping: boolean;
}

interface RuntimeProcessConfig {
  label?: string;
  command?: string;
  command_win32?: string;
  cwd?: string;
  env: Record<string, string>;
  bind: Record<string, string>;
  endpoints: Record<string, string>;
}

export function supervisorPlugin(): Plugin {
  let supervisor: ChildProcess | null = null;
  let cleanedUp = false;
  const repoRoot = fileURLToPath(new URL("../..", import.meta.url));
  const standalone = new StandaloneManager(repoRoot);

  return {
    name: "kaguya-supervisor",
    configureServer(server) {
      supervisor = startRustSupervisor();

      const cleanup = () => {
        if (cleanedUp) return;
        cleanedUp = true;
        standalone.shutdown();
        stopRustSupervisor(supervisor);
        supervisor = null;
      };
      const exit = () => {
        cleanup();
        process.exit(0);
      };

      server.httpServer?.once("close", cleanup);
      server.watcher.once("close", cleanup);
      process.once("SIGINT", exit);
      process.once("SIGTERM", exit);
      process.once("SIGHUP", exit);
      process.once("exit", cleanup);

      server.middlewares.use((req, res, next) => {
        if (!req.url?.startsWith("/api/")) return next();
        void handleApi(req, res, standalone);
      });
    },
  };
}

async function handleApi(
  req: http.IncomingMessage,
  res: http.ServerResponse,
  standalone: StandaloneManager,
): Promise<void> {
  const url = new URL(req.url ?? "/", SUPERVISOR_ORIGIN);

  if (url.pathname === "/api/app/status" && req.method === "GET") {
    const app = await fetchSupervisorStatus();
    json(res, composeConsoleStatus(app, standalone));
    return;
  }

  if (url.pathname === "/api/process/status" && req.method === "GET") {
    const app = await fetchSupervisorStatus();
    json(res, composeConsoleStatus(app, standalone).processes);
    return;
  }

  if (url.pathname === "/api/logs/stream" && req.method === "GET") {
    proxyToSupervisor(req, res);
    return;
  }

  if (url.pathname === "/api/standalone/logs/stream" && req.method === "GET") {
    standalone.streamLogs(res);
    return;
  }

  const processAction = /^\/api\/process\/([^/]+)\/(start|stop|restart)$/.exec(
    url.pathname,
  );
  if (processAction && req.method === "POST") {
    const [, name, action] = processAction as [
      string,
      string,
      ProcessAction,
    ];
    if (name === "kaguya_app") {
      await handleAppProcessAction(action, res, standalone);
      return;
    }
    if (standalone.has(name)) {
      const app = await fetchSupervisorStatus();
      json(res, standalone.apply(name, action, isAppActive(app)));
      return;
    }
    proxyToSupervisor(req, res);
    return;
  }

  if (url.pathname === "/api/app/start" && req.method === "POST") {
    const runningStandalone = standalone.runningNames();
    if (runningStandalone.length > 0) {
      json(res, {
        ok: false,
        error: `stop standalone process(es) before starting app mode: ${runningStandalone.join(", ")}`,
      });
      return;
    }
    proxyToSupervisor(req, res);
    return;
  }

  proxyToSupervisor(req, res);
}

function startRustSupervisor(): ChildProcess {
  const supervisorDir = fileURLToPath(
    new URL("../../supervisor/", import.meta.url),
  );
  const configPath = fileURLToPath(
    new URL("../../config/kaguya.runtime.toml", import.meta.url),
  );
  const cargoToml = path.join(supervisorDir, "Cargo.toml");
  const child = spawn("cargo", ["run", "--manifest-path", cargoToml], {
    cwd: supervisorDir,
    env: {
      ...process.env,
      KAGUYA_RUNTIME_CONFIG: configPath,
      KAGUYA_RUNTIME_PROFILE: APP_RUNTIME_PROFILE,
      KAGUYA_SUPERVISOR_AUTOSTART: "false",
      RUST_LOG: process.env.RUST_LOG ?? "kaguya_supervisor=info",
    },
    shell: process.platform === "win32",
    stdio: ["ignore", "inherit", "inherit"],
    windowsHide: true,
  });

  child.on("exit", (code, signal) => {
    console.error(`[supervisor] exited code=${code} signal=${signal}`);
  });
  child.on("error", (err) => {
    console.error(`[supervisor] spawn error: ${err.message}`);
  });

  return child;
}

function stopRustSupervisor(child: ChildProcess | null): void {
  if (
    !child ||
    child.killed ||
    child.exitCode !== null ||
    child.signalCode !== null
  ) {
    return;
  }
  if (process.platform === "win32" && child.pid !== undefined) {
    spawnSync("taskkill", ["/PID", String(child.pid), "/T", "/F"], {
      stdio: "ignore",
      windowsHide: true,
    });
    return;
  }
  child.kill("SIGTERM");
}

function proxyToSupervisor(
  req: http.IncomingMessage,
  res: http.ServerResponse,
): void {
  const target = new URL(req.url ?? "/", SUPERVISOR_ORIGIN);
  const proxyReq = http.request(
    target,
    {
      method: req.method,
      headers: {
        ...req.headers,
        host: target.host,
      },
    },
    (proxyRes) => {
      res.writeHead(proxyRes.statusCode ?? 502, proxyRes.headers);
      proxyRes.pipe(res);
    },
  );

  proxyReq.on("error", (err) => {
    if (res.headersSent) {
      res.end();
      return;
    }
    res.writeHead(503, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ ok: false, error: err.message }));
  });

  req.pipe(proxyReq);
}

async function fetchSupervisorStatus(): Promise<AppStatusSnapshot> {
  try {
    const res = await fetch(`${SUPERVISOR_ORIGIN}/api/app/status`, {
      signal: AbortSignal.timeout(1000),
    });
    if (!res.ok) throw new Error(`supervisor status ${res.status}`);
    return (await res.json()) as AppStatusSnapshot;
  } catch {
    return { state: "stopped", processes: [], gateway: null };
  }
}

function composeConsoleStatus(
  app: AppStatusSnapshot,
  standalone: StandaloneManager,
): AppStatusSnapshot {
  const appActive = isAppActive(app);
  const standaloneProcesses = standalone.status(appActive);
  const runningStandalone = standalone.runningNames();
  const appCard: ProcessInfo = {
    name: "kaguya_app",
    label: "Kaguya App",
    managed: true,
    status: appStatusToProcessStatus(app),
    group: "app",
    blockedBy: runningStandalone,
    children: appActive
      ? app.processes.map((process) => ({
          name: process.name,
          label: process.label,
          kind: "process",
          status: process.status,
          pid: process.pid ?? null,
          exitCode: process.exitCode ?? null,
        }))
      : [],
  };

  return {
    ...app,
    processes: [appCard, ...standaloneProcesses],
  };
}

function isAppActive(app: AppStatusSnapshot): boolean {
  return (
    app.state === "running" ||
    app.state === "degraded" ||
    app.state === "stopping" ||
    app.processes.some(
      (process) =>
        process.managed &&
        ["running", "starting", "errored"].includes(process.status),
    )
  );
}

function appStatusToProcessStatus(app: AppStatusSnapshot): ProcessStatus {
  if (app.state === "running") return "running";
  if (app.state === "degraded") return "errored";
  if (app.state === "stopping") return "starting";
  return "stopped";
}

async function handleAppProcessAction(
  action: ProcessAction,
  res: http.ServerResponse,
  standalone: StandaloneManager,
): Promise<void> {
  if (action === "stop") {
    json(res, await supervisorAction("/api/app/shutdown"));
    return;
  }

  const runningStandalone = standalone.runningNames();
  if (runningStandalone.length > 0) {
    json(res, {
      ok: false,
      error: `stop standalone process(es) before starting app mode: ${runningStandalone.join(", ")}`,
    });
    return;
  }

  if (action === "restart") {
    const stopped = await supervisorAction("/api/app/shutdown");
    if (!stopped.ok) {
      json(res, stopped);
      return;
    }
  }
  json(res, await supervisorAction("/api/app/start"));
}

async function supervisorAction(pathname: string): Promise<ActionResult> {
  try {
    const res = await fetch(`${SUPERVISOR_ORIGIN}${pathname}`, {
      method: "POST",
      signal: AbortSignal.timeout(30_000),
    });
    return (await res.json()) as ActionResult;
  } catch (err) {
    return {
      ok: false,
      error: err instanceof Error ? err.message : String(err),
    };
  }
}

function json(res: http.ServerResponse, payload: unknown): void {
  res.writeHead(200, { "Content-Type": "application/json" });
  res.end(JSON.stringify(payload));
}

class StandaloneManager {
  private readonly processes = new Map<string, StandaloneProcessState>();
  private readonly logs: LogEntry[] = [];
  private readonly logSubscribers = new Set<(entry: LogEntry) => void>();
  private logId = LOCAL_LOG_ID_START;

  constructor(repoRoot: string) {
    const runtimeConfigPath = path.join(repoRoot, "config", "kaguya.runtime.toml");
    const gateway = loadRuntimeProcessConfig(
      runtimeConfigPath,
      DEV_STANDALONE_RUNTIME_PROFILE,
      "gateway",
    );
    const voiceStack = loadRuntimeProcessConfig(
      runtimeConfigPath,
      DEV_STANDALONE_RUNTIME_PROFILE,
      "voice_stack",
    );

    this.add({
      name: "gateway_standalone",
      label: gateway.label ?? "Gateway",
      command: gateway.command ?? "cargo run --features dev-console",
      command_win32: gateway.command_win32,
      cwd: resolveRuntimeCwd(runtimeConfigPath, gateway.cwd, repoRoot, "gateway"),
      env: {
        ...gateway.env,
        KAGUYA_RUNTIME_CONFIG: runtimeConfigPath,
        KAGUYA_RUNTIME_PROFILE: DEV_STANDALONE_RUNTIME_PROFILE,
      },
    });
    this.add({
      name: "voice_stack_standalone",
      label: voiceStack.label ?? "Voice Stack",
      command: voiceStack.command ?? ".venv/bin/python main.py",
      command_win32: voiceStack.command_win32 ?? ".venv\\Scripts\\python.exe main.py",
      cwd: resolveRuntimeCwd(runtimeConfigPath, voiceStack.cwd, repoRoot, "talker"),
      env: {
        ...voiceStack.env,
        ...voiceStackBindEnv(voiceStack.bind),
      },
    });
  }

  has(name: string): boolean {
    return this.processes.has(name);
  }

  apply(name: string, action: ProcessAction, appActive: boolean): ActionResult {
    if (appActive && action === "start") {
      return {
        ok: false,
        error: "stop app mode before starting standalone processes",
      };
    }
    if (action === "start") return this.start(name);
    if (action === "stop") return this.stop(name);
    return this.restart(name, appActive);
  }

  status(appActive: boolean): ProcessInfo[] {
    return [...this.processes.values()].map((process) => {
      const info: ProcessInfo = {
        name: process.config.name,
        label: process.config.label,
        managed: true,
        status: process.status,
        group: "standalone",
        blockedBy:
          appActive && process.status !== "running" ? ["kaguya_app"] : [],
        pid: process.child?.pid,
        exitCode: process.exitCode,
      };
      if (process.startedAt && process.child) {
        info.uptimeSecs = Math.floor((Date.now() - process.startedAt) / 1000);
      }
      return info;
    });
  }

  runningNames(): string[] {
    return [...this.processes.values()]
      .filter((process) => process.child !== null)
      .map((process) => process.config.name);
  }

  streamLogs(res: http.ServerResponse): void {
    res.writeHead(200, {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-cache",
      Connection: "keep-alive",
    });

    for (const entry of this.logs.slice(-200)) {
      res.write(`data: ${JSON.stringify(entry)}\n\n`);
    }

    const subscriber = (entry: LogEntry) => {
      res.write(`data: ${JSON.stringify(entry)}\n\n`);
    };
    this.logSubscribers.add(subscriber);
    res.on("close", () => this.logSubscribers.delete(subscriber));
  }

  shutdown(): void {
    for (const state of this.processes.values()) {
      if (state.child) this.stopChild(state.child, true);
    }
  }

  private add(config: StandaloneProcessConfig): void {
    this.processes.set(config.name, {
      config,
      child: null,
      status: "stopped",
      startedAt: null,
      exitCode: null,
      stopping: false,
    });
  }

  private start(name: string): ActionResult {
    const state = this.processes.get(name);
    if (!state) return { ok: false, error: `unknown process: ${name}` };
    if (state.child) return { ok: false, error: `${name} already running` };

    state.status = "starting";
    state.startedAt = Date.now();
    state.exitCode = null;
    state.stopping = false;

    const child = spawn(this.commandForPlatform(state.config), {
      cwd: state.config.cwd,
      env: { ...process.env, ...state.config.env },
      shell: process.platform === "win32" ? true : "/bin/bash",
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    state.child = child;
    state.status = "running";

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
      state.child = null;
      state.exitCode = code;
      state.status = state.stopping || code === 0 ? "stopped" : "errored";
      state.stopping = false;
      this.pushLog(name, "stderr", `[supervisor] exited code=${code} signal=${signal}`);
    });
    child.on("error", (err) => {
      state.child = null;
      state.status = "errored";
      state.stopping = false;
      this.pushLog(name, "stderr", `[supervisor] spawn error: ${err.message}`);
    });

    return { ok: true };
  }

  private stop(name: string): ActionResult {
    const state = this.processes.get(name);
    if (!state) return { ok: false, error: `unknown process: ${name}` };
    if (!state.child) return { ok: true };
    state.stopping = true;
    this.pushLog(name, "stdout", "[supervisor] stopping process");
    this.stopChild(state.child);
    return { ok: true };
  }

  private restart(name: string, appActive: boolean): ActionResult {
    const state = this.processes.get(name);
    if (!state) return { ok: false, error: `unknown process: ${name}` };
    if (!state.child) return this.apply(name, "start", appActive);
    state.stopping = true;
    const child = state.child;
    child.once("exit", () => setTimeout(() => this.start(name), 200));
    this.stopChild(child);
    return { ok: true };
  }

  private commandForPlatform(config: StandaloneProcessConfig): string {
    if (process.platform === "win32" && config.command_win32) {
      return config.command_win32;
    }
    return config.command;
  }

  private stopChild(child: ChildProcess, sync = false): void {
    if (process.platform === "win32" && child.pid !== undefined) {
      const args = ["/PID", String(child.pid), "/T", "/F"];
      if (sync) {
        spawnSync("taskkill", args, {
          stdio: "ignore",
          windowsHide: true,
        });
        return;
      }
      spawn("taskkill", args, {
        stdio: "ignore",
        windowsHide: true,
      });
      return;
    }
    child.kill("SIGTERM");
  }

  private pushLog(
    source: string,
    stream: "stdout" | "stderr",
    line: string,
  ): void {
    const cleaned = line.replace(ANSI_RE, "");
    if (!cleaned.trim() || isTransientStatusLine(cleaned)) return;

    const entry: LogEntry = {
      id: this.logId++,
      timestamp: new Date().toISOString(),
      source: normalizeLogSource(source),
      stream,
      line: cleaned,
    };
    this.logs.push(entry);
    if (this.logs.length > MAX_LOG_ENTRIES) {
      this.logs.splice(0, this.logs.length - MAX_LOG_ENTRIES);
    }
    for (const subscriber of this.logSubscribers) {
      subscriber(entry);
    }
  }
}

function attachLineSplitter(
  stream: NodeJS.ReadableStream,
  onLine: (line: string) => void,
): void {
  let buffer = "";
  stream.setEncoding("utf-8");
  stream.on("data", (chunk: string | Buffer) => {
    buffer += typeof chunk === "string" ? chunk : chunk.toString("utf-8");
    const parts = buffer.split(/\r\n|\r|\n/);
    buffer = parts.pop() ?? "";
    for (const part of parts) {
      if (part.length > 0) onLine(part);
    }
  });
  stream.on("end", () => {
    if (buffer.length > 0) onLine(buffer);
    buffer = "";
  });
}

function isTransientStatusLine(line: string): boolean {
  const trimmed = line.trim();
  return SPINNER_RE.test(trimmed) || VOICE_STATUS_SPINNER_RE.test(trimmed);
}

function normalizeLogSource(source: string): string {
  if (source === "gateway_standalone") return "gateway";
  if (source === "voice_stack_standalone") return "talker";
  return source;
}

function loadRuntimeProcessConfig(
  configPath: string,
  profile: string,
  processName: string,
): RuntimeProcessConfig {
  const content = fs.readFileSync(configPath, "utf-8");
  return parseRuntimeProcessConfig(content, profile, processName);
}

function parseRuntimeProcessConfig(
  content: string,
  profile: string,
  processName: string,
): RuntimeProcessConfig {
  const result: RuntimeProcessConfig = { env: {}, bind: {}, endpoints: {} };
  const baseSection = `profiles.${profile}.processes.${processName}`;
  let section = "";

  for (const rawLine of content.split(/\r?\n/)) {
    const line = stripTomlComment(rawLine).trim();
    if (!line) continue;

    const sectionMatch = /^\[([^\]]+)\]$/.exec(line);
    if (sectionMatch) {
      section = sectionMatch[1] ?? "";
      continue;
    }

    if (
      section !== baseSection &&
      section !== `${baseSection}.env` &&
      section !== `${baseSection}.bind` &&
      section !== `${baseSection}.endpoints`
    ) {
      continue;
    }

    const keyValue = /^([A-Za-z0-9_]+)\s*=\s*(.+)$/.exec(line);
    if (!keyValue) continue;
    const [, key, rawValue] = keyValue;
    const value = parseTomlString(rawValue);
    if (value === null) continue;

    if (section === `${baseSection}.env`) {
      result.env[key] = value;
    } else if (section === `${baseSection}.bind`) {
      result.bind[key] = value;
    } else if (section === `${baseSection}.endpoints`) {
      result.endpoints[key] = value;
    } else if (key === "label") {
      result.label = value;
    } else if (key === "command") {
      result.command = value;
    } else if (key === "command_win32") {
      result.command_win32 = value;
    } else if (key === "cwd") {
      result.cwd = value;
    }
  }

  validateBindEndpointPorts(result, `${profile}.${processName}`);
  return result;
}

function stripTomlComment(line: string): string {
  let inString = false;
  let escaped = false;
  for (let i = 0; i < line.length; i += 1) {
    const char = line[i];
    if (escaped) {
      escaped = false;
      continue;
    }
    if (char === "\\") {
      escaped = true;
      continue;
    }
    if (char === '"') {
      inString = !inString;
      continue;
    }
    if (char === "#" && !inString) {
      return line.slice(0, i);
    }
  }
  return line;
}

function parseTomlString(rawValue: string | undefined): string | null {
  if (!rawValue) return null;
  const value = rawValue.trim();
  if (!value.startsWith('"') || !value.endsWith('"')) return null;
  return value
    .slice(1, -1)
    .replace(/\\"/g, '"')
    .replace(/\\\\/g, "\\");
}

function resolveRuntimeCwd(
  configPath: string,
  configuredCwd: string | undefined,
  repoRoot: string,
  fallbackDir: string,
): string {
  if (!configuredCwd) return path.join(repoRoot, fallbackDir);
  if (path.isAbsolute(configuredCwd)) return configuredCwd;
  return path.resolve(path.dirname(configPath), configuredCwd);
}

function voiceStackBindEnv(bind: Record<string, string>): Record<string, string> {
  const env: Record<string, string> = {};
  if (bind.talker_grpc) {
    env.KAGUYA_TALKER_LISTEN_ADDR = bind.talker_grpc;
  }
  if (bind.listener_grpc) {
    env.KAGUYA_LISTENER_GRPC_ADDR = bind.listener_grpc;
  }
  if (bind.listener_audio) {
    const { host, port } = splitHostPort(bind.listener_audio);
    env.KAGUYA_LISTENER_AUDIO_ADDR = host;
    env.KAGUYA_LISTENER_AUDIO_PORT = port;
  }
  return env;
}

function splitHostPort(addr: string): { host: string; port: string } {
  const index = addr.lastIndexOf(":");
  if (index < 0) return { host: addr, port: "" };
  return {
    host: addr.slice(0, index).replace(/^\[/, "").replace(/\]$/, ""),
    port: addr.slice(index + 1),
  };
}

function validateBindEndpointPorts(
  config: RuntimeProcessConfig,
  label: string,
): void {
  for (const [name, bindAddr] of Object.entries(config.bind)) {
    const endpointAddr = config.endpoints[name];
    if (!endpointAddr) continue;
    const bindPort = portOf(bindAddr);
    const endpointPort = portOf(endpointAddr);
    if (!bindPort) {
      throw new Error(`${label}.bind.${name} has no explicit port: ${bindAddr}`);
    }
    if (!endpointPort) {
      throw new Error(
        `${label}.endpoints.${name} has no explicit port: ${endpointAddr}`,
      );
    }
    if (bindPort !== endpointPort) {
      throw new Error(
        `${label}.${name} bind/endpoints port mismatch: bind=${bindAddr}, endpoint=${endpointAddr}`,
      );
    }
  }
}

function portOf(addr: string): string | null {
  const withoutScheme = addr.includes("://")
    ? (addr.split("://", 2)[1] ?? addr)
    : addr;
  const authority = withoutScheme.split("/", 1)[0] ?? withoutScheme;
  if (authority.startsWith("[")) {
    const marker = "]:";
    const index = authority.indexOf(marker);
    if (index < 0) return null;
    return authority.slice(index + marker.length) || null;
  }
  const index = authority.lastIndexOf(":");
  if (index < 0) return null;
  return authority.slice(index + 1) || null;
}
