import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";

const sourcePath = fileURLToPath(new URL("./plugin.ts", import.meta.url));
// Expose internal boundaries in the test bundle without widening the production API.
const compiled = await build({
  stdin: {
    contents: `${await fs.readFile(sourcePath, "utf8")}\nexport { fetchSupervisorStatus, handleApi };`,
    resolveDir: path.dirname(sourcePath),
    loader: "ts",
  },
  bundle: true, write: false, format: "esm", platform: "node",
});
const plugin = await import(`data:text/javascript;base64,${Buffer.from(compiled.outputFiles[0].text).toString("base64")}`);

test("Supervisor unavailability cannot become stopped state or authorize a direct launch", async (t) => {
  t.mock.method(globalThis, "fetch", async () => { throw new Error("unreachable"); });
  await assert.rejects(plugin.fetchSupervisorStatus(), /unreachable/);
  let launched = false;
  const standalone = { has: () => true, apply: () => { launched = true; } };
  await assert.rejects(plugin.handleApi({
    url: "/api/process/gateway_standalone/start", method: "POST",
  }, {}, standalone), /unreachable/);
  assert.equal(launched, false);
});

test("Supervisor HTTP errors and invalid JSON propagate instead of fabricating status", async (t) => {
  const fetch = t.mock.method(globalThis, "fetch", async () => new Response("busy", { status: 503 }));
  await assert.rejects(plugin.fetchSupervisorStatus(), /unavailable \(503\)/);
  fetch.mock.mockImplementation(async () => new Response("not json"));
  await assert.rejects(plugin.fetchSupervisorStatus(), SyntaxError);
  fetch.mock.mockImplementation(async () => Response.json({ state: "unknown", processes: [] }));
  await assert.rejects(plugin.fetchSupervisorStatus(), /invalid app status/);
});
