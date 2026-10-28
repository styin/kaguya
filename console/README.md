# Kaguya Developer Console

Local React/Vite interface for the Gateway conversation endpoint and Rust
Supervisor controls. It displays transcripts, generation activity, microphone
input, process state and captured logs. Speech currently plays on the Talker
host; the browser has no TTS playback transport.

The [Endpoint specification](../docs/spec-endpoint-v0.1.0.md) defines the current
wire contract. The [implementation plan](../docs/implementation-plan-v0.1.0.md)
owns remaining work and acceptance gates.

## Running and Checking

Initialize and select an instance using the [deployment instructions](../docs/r1a-identity.md),
then run:

```sh
cd console
npm ci
npm run dev -- --runtime-config ../config/kaguya.local.toml --profile app
```

```sh
npm test
npm run build
```

Tests cover runtime selection, launch validation and generation projection.
The build type-checks browser and server TypeScript before bundling the UI.
Neither command starts a voice session or proves live browser acceptance.

## Deployment and Process Ownership

The launcher consumes `--runtime-config` and `--profile` before forwarding normal
Vite arguments. CLI selection overrides environment selection, then documented
root discovery and the file's selected profile. Invalid explicit inputs fail.

One selected root/profile supplies managed and direct debugging launches. Before
starting children, Console calls Gateway's read-only `show-config` command and
validates the reported paths, profile and instance/session pair. It supplies the
validated `KAGUYA_INSTANCE_ID` / `KAGUYA_SESSION_ID` and absolute runtime selection
after child environment overrides.

The Vite server starts Rust Supervisor with app autostart disabled. `Start App`
launches the configured eager graph; individual process cards support direct
Gateway/voice-stack debugging. These ownership modes block one another to avoid
launching duplicate services. Console does not switch profiles implicitly.

The proxy currently targets Gateway at `127.0.0.1:8080` and Supervisor at
`127.0.0.1:3001`. Runtime parser consolidation, address derivation, structured
launch descriptors and complete environment policy remain R1d work. Current
shell command strings are trusted executable configuration.

## State and UI

`src/store.ts` stores a capped semantic WebSocket event log and derives assistant
rows with pure selectors. `generation_id` identifies a Talker dispatch; sentence,
emotion and completion events update that generation only. Unknown IDs cannot
attach output to another open row. User rows come from voice transcript events
or locally recorded typed prompts. Session bootstrap supplies the configured
`instance_id` and `session_id` separately from the capped log.

Microphone PCM streams directly to Gateway, outside the React event log. Its
live meter reads an `AnalyserNode` through an animation-frame loop. The UI does
not display TTS byte counts or playback latency without a producer for those
measurements.

| Region | Implemented behavior |
| --- | --- |
| TopBar | Version, session identity, WebSocket state/uptime, event counts, reconnect and app controls |
| LeftRail | Microphone meter/toggle and managed/direct process cards |
| TurnsPanel | User and Talker timeline, transcript search, generation status and sentence counts |
| Inspector | Live/latest response or selected generation identity, duration and sentence timestamps |
| LogsPanel | Supervisor/direct-launch SSE logs, source/level/regex filters, pause, clear, save and collapse |

Log levels are parsed from text and are best effort. Buffers and retry settings
live in `src/config.ts`; state is ephemeral. The Console has no durable session
replay or task/approval authority.

## Source Layout

- `server/dev.mjs`: launcher argument handling.
- `server/runtime-selection.mjs`: current runtime-file projection and launch plan.
- `server/plugin.ts`: Supervisor proxy and direct debugging process lifecycle.
- `src/ws.ts`, `src/types.ts`, `src/store.ts`: wire client, HTTP/WS shapes and UI projections.
- `src/audio/`: microphone capture worklet and meter.
- `src/regions/`: TopBar, LeftRail, TurnsPanel, Inspector and LogsPanel.
- `vite.config.ts`: development proxies and application version injection.

## Remaining Work

R1-R6 establish the state, policy and execution contracts that the R7 views will
consume. Remaining Console work includes transcript recovery, task/workspace and
approval surfaces, Talker-to-browser speech transport, authoritative structured
logs, device selection and validated lifecycle error reporting. Debug
instrumentation can be added with a concrete requirement and a complete wire
contract. Tokens stay inside the inference service, and raw audio stays outside
protobuf.
