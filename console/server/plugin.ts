import { spawn, type ChildProcess } from "node:child_process";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { Plugin } from "vite";

const SUPERVISOR_ORIGIN = "http://127.0.0.1:3001";

export function supervisorPlugin(): Plugin {
  let supervisor: ChildProcess | null = null;

  return {
    name: "kaguya-supervisor",
    configureServer(server) {
      supervisor = startRustSupervisor();

      server.httpServer?.on("close", () => stopRustSupervisor(supervisor));
      process.on("SIGINT", () => {
        stopRustSupervisor(supervisor);
        process.exit(0);
      });
      process.on("SIGTERM", () => {
        stopRustSupervisor(supervisor);
        process.exit(0);
      });

      server.middlewares.use((req, res, next) => {
        if (!req.url?.startsWith("/api/")) return next();
        proxyToSupervisor(req, res);
      });
    },
  };
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
  if (!child || child.killed) return;
  if (process.platform === "win32" && child.pid !== undefined) {
    spawn("taskkill", ["/PID", String(child.pid), "/T", "/F"], {
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
