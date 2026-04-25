import type { Plugin } from "vite";
import { Supervisor } from "./supervisor.js";

export function supervisorPlugin(): Plugin {
  let supervisor: Supervisor;

  return {
    name: "kaguya-supervisor",
    configureServer(server) {
      supervisor = new Supervisor(
        new URL("../supervisor.json", import.meta.url).pathname
      );
      supervisor.init().catch((err) => {
        console.error("[supervisor] init failed:", err);
      });

      // Clean up child processes on server close
      server.httpServer?.on("close", () => supervisor.shutdown());
      process.on("SIGINT", () => {
        supervisor.shutdown();
        process.exit(0);
      });
      process.on("SIGTERM", () => {
        supervisor.shutdown();
        process.exit(0);
      });

      // ── HTTP API ──

      server.middlewares.use((req, res, next) => {
        if (!req.url?.startsWith("/api/")) return next();

        // POST /api/process/:name/start|stop|restart
        const actionMatch = req.url.match(
          /^\/api\/process\/(\w+)\/(start|stop|restart)$/
        );
        if (actionMatch && req.method === "POST") {
          const [, name, action] = actionMatch;
          const result =
            action === "start"
              ? supervisor.start(name)
              : action === "stop"
                ? supervisor.stop(name)
                : supervisor.restart(name);
          res.writeHead(result.ok ? 200 : 400, {
            "Content-Type": "application/json",
          });
          res.end(JSON.stringify(result));
          return;
        }

        // GET /api/process/status
        if (req.url === "/api/process/status" && req.method === "GET") {
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(JSON.stringify(supervisor.status()));
          return;
        }

        // GET /api/logs/stream — SSE for real-time log push (must match before /api/logs)
        if (req.url === "/api/logs/stream" && req.method === "GET") {
          res.writeHead(200, {
            "Content-Type": "text/event-stream",
            "Cache-Control": "no-cache",
            Connection: "keep-alive",
          });

          // Send recent backlog as initial batch
          const backlog = supervisor.getLogsSince(0);
          for (const entry of backlog) {
            res.write(`data: ${JSON.stringify(entry)}\n\n`);
          }

          const unsubscribe = supervisor.subscribeLogs((entry) => {
            res.write(`data: ${JSON.stringify(entry)}\n\n`);
          });

          req.on("close", unsubscribe);
          return;
        }

        // GET /api/logs?since=N (polling fallback)
        if (req.url?.startsWith("/api/logs") && req.method === "GET") {
          const url = new URL(req.url, "http://localhost");
          const since = parseInt(url.searchParams.get("since") ?? "0", 10);
          res.writeHead(200, { "Content-Type": "application/json" });
          res.end(JSON.stringify(supervisor.getLogsSince(since)));
          return;
        }

        next();
      });
    },
  };
}
