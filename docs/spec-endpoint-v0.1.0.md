# Project Kaguya — Endpoint and Developer Console Specification

**Component:** Gateway development endpoint and React/Vite Console
**Version:** 0.1.0
**Updated:** 2026-10-28

This document describes the implemented endpoint and Console contract. The
[implementation plan](implementation-plan-v0.1.0.md) owns build order and acceptance
gates. Deferred work appears in the final section; it is not part of the current
wire contract.

## 1. Ownership and Runtime

The browser connects to Gateway for conversation traffic and to the Rust
Supervisor through Vite's development proxy for process controls and logs.

| Surface | Owner | Current behavior |
| --- | --- | --- |
| `/ws` | Gateway | Text, control, raw audio ingress, and semantic metadata egress |
| `/health` | Gateway | Liveness response |
| `/runtime/status` | Gateway | Instance/session identity and capability lifecycle snapshot consumed by Supervisor |
| `/api/app/*`, `/api/process/*` | Rust Supervisor, through Console proxy | Application and process lifecycle/status |
| `/api/logs/stream` | Rust Supervisor, through Console proxy | Captured process logs over SSE |
| `/api/standalone/logs/stream` | Console server plugin | Logs from direct debugging processes |

Gateway serves its development endpoint with the `dev-console` Cargo feature.
The Console does not implement a separate debug WebSocket. Gateway remains the
owner of session state, routing and tool semantics; Supervisor owns managed app
processes and sandbox resources. Console's server plugin additionally supports
explicit direct debugging launches and prevents them from running alongside its
managed app controls.

The implemented files are [Gateway endpoint](../gateway/src/services/endpoint.rs),
[Console server plugin](../console/server/plugin.ts),
[launch selection](../console/server/runtime-selection.mjs), and
[Supervisor server](../supervisor/src/server.rs).

## 2. Deployment Selection

Initialize and select an instance using the [instance lifecycle
instructions](r1a-identity.md), then run:

```sh
cd console
npm ci
npm run dev -- --runtime-config ../config/kaguya.local.toml --profile app
```

CLI runtime root/profile selection overrides environment selection, followed by
root discovery and the file's selected profile. Invalid explicit selections fail.
Console obtains the validated instance/session pair through Gateway's read-only
`show-config` command before starting children. That pair and the selected absolute
runtime root/profile override child environment values.

`config/kaguya.runtime.toml` describes process commands, cwd, environment, bind
addresses and endpoints. There is no Console-specific process configuration file.
Current `command` and `command_win32` strings are trusted shell input. The Console
parser supports a documented single-line TOML subset and rejects unsupported
selection syntax. Shared reader/launch descriptor work remains R1d.

The current Vite proxy targets Gateway at `127.0.0.1:8080` and Supervisor at
`127.0.0.1:3001`; deriving these addresses from the deployment is still R1d work.
The browser uses its page host for `/ws`. The dev server runs on port 3000 by
default. These are development surfaces, not an authenticated remote deployment.

## 3. WebSocket Connection

Gateway accepts one active browser client. A new `/ws` connection replaces the
previous connection, which closes with code `4001`. The displaced browser stops
automatic reconnection so two tabs do not continually take over from each other.

For other disconnects, `console/src/ws.ts` retries using the configured delay
sequence and jitter, then leaves an explicit Reconnect control available. A
connection begins with `session_init`. It does not replay transcript history or
select a different session.

## 4. WebSocket Protocol

### 4.1 Ingress: Browser to Gateway

| Frame | Shape | Routing |
| --- | --- | --- |
| Binary | Signed PCM16, 16 kHz mono | Forwarded unchanged to Listener's raw audio connection |
| JSON text | `{"type":"text","content":"..."}` | P1 `TextCommand` |
| JSON text | `{"type":"control","command":"stop"}` | Separate P0 control channel |
| JSON text | `{"type":"control","command":"shutdown"}` | Separate P0 control channel |

The Gateway neither decodes nor interprets audio. Listener's transport uses a
scoped connection header and length-prefixed raw frames; audio does not enter
protobuf serialization. The selected deployment fixes the instance/session pair,
so browser messages cannot select an arbitrary session.

### 4.2 Egress: Gateway to Browser

JSON metadata has an `event_type` discriminator and a nested `data` object.

| Event | `data` |
| --- | --- |
| `session_init` | `{ "instance_id": "...", "session_id": "..." }` |
| `response_started` | `{ "generation_id": "..." }` |
| `sentence` | `{ "generation_id": "...", "text": "..." }` |
| `emotion` | `{ "generation_id": "...", "emotion": "joy" }` |
| `response_complete` | `{ "generation_id": "...", "interrupted": false }` |
| `user_input` | `{ "text": "..." }` |

`user_input` echoes voice transcripts. Typed prompts are recorded locally and are
not echoed by Gateway. `session_init` identifies the configured deployment; its
`session_id` is the manifest's selected `default_session_id`.

Each Talker generation starts with a protobuf `GenerationStart` containing a
`SessionRef`. Gateway validates every `TalkerOutput.generation_id` against that
dispatch and projects the envelope ID into WebSocket metadata. Lifecycle payloads
do not duplicate it. The Console attaches sentence/emotion/completion events to
the matching assistant generation, including delayed output. Unknown generation
references do not mutate another generation.

There is no binary audio egress or browser playback contract. Current TTS plays
on the voice-service host. A Talker-to-browser speech transport remains R7 work.

## 5. Console State and Presentation

The Console uses React with a small external store in `console/src/store.ts`.
Semantic state derives from a capped WebSocket event log. Microphone audio
bypasses that log. Session identity is stored separately from capped event history.

Rows are labelled "turns" in the UI. Assistant rows represent individual Talker
generations, while user rows represent voice transcripts or typed input. These
rows do not define a durable conversational-turn model.

The current regions are TopBar, LeftRail, TurnsPanel, Inspector and LogsPanel.
They display connection state, generation transcripts, microphone activity,
process controls, and captured stdout/stderr. Tool and Reasoner views are deferred. Log-level filtering parses text and is best effort; log
entries do not currently have an authoritative structured level field.

Buffers, retry delays and jitter live in `console/src/config.ts`. UI state and
logs are ephemeral. Logs can be paused, cleared and saved by the browser. There
is no transcript recovery or server-side log persistence contract.

## 6. Platform Boundaries

Gateway and voice gRPC use TCP. Raw Listener audio uses a separate TCP connection.
Browser audio uses AudioWorklet and requires microphone permission; device/sample
rate behavior still needs supported-browser and supported-host validation.

Runtime paths use platform path helpers. Windows supports `command_win32`, and
Console termination uses `taskkill /T /F`; Unix launches use Bash and SIGTERM.
These existing behaviors do not establish complete process-tree or graceful
shutdown acceptance on every host. Supervisor's native process handling and
sandbox backend contracts are documented separately in
[sandbox deployment testing](sandbox-backend-deployment-testing.md).

## 7. Validation and Remaining Work

Run `npm test` and `npm run build` from `console/`. The tests cover deployment
selection, launch environment and generation projection. They do not replace a
live voice or browser integration test. See
[telemetry and end-to-end testing](telemetry-e2e-testing.md) for current probes and
limits.

The implementation plan tracks the remaining work:

- **R1d:** Unified runtime readers, endpoint/proxy derivation and structured launch
  descriptors.
- **R1c/R7:** Durable transcript recovery and replay, with authoritative session
  history supplied by Gateway.
- **R2/R7:** Correlated approval controls and policy state.
- **R3–R7:** Task, workspace, execution and Reasoner events/views.
- **R7:** Talker TTS transport to the browser and measured audio behavior.
- **R7/R8:** Optional debug instrumentation, device selection and
  broader browser/platform validation when their requirements are defined.

Any future debug channel must preserve P0 responsiveness, keep raw audio outside
protobuf and emit complete semantic units rather than tokens. New wire events
must be specified with their producers, consumers and acceptance tests together.
