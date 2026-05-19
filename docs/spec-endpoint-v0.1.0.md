# Project Kaguya — Dev Console UI Specification

**Component:** Dev Console (Phase 1 debug UI)
**Version:** 0.1.0
**Date:** April 2026
**Audience:** Developers building and testing the Kaguya voice pipeline
**Replaces:** M6 placeholder in `implementation-plan-v0.1.0.md`

---

## 1. Purpose

The Dev Console is a browser-based single-page application for developing and debugging the Kaguya voice pipeline. It connects to the Gateway via WebSocket, providing:

- Microphone capture and audio playback (voice in/out)
- Text input fallback (type instead of talk)
- Service lifecycle management (start/stop/restart Gateway, Talker, llama.cpp)
- Real-time log viewing with configurable levels
- Internal state observability (Input Stream events, TalkerContext, TalkerOutput)
- Log persistence to disk

The Dev Console is a **Phase 1 development tool**. It may be retained as a debug tool in Phase 2 (OpenPod) or retired. No production user-facing logic lives here.

---

## 2. Architecture

```
┌────────────────────────────────────┐
│  Browser (React + Vite)            │
│  ├── AudioWorklet: mic → PCM out   │
│  ├── AudioWorklet: PCM in → spkr   │
│  ├── WS client: /ws (hot path)     │
│  └── WS client: /ws/debug (telemetry) │
└──────────┬──────────┬──────────────┘
           │          │
      /ws  │    /ws/debug
           │          │
┌──────────┴──────────┴──────────────┐
│  Gateway (Rust / axum)             │
│  ├── /ws      — audio + text + ctrl│
│  ├── /ws/debug — debug telemetry   │
│  └── /health  — liveness probe     │
└────────────────────────────────────┘
```

### 2.1 Two WebSocket Channels

| Endpoint     | Purpose                                        | Traffic Profile         |
| ------------ | ---------------------------------------------- | ----------------------- |
| `/ws`        | Hot path: audio binary frames, text commands, control signals, conversation metadata | High-frequency (50fps audio) |
| `/ws/debug`  | Cold path: log stream, Input Stream events, TalkerContext snapshots, TalkerOutput events, process health | Low-frequency (event-driven) |

**Rationale:** Separating debug telemetry from the hot path ensures observability cannot degrade audio latency. The debug channel adds zero overhead when no client is connected (sender is `Option<mpsc::Sender>` — events are only cloned when a debug client exists).

### 2.2 Gateway Modifications

Debug telemetry requires Gateway-side changes gated behind a **Cargo feature flag** `dev-console`:

```toml
[features]
dev-console = []
```

When enabled:
- The `/ws/debug` WebSocket endpoint is compiled in and served alongside `/ws`.
- The main event loop clones events to an optional `debug_tx: Option<mpsc::Sender<DebugEvent>>` as they flow through `tokio::select!`.
- `TalkerContext` is serialized to JSON and forwarded to the debug channel before dispatch.
- `TalkerOutput` messages are forwarded to the debug channel as they arrive.
- Gateway log output (tracing spans) is tee'd to the debug channel via a custom `tracing_subscriber::Layer`.

When disabled (`--release` without feature):
- `/ws/debug` does not exist. The `debug_tx` sender is `None`. Zero runtime cost.
- All debug-specific code is compiled out via `#[cfg(feature = "dev-console")]`.

**No changes to the Talker, Reasoner, or Toolkit.** All debug data is intercepted at the Gateway level.

### 2.3 Migration Path: /ws/debug to /ws

If specific telemetry events prove useful for end users (e.g., emotion tags, task progress), promoting them from `/ws/debug` to `/ws` is a one-line routing change per message type in the Gateway's output mux. The message format is identical on both channels.

### 2.4 Connection Constraints

The dev console supports **exactly one connected browser client** at a time. The Gateway's output channels (`audio_out_rx`, `metadata_rx`) are `tokio::sync::mpsc` — single-consumer. If a second client connects while one is active, audio and metadata frames would be split non-deterministically between the two, producing garbled playback on both.

**Enforcement:** On new WebSocket upgrade to `/ws`, the Gateway closes any existing client connection before accepting the new one. This means refreshing the page works seamlessly (old connection is dropped, new one takes over), and accidentally opening a second tab gets a clean single-stream experience rather than a silent data-splitting bug.

**Phase 2 upgrade path:** If multi-client becomes necessary (e.g., a shared debugging session), replace `mpsc` with `tokio::sync::broadcast` for the output channels. Each new client calls `tx.subscribe()` to get its own `Receiver`. `Bytes` and `MetadataEvent` are both cheap to clone. This is ~5 lines of change in `output.rs`.

---

## 3. Technology Stack

| Layer     | Choice                | Rationale                                                   |
| --------- | --------------------- | ----------------------------------------------------------- |
| Framework | React 19 + TypeScript | Already in stack (Reasoner, Toolkit). Largest ecosystem.    |
| Bundler   | Vite                  | Fast HMR, minimal config, TypeScript out of the box.        |
| Styling   | CSS Modules or Tailwind CSS | Minimalist — no component library overhead.           |
| Audio     | AudioWorklet API      | Low-latency mic capture and playback in browser.            |
| WebSocket | Native browser `WebSocket` | No library needed for two simple connections.          |
| State     | React `useReducer` + Context | Sufficient for single-page; no Redux overhead.       |

**Future option:** If native desktop capabilities are needed (system monitoring, screen capture beyond `getDisplayMedia`), wrap the same React app in **Tauri** (Rust shell, ~3MB). No UI rewrite required.

---

## 4. WebSocket Protocol

### 4.1 Hot Path: `/ws` (existing, extended)

**Client → Gateway (ingress):**

| Frame Type | Format | Routing |
| ---------- | ------ | ------- |
| Binary | Raw PCM bytes (16-bit signed, 16kHz mono) | → Listener (audio) |
| Text/JSON | `{"type": "text", "content": "..."}` | → Input Stream P1 (TextCommand) |
| Text/JSON | `{"type": "control", "command": "stop"}` | → P0 bypass (ControlSignal::Stop) |
| Text/JSON | `{"type": "control", "command": "shutdown"}` | → P0 bypass (ControlSignal::Shutdown) |

**Gateway → Client (egress):**

| Frame Type | Format | Source |
| ---------- | ------ | ------ |
| Binary | Raw PCM bytes (TTS audio) | Talker TTS output |
| Text/JSON | `{"type": "sentence", "text": "..."}` | TalkerOutput::SentenceEvent |
| Text/JSON | `{"type": "emotion", "emotion": "joy"}` | TalkerOutput::EmotionEvent |
| Text/JSON | `{"type": "response_started", "turn_id": "..."}` | TalkerOutput::ResponseStarted |
| Text/JSON | `{"type": "response_complete", "turn_id": "...", "interrupted": false}` | TalkerOutput::ResponseComplete |

### 4.2 Debug Path: `/ws/debug` (new, feature-gated)

All messages are JSON. Direction is Gateway → Client only (read-only telemetry).

```typescript
// Union type for all debug messages
type DebugMessage =
  | { type: "log"; level: "DEBUG" | "INFO" | "WARN" | "ERROR"; target: string; message: string; timestamp: string }
  | { type: "input_event"; priority: "P0" | "P1" | "P2" | "P3" | "P4" | "P5"; event: string; detail: object; timestamp: string }
  | { type: "talker_context"; turn_id: string; context: object; timestamp: string }
  | { type: "talker_output"; seq: number; payload_type: string; payload: object; timestamp: string }
  | { type: "process_health"; processes: ProcessStatus[]; timestamp: string }
  | { type: "state_snapshot"; history_len: number; active_tasks: number; silence_timer: string | null; timestamp: string }

type ProcessStatus = {
  name: "gateway" | "talker" | "llm_server";
  status: "running" | "stopped" | "unreachable";
  pid?: number;
  uptime_secs?: number;
}
```

**Client → Gateway (debug commands):**

```typescript
// Optional: request specific debug data
type DebugCommand =
  | { type: "set_log_level"; level: "DEBUG" | "INFO" | "WARN" | "ERROR" }
  | { type: "request_state_snapshot" }
```

### 4.3 Audio Format

**Phase 1:** Raw PCM (16-bit signed little-endian, 16kHz, mono) in both directions. No Opus encoding. The Listener performs any codec work server-side.

**Known test gap:** This means the Listener's Opus decode path (`voice/opus_decoder.py`, REF-002) is never exercised from the dev console. In Phase 2, OpenPod will deliver Opus frames and the Listener's `OpusDecoder` will be on the critical audio path. To mitigate: write a standalone integration test that feeds Opus-encoded audio directly to the Listener's `opus_decoder.decode()` → `recorder.feed_audio()` path, independent of the endpoint transport.

**[OPEN] Phase 2 expansion:** Opus encoding in the browser (via `opus-stream-decoder` or WebAssembly libopus) to reduce bandwidth. The Gateway forwards Opus frames to the Listener unchanged. This is an additive change — raw PCM remains a fallback.

### 4.4 WebSocket Reconnection

Both `/ws` and `/ws/debug` clients must auto-reconnect when the connection drops. During development, the Gateway restarts frequently — without reconnection, the UI goes dead and requires a manual page refresh.

**Strategy:** Exponential backoff with jitter and bounded retries.

- Retry delays: `[0, 300, 1200, 2700, 4800, 7000]` ms (polynomial growth, capped at 7s).
- Add random jitter of 0–1000ms after the first attempt to avoid thundering herd on restart.
- After exhausting the retry array, stop retrying and show a "Disconnected — click to reconnect" banner in the toolbar.
- On successful reconnection, reset the retry counter.
- Implementation: ~20 lines in `ws/hotpath.ts` and `ws/debug.ts`. No library needed — `setTimeout` + `new WebSocket()` in a retry loop.

This matches the pattern used by LiveKit's JS client (`DefaultReconnectPolicy`) and Pipecat's `ReconnectingWebSocket`, both of which use polynomial/exponential backoff with bounded retries.

---

## 5. UI Layout

Single-page layout. Three horizontal regions, top to bottom:

```
┌─────────────────────────────────────────────────┐
│  TOOLBAR                                         │
│  [Start All] [Stop All]  Gateway: ● Talker: ●   │
│  LLM Server: ●           Logs: [DEBUG ▾]         │
├────────────────────────────┬────────────────────┤
│  CONVERSATION              │  INSPECTOR          │
│                            │                     │
│  [User]: What time is it?  │  ▸ Input Stream     │
│  [Kaguya]: It's 3:42 PM.  │  ▸ TalkerContext     │
│  [Emotion: neutral]        │  ▸ TalkerOutput     │
│                            │  ▸ Process Health    │
│                            │                     │
│  ┌─────────────────────┐   │                     │
│  │ Type a message...   │   │                     │
│  └─────────────────────┘   │                     │
│  [🎤 Mic: ON] [Send]      │                     │
├────────────────────────────┴────────────────────┤
│  LOG PANEL                                       │
│  [14:30:01.234 INFO  gateway] P1: user intent... │
│  [14:30:01.456 DEBUG talker] response started    │
│  [Save Logs]                        [Clear]      │
└─────────────────────────────────────────────────┘
```

### 5.1 Toolbar

- **Process controls:** Individual Start / Stop / Restart buttons for Gateway and Talker. LLM Server shows status indicator only (green/yellow/red) with a tooltip showing the configured URL — no start/stop (LM Studio is managed externally).
- **Status indicators:** Colored dots. Green = running/connected. Red = stopped/unreachable. Yellow = starting/connecting.
- **Log level selector:** Dropdown to set minimum log level for the Log Panel. Applies client-side filter (all levels always stream from Gateway; client filters display). Also sends `set_log_level` debug command to Gateway to reduce server-side noise if desired.

### 5.2 Conversation Panel (left)

- Displays the conversation as the user experiences it: user messages (voice transcripts and typed text) and Kaguya's responses (sentence-by-sentence as they stream in).
- Emotion tags displayed inline as subtle badges next to the relevant response.
- Text input field at bottom with Send button.
- Mic toggle button: when ON, browser captures audio and streams PCM to `/ws`. Visual indicator (e.g., pulsing dot) when mic is active.
- No waveform visualization in v1. **[OPEN]** Add later if useful.

### 5.3 Inspector Panel (right)

Collapsible sections showing real-time internal state from `/ws/debug`:

- **Input Stream:** Live feed of events entering the priority queue. Each event shows: priority level (P0-P5), event type, timestamp, and a one-line summary. Color-coded by priority.
- **TalkerContext:** The most recent context package sent to the Talker, rendered as a collapsible JSON tree. Shows: user_input, history length, memory_contents (truncated), tool list, active tasks.
- **TalkerOutput:** The most recent TalkerOutput stream rendered event-by-event: ResponseStarted, SentenceEvent, EmotionEvent, ToolRequest, DelegateRequest, ResponseComplete. Each with seq number and timestamp.
- **Process Health:** Current status of each managed process. Updated on a poll interval (every 5s from Gateway).

Each section is independently collapsible to reduce noise. Sections auto-scroll to latest entry. Click an entry to freeze/inspect it.

### 5.4 Log Panel (bottom)

- Streams structured log entries from the Gateway's `tracing` output (via `/ws/debug`).
- Each entry: `[timestamp] [LEVEL] [target] message`
- Color-coded by level: DEBUG=gray, INFO=white, WARN=yellow, ERROR=red.
- Client-side filter by level (controlled by toolbar dropdown).
- **Save Logs** button: downloads the current log buffer as a `.log` text file with timestamp in filename.
- **Clear** button: clears the display buffer (does not affect server-side logging).
- Scrollback buffer: retain last 10,000 entries in memory. Older entries are discarded from the UI (but persisted to the downloaded file if saved before discard).

---

## 6. Process Management

### 6.1 Managed Processes

| Process | Start Command | Stop Method | Health Check |
| ------- | ------------- | ----------- | ------------ |
| Gateway | `cargo run --features dev-console` (from `gateway/`) | SIGTERM (graceful) or WS shutdown command | `/health` HTTP endpoint (200 OK) |
| Talker  | `python -m talker.main` (from `talker/`, in conda env `kaguya`) | SIGTERM | gRPC health or process alive check |
| LLM Server | N/A (started externally via LM Studio) | N/A | HTTP GET to configured `llm_base_url` (e.g., `http://localhost:8080/health`) |

### 6.2 Process Supervisor

The UI does **not** manage processes directly from the browser. Instead, a lightweight **process supervisor backend** runs alongside the UI dev server:

```
┌──────────────────┐     HTTP/WS      ┌────────────────────┐
│  Browser (React)  │ ◄──────────────► │  Dev Server (Node)  │
│                   │                  │  ├── Vite (HMR)     │
│                   │                  │  └── Supervisor API │
└──────────────────┘                  └────────┬───────────┘
                                              │ child_process.spawn
                                    ┌─────────┼──────────┐
                                    ▼         ▼          ▼
                                 Gateway   Talker    (LLM: poll only)
```

The supervisor is a small Express/Fastify sidecar (or integrated into Vite's dev server as middleware) that:
- Spawns child processes with configured commands and working directories.
- Captures stdout/stderr and forwards to the UI as structured log events.
- Tracks PIDs, uptime, exit codes.
- Exposes a simple REST API: `POST /api/process/:name/start`, `POST /api/process/:name/stop`, `POST /api/process/:name/restart`, `GET /api/process/status`.
- Reads process configuration from a config file (`console/supervisor.json`).

**Rationale:** Browsers cannot spawn OS processes. A Node.js sidecar is the minimal addition that enables process management without requiring a separate tool (like `pm2` or `supervisord`). It runs as part of the dev server — no extra process to manage.

### 6.3 Supervisor Configuration

```jsonc
// console/supervisor.json
{
  "processes": {
    "gateway": {
      "command": "cargo run --features dev-console",
      "cwd": "../gateway",
      "env": { "RUST_LOG": "kaguya_gateway=debug" }
    },
    "talker": {
      "command": "python -m talker.main",
      "cwd": "../talker",
      "env": {},
      "shell": "conda run -n kaguya --no-banner"
    },
    "llm_server": {
      "managed": false,
      "health_url": "http://localhost:8080/health",
      "poll_interval_ms": 5000
    }
  }
}
```

---

## 7. Audio Pipeline

### 7.1 Mic Capture (Browser → Gateway)

1. Create an `AudioContext` with `sampleRate: 16000`. The AudioContext handles resampling from the hardware's native rate (typically 48kHz) to 16kHz internally — no manual resampler in the worklet.
2. Call `navigator.mediaDevices.getUserMedia({ audio: true })` — do **not** constrain `sampleRate` in the getUserMedia call. Most browsers ignore this constraint and return the device's native rate regardless. Let the AudioContext handle rate conversion.
3. Connect the `MediaStream` source to an `AudioWorkletNode` that:
   - Converts `Float32Array` samples to 16-bit signed integers (PCM16).
   - Buffers into 20ms chunks (320 samples = 640 bytes at 16kHz) and posts to the main thread via `port.postMessage`.
4. Main thread sends each chunk as a binary WebSocket frame to `/ws`.

**Why AudioContext resampling, not worklet resampling:** This is the pattern used by Pipecat's `WavRecorder` and LiveKit's JS client. The browser's internal resampler is native code, runs off the audio thread, and handles edge cases (non-integer ratios, anti-aliasing) that a manual worklet resampler would need to reimplement. A manual 48kHz→16kHz downsampler in the worklet is ~30 lines but introduces subtle aliasing artifacts if done with naive linear interpolation.

### 7.2 Audio Playback (Gateway → Browser)

1. Binary WebSocket frames from `/ws` contain raw PCM16 (16kHz mono) from the Talker's TTS output.
2. Use the **same `AudioContext`** created for mic capture (already configured at `sampleRate: 16000`). The AudioContext upsamples to the hardware output rate internally.
3. Route through an `AudioWorkletNode` that:
   - Converts 16-bit signed integers back to `Float32Array`.
   - Queues samples in a ring buffer.
   - Pulls from the buffer in the `process()` callback to feed the audio output.
4. The AudioContext's destination node handles 16kHz → device output rate conversion transparently.

### 7.3 Mic Toggle

- A toggle button in the UI controls whether mic audio is captured and streamed.
- When OFF: `AudioWorkletNode` is disconnected (no processing, no WebSocket sends).
- When ON: connected and streaming.
- Default state: OFF (user must explicitly enable mic).

---

## 8. Logging and Persistence

### 8.1 Log Sources

| Source | Mechanism |
| ------ | --------- |
| Gateway (`tracing`) | Custom `tracing_subscriber::Layer` forwards formatted spans/events to `debug_tx` channel → `/ws/debug` |
| Talker (Python `logging`) | Supervisor captures stdout/stderr → parses structured log lines → forwards to UI |
| LLM Server | Not captured (externally managed). Health polling only. |

### 8.2 Log Persistence

- **In-browser:** 10,000 entry ring buffer. "Save Logs" button exports as `.log` file.
- **Server-side:** The supervisor process writes all captured stdout/stderr to rotating log files in `console/logs/` directory:
  - `gateway-YYYY-MM-DD.log`
  - `talker-YYYY-MM-DD.log`
  - Retention: 7 days (configurable). Oldest files auto-deleted.
- Log files are plain text, one entry per line, with timestamp and level prefix.

---

## 9. Gateway-Side Changes (Summary)

All changes are in `gateway/src/endpoint.rs` (and new `gateway/src/endpoint/debug.rs`), gated behind `#[cfg(feature = "dev-console")]`.

### 9.1 New Types

```rust
#[cfg(feature = "dev-console")]
pub enum DebugEvent {
    Log { level: String, target: String, message: String },
    InputEvent { priority: u8, event_type: String, detail: serde_json::Value },
    TalkerContext { turn_id: String, context: serde_json::Value },
    TalkerOutput { seq: u32, payload_type: String, payload: serde_json::Value },
    ProcessHealth { processes: Vec<ProcessStatus> },
    StateSnapshot { history_len: usize, active_tasks: usize, silence_timer: Option<String> },
}
```

### 9.2 Integration Points in Main Event Loop

At each point in the main `tokio::select!` loop where an event is processed, add:

```rust
#[cfg(feature = "dev-console")]
if let Some(tx) = &debug_tx {
    let _ = tx.try_send(DebugEvent::InputEvent { ... });
}
```

This is ~1-2 lines per match arm. `try_send` is non-blocking — if the debug channel is full or disconnected, the event is silently dropped. The hot path is never blocked.

### 9.3 Tracing Layer

A custom `tracing_subscriber::Layer` that formats log events and sends them to `debug_tx`:

```rust
#[cfg(feature = "dev-console")]
struct DebugTracingLayer {
    tx: mpsc::Sender<DebugEvent>,
}
```

Composed with the existing `fmt` layer via `tracing_subscriber::registry().with(fmt_layer).with(debug_layer)`.

---

## 10. Directory Structure

```
kaguya/
├── console/                       # Dev Console (React + Vite + TypeScript)
│   ├── public/
│   ├── src/
│   │   ├── main.tsx               # React entry point
│   │   ├── App.tsx                # Root layout (toolbar + panels)
│   │   ├── components/
│   │   │   ├── Toolbar.tsx        # Process controls, status, log level
│   │   │   ├── Conversation.tsx   # Chat display, text input, mic toggle
│   │   │   ├── Inspector.tsx      # Debug panels (input stream, context, output)
│   │   │   ├── LogPanel.tsx       # Structured log viewer
│   │   │   └── ProcessControl.tsx # Individual process start/stop/restart
│   │   ├── audio/
│   │   │   ├── capture.ts         # AudioWorklet mic capture → PCM
│   │   │   ├── playback.ts        # PCM → AudioWorklet speaker output
│   │   │   └── worklet.ts         # AudioWorkletProcessor (runs in audio thread)
│   │   ├── ws/
│   │   │   ├── hotpath.ts         # /ws connection manager (audio + text + control)
│   │   │   └── debug.ts           # /ws/debug connection manager (telemetry)
│   │   ├── supervisor/
│   │   │   └── client.ts          # REST client for supervisor API
│   │   ├── hooks/
│   │   │   ├── useAudio.ts        # Mic capture + playback state
│   │   │   ├── useWebSocket.ts    # WS connection lifecycle
│   │   │   └── useProcesses.ts    # Process status polling
│   │   ├── state/
│   │   │   ├── reducer.ts         # App state reducer
│   │   │   └── types.ts           # TypeScript types for all WS messages
│   │   └── styles/
│   │       └── ...                # CSS modules or Tailwind config
│   ├── server/
│   │   ├── index.ts               # Dev server entry (Vite + supervisor middleware)
│   │   └── supervisor.ts          # Process supervisor (spawn, capture, health)
│   ├── logs/                      # Persisted log files (gitignored)
│   ├── supervisor.json            # Process configuration
│   ├── index.html
│   ├── vite.config.ts
│   ├── tsconfig.json
│   └── package.json
```

**Placement rationale:** `console/` is a top-level directory (not inside `gateway/` or `tools/`) because it is a standalone process with its own dev server, dependencies, and build step. It depends on the Gateway being reachable but does not share code with it.

---

## 11. Configuration

### 11.1 Console Config

```jsonc
// console/supervisor.json (process management)
{
  "processes": { ... }  // See §6.3
}
```

```typescript
// console/src/config.ts (connection defaults)
export const config = {
  gatewayWsUrl: "ws://127.0.0.1:8080/ws",
  gatewayDebugUrl: "ws://127.0.0.1:8080/ws/debug",
  gatewayHealthUrl: "http://127.0.0.1:8080/health",
  supervisorUrl: "http://127.0.0.1:3001/api",
  defaultLogLevel: "INFO",
  logBufferSize: 10_000,
  processHealthPollMs: 5_000,
} as const;
```

### 11.2 Gateway Config Addition

```toml
# config/gateway.toml — new section
[console]
ws_debug_addr = "127.0.0.1:8080"  # shares addr with /ws; just a new route
```

No new port needed — `/ws/debug` is an additional route on the same axum server as `/ws` and `/health`.

---

## 12. Implementation Order

### E0 — Scaffold (blocks everything)

- [ ] Init `console/` as Vite + React + TypeScript project.
- [ ] Set up `package.json` with dependencies: `react`, `react-dom`, `vite`, `@vitejs/plugin-react`, `typescript`.
- [ ] Create `vite.config.ts` with dev server proxy (forward `/ws` and `/ws/debug` to Gateway at `127.0.0.1:8080`).
- [ ] Stub `App.tsx` with three-panel layout (toolbar, conversation+inspector, logs).
- [ ] Verify `npm run dev` serves the UI at `http://localhost:3000`.

### E1 — WebSocket Hot Path + Text Input

- [ ] Implement `/ws` connection manager (`ws/hotpath.ts`).
- [ ] Text input component: type message → send JSON `{"type": "text", "content": "..."}` over `/ws`.
- [ ] Receive and display `sentence`, `emotion`, `response_started`, `response_complete` events in Conversation panel.
- [ ] Control buttons: Stop (sends `{"type": "control", "command": "stop"}`).
- [ ] **Done when:** Can type a message, see it dispatched to Gateway, see Talker response stream back sentence-by-sentence.

### E2 — Audio Pipeline

- [ ] Implement `AudioWorklet` for mic capture (16kHz mono PCM16).
- [ ] Implement `AudioWorklet` for playback (PCM16 → speakers).
- [ ] Mic toggle button with visual indicator.
- [ ] Wire binary frames to/from `/ws`.
- [ ] **Done when:** Can speak into mic, hear Kaguya's TTS response through speakers.

### E3 — Process Supervisor

- [ ] Implement supervisor backend (`server/supervisor.ts`): spawn, kill, health check, stdout/stderr capture.
- [ ] REST API: `/api/process/:name/start`, `stop`, `restart`, `status`.
- [ ] Integrate as Vite dev server middleware or standalone Express sidecar.
- [ ] Supervisor configuration via `supervisor.json`.
- [ ] Toolbar UI: Start/Stop/Restart buttons for Gateway and Talker. Status indicator for LLM Server (poll only).
- [ ] **Done when:** Can start Gateway and Talker from the UI, see status indicators update.

### E4 — Debug Channel + Inspector

- [ ] **Gateway:** Add `#[cfg(feature = "dev-console")]` gated `/ws/debug` endpoint.
- [ ] **Gateway:** Add `debug_tx` channel. Clone Input Stream events, TalkerContext, TalkerOutput to it.
- [ ] **Gateway:** Add custom tracing Layer to forward logs to `debug_tx`.
- [ ] **UI:** Implement `/ws/debug` connection manager (`ws/debug.ts`).
- [ ] **UI:** Inspector panel: Input Stream feed, TalkerContext viewer, TalkerOutput viewer, Process Health.
- [ ] **Done when:** Can see Input Stream events flow in real-time, inspect TalkerContext JSON, watch TalkerOutput sequence.

### E5 — Log Panel + Persistence

- [ ] Log Panel component with timestamp, level, target, message columns.
- [ ] Client-side level filtering (toolbar dropdown).
- [ ] 10,000 entry ring buffer in browser.
- [ ] "Save Logs" button → download as `.log` file.
- [ ] "Clear" button.
- [ ] Server-side log persistence in supervisor (write captured stdout/stderr to `console/logs/`).
- [ ] **Done when:** Logs stream from Gateway and Talker, filterable by level, downloadable as file.

---

## 13. Open Questions

| # | Question | Resolve By |
| - | -------- | ---------- |
| EQ1 | Opus encoding in browser for bandwidth reduction. Worth adding in Phase 1 or defer? | After E2 — measure raw PCM bandwidth on localhost. |
| EQ2 | Waveform visualization. Useful enough to add? | After E2 — user feedback on audio debugging needs. |
| EQ3 | Tailwind vs CSS Modules for styling. | E0 — decide during scaffold based on contributor preference. |
| EQ4 | Should the supervisor persist between UI restarts (daemon mode), or is it tied to `npm run dev` lifecycle? | E3 — decide during implementation. Tied to dev server is simpler. |
| EQ5 | Audio format negotiation — should the Gateway advertise supported formats, or is raw PCM hardcoded? | Defer to Phase 2 (Opus expansion). |
| EQ6 | Tauri wrapping — when does native capability become necessary? | Post-Phase 1 evaluation. |

---

## 14. Non-Goals (Out of Scope)

- OpenPod protocol integration (Phase 2).
- Multi-user / multi-session support.
- Production-grade authentication or access control.
- Opus encoding/decoding in browser (Phase 1 uses raw PCM).
- Session recording and replay.
- Waveform visualization.
- Mobile or responsive layout (desktop browser only).

---

## 15. Cross-OS Compatibility

The dev console targets Linux as the primary development platform (WSL2), with macOS and Windows as secondary. Browser-side code (React, AudioWorklet, WebSocket) is OS-agnostic. Server-side code (process supervisor, file I/O) has OS-specific concerns documented below.

### 15.1 Process Supervisor

**Signal handling:** `child_process.spawn` + `process.kill(pid, 'SIGTERM')` works on Linux and macOS. On Windows, `SIGTERM` is not a real signal — Node.js translates it to `TerminateProcess()`, which is an unconditional kill (no graceful shutdown). If Windows native support is needed, send an IPC shutdown message to the child before killing the process — the child handles cleanup on receipt, then the supervisor kills after a timeout. This is the approach used by LiveKit Agents' `supervised_proc.py` (sends `DumpStackTraceRequest` via IPC on Windows instead of `SIGUSR1`).

**Conda activation:** The supervisor config uses `"shell": "conda run -n kaguya --no-banner"` for the Talker. `conda run` works cross-platform (Linux, macOS, Windows) without requiring shell-specific activation scripts (`source activate` vs. `conda.bat activate`). This is the correct portable approach.

**Path separators:** `supervisor.ts` must use `path.join()` for all file paths (log files, working directories). Never hardcode `/` as a separator.

### 15.2 AudioWorklet

**macOS known issue:** AudioWorklet `process()` passthrough (copying input directly to output) produces choppy audio on some macOS devices. This is a known WebKit/Chrome bug documented in Pipecat's `audio_processor` worklet. The playback worklet avoids this by using a ring buffer (no direct input-to-output copy).

**Sample rate:** `AudioContext({ sampleRate: 16000 })` is supported on all modern browsers (Chrome 64+, Firefox 61+, Safari 14.1+). Older Safari versions silently ignore the rate and use the hardware default — test on target Safari versions if macOS support is required.

### 15.3 Gateway IPC

The Gateway uses Unix domain sockets (`/tmp/kaguya-gateway.sock`) for gRPC communication with the Talker and Reasoner. This works on Linux, macOS, and WSL2. Native Windows does not support UDS — if native Windows Gateway operation is ever needed, add a TCP fallback for gRPC transport. The dev console itself (WebSocket over TCP) is unaffected.

### 15.4 Log File Persistence

Supervisor log files (`console/logs/`) use date-stamped filenames (`gateway-YYYY-MM-DD.log`). File rotation and deletion use `fs.unlink` — no OS-specific concern. Ensure the `logs/` directory is created with `fs.mkdirSync(path, { recursive: true })` on supervisor startup.

---
---

# Appendix A — Lean v0 Subset

Sections 1–14 above describe the full vision. This appendix defines the **minimal buildable subset** — what to actually ship first. Everything above remains the north star; this is the starting line.

## A.1 Philosophy

Get a working conversation loop (text + voice) with process management and scrolling logs. No debug introspection, no feature flags, no custom tracing layers, no separate debug WebSocket. Add those when a real debugging need demands them.

## A.2 What v0 Includes

| Capability | Scope |
| ---------- | ----- |
| Text chat | Type a message, see streamed response |
| Mic capture + playback | Browser AudioWorklet, raw PCM16, 16kHz mono |
| Process management | Start/stop Gateway and Talker from UI. LLM Server: poll-only health check |
| Logs | Pipe Gateway + Talker stdout/stderr into a scrolling log panel. Level filter client-side. Save to file |
| Connection status | Green/red dot for Gateway WS, Talker process, LLM Server |

## A.3 What v0 Defers

Everything in sections 4.2, 5.3, 9, and milestones E4–E5's debug-introspection parts:

- `/ws/debug` separate channel — just use `/ws` for everything. Split later if needed.
- Cargo feature flag `dev-console` — use a runtime config toggle or just always-on during dev.
- Custom `tracing_subscriber::Layer` — supervisor captures stdout, that's the log source.
- Inspector panel (Input Stream events, TalkerContext viewer, TalkerOutput viewer) — defer entirely.
- State snapshots over WebSocket.
- `DebugEvent` enum and Gateway-side debug instrumentation.

## A.4 Simplified Architecture

```
┌──────────────────────────────┐
│  Browser (React + Vite)      │
│  ├── AudioWorklet: mic/spkr  │
│  └── WS client: /ws          │
└──────────────┬───────────────┘
               │ ws://127.0.0.1:8080/ws
┌──────────────┴───────────────┐
│  Gateway (Rust / axum)       │
│  ├── /ws    (existing)       │
│  └── /health (existing)      │
└──────────────────────────────┘

Process supervisor: part of Vite dev server.
Captures stdout/stderr from child processes.
No REST API — direct function calls from server middleware.
```

One WebSocket connection. The Gateway's existing `/ws` endpoint handles all traffic. No Gateway code changes needed for v0 (the current `endpoint.rs` already handles text commands and control signals; audio binary frame forwarding may need minor extension).

## A.5 v0 Directory Structure

```
console/
├── src/
│   ├── main.tsx
│   ├── App.tsx                # Single-page: toolbar + conversation + logs
│   ├── components/
│   │   ├── Toolbar.tsx        # Process buttons, status dots, log level dropdown
│   │   ├── Conversation.tsx   # Chat messages, text input, mic toggle
│   │   └── LogPanel.tsx       # Scrolling log viewer, save/clear
│   ├── audio/
│   │   ├── capture.ts         # Mic → PCM16 → WS binary frames
│   │   ├── playback.ts        # WS binary frames → PCM16 → speakers
│   │   └── worklet.ts         # AudioWorkletProcessor
│   ├── ws.ts                  # Single /ws connection manager
│   ├── config.ts              # URLs, defaults
│   └── types.ts               # WS message types
├── server/
│   └── supervisor.ts          # spawn/kill child processes, pipe stdout
├── supervisor.json            # Process commands and health URLs
├── index.html
├── vite.config.ts
├── tsconfig.json
└── package.json
```

~12 source files. No state management library, no REST API layer, no debug channel client.

## A.6 v0 Implementation Order

### v0.1 — Scaffold + Text Chat

- [ ] `npm create vite@latest console -- --template react-ts`
- [ ] `vite.config.ts`: proxy `/ws` to `127.0.0.1:8080`.
- [ ] `ws.ts`: connect to `/ws` with auto-reconnect (exponential backoff, see §4.4), send/receive JSON and binary frames.
- [ ] `Conversation.tsx`: display messages, text input + send.
- [ ] `Toolbar.tsx`: stub with connection status dot (green if WS open).
- [ ] **Done when:** Type a message → see Gateway process it (check Gateway logs manually).

### v0.2 — Audio

- [ ] `worklet.ts`: AudioWorkletProcessor for capture and playback.
- [ ] `capture.ts`: `getUserMedia` → AudioWorklet → 20ms PCM16 chunks → WS binary.
- [ ] `playback.ts`: WS binary → AudioWorklet ring buffer → speakers.
- [ ] Mic toggle button in Conversation panel.
- [ ] **Done when:** Speak → hear TTS response. (Requires Gateway + Talker + llama.cpp running.)

### v0.3 — Process Supervisor + Logs

- [ ] `supervisor.ts`: wrap `child_process.spawn`, capture stdout/stderr, track PID/alive.
- [ ] Wire into Vite dev server as middleware (custom plugin or `configureServer` hook).
- [ ] `Toolbar.tsx`: Start/Stop/Restart buttons for Gateway and Talker. Health poll for LLM Server.
- [ ] `LogPanel.tsx`: render captured stdout lines, color by parsed level, filter dropdown, save button.
- [ ] `supervisor.json`: configure process commands.
- [ ] **Done when:** Click "Start Gateway" → process spawns → logs scroll in panel → click "Stop" → process dies.

## A.7 Gateway Changes for v0

Minimal. The existing `endpoint.rs` already handles:
- Text commands (`{"type": "text", "content": "..."}` → P1)
- Control signals (`{"type": "control", "command": "stop"}` → P0)

**Needed additions** (small, in existing `endpoint.rs`):
- Forward binary WebSocket frames (audio from browser) to the Listener's audio input channel.
- Forward Talker TTS audio (from `audio_out_rx`) as binary WebSocket frames to the client.
- Forward metadata events (from `metadata_rx`) as JSON WebSocket frames to the client.

These are already stubbed via `EndpointState.audio_out_rx` and `EndpointState.metadata_rx` — the channels exist, they just need to be wired into the WebSocket send loop. Estimated: ~30 lines added to `handle_ws`. Additionally, enforce the single-client constraint (§2.4): close any existing WS connection before accepting a new one.

## A.8 Graduation Criteria

v0 graduates to the full spec (sections 1–14) when:

1. You find yourself wanting to inspect Input Stream events or TalkerContext during debugging — triggers E4.
2. Localhost audio bandwidth becomes a concern — triggers Opus encoding (EQ1).
3. The single `/ws` channel shows measurable latency from debug traffic — triggers `/ws/debug` split.

Until then, v0 is sufficient.
