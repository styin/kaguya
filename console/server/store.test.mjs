import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";

// Load the production selector with the existing Vite compiler dependency.
// Only the browser location constant is substituted; projection logic is unchanged.
const compiled = await build({
  entryPoints: [fileURLToPath(new URL("../src/store.ts", import.meta.url))],
  bundle: true, write: false, format: "esm", platform: "browser",
  define: { "window.location.host": '"localhost"', "process.env.NODE_ENV": '"production"' },
});
const store = await import(`data:text/javascript;base64,${Buffer.from(compiled.outputFiles[0].text).toString("base64")}`);
const incoming = (ts, event_type, data) => ({ kind: "ws_in", ts, msg: { event_type, data } });

test("queued output stays associated with its originating generation", () => {
  const turns = store.selectTurns([
    incoming(1, "response_started", { generation_id: "g1" }),
    incoming(2, "response_started", { generation_id: "g2" }),
    incoming(3, "sentence", { generation_id: "g1", text: "delayed g1" }),
    incoming(4, "emotion", { generation_id: "g2", emotion: "calm" }),
    incoming(5, "sentence", { generation_id: "unknown", text: "unrelated" }),
    incoming(6, "sentence", { text: "missing provenance" }),
    incoming(7, "emotion", { generation_id: "unknown", emotion: "unrelated" }),
    incoming(8, "response_complete", { generation_id: "g1", interrupted: true }),
    incoming(9, "sentence", { generation_id: "g1", text: "known late output" }),
    incoming(10, "response_complete", { generation_id: "g2", interrupted: false }),
  ]);
  assert.deepEqual(turns.map((turn) => turn.generation_id), ["g1", "g2"]);
  assert.deepEqual(turns[0].sentences.map((sentence) => sentence.text), ["delayed g1", "known late output"]);
  assert.equal(turns[0].interrupted, true);
  assert.equal(turns[0].complete_at, 8);
  assert.equal(turns[0].emotion, undefined);
  assert.deepEqual(turns[1].sentences, []);
  assert.equal(turns[1].emotion, "calm");
  assert.equal(turns[1].complete_at, 10);
});
