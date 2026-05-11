# Kaguya Dev Console

Local web app for supervising and inspecting a Kaguya stack — gateway, talker, llm_server. Provides a chat surface, live transcript timeline, mic/TTS audio meters, process control, and a streaming log viewer.

Protocol surface this UI binds to is defined in `docs/spec-endpoint-v0.1.0.md` (Appendix A) and the gateway-side egress wiring in `gateway/src/output.rs`.

## Running

```bash
cd console
npm install     # first time
npm run dev     # http://localhost:3000
```

The Vite dev server proxies `/ws` and `/health` to `127.0.0.1:8080` (gateway) and mounts an in-process supervisor at `/api/*` for managing/observing child processes. The supervisor's process definitions live in `console/supervisor.json`.

## Layout

```
┌───────────────────────────────────────────────────────────────────────┐
│                              Top bar (44px)                            │
├──────────┬────────────────────────────────────┬───────────────────────┤
│          │           Turns panel              │                       │
│  Left    │   (timeline strip + rows list)     │   Right inspector     │
│  rail    │                                    │   (live monitor or    │
│ (audio + │                                    │    selected-turn      │
│ procs)   │                                    │    detail)            │
│          │                                    │                       │
├──────────┴────────────────────────────────────┴───────────────────────┤
│                Logs panel (collapsible) — prompt bar pinned            │
└───────────────────────────────────────────────────────────────────────┘
```

CSS grid: `44px 1fr var(--logs-h, 240px)` × `248px 1fr 360px`. When the logs panel is collapsed, the third row shrinks to `min-content` and the prompt bar stays visible.

## State model

All UI state derives from a single append-only event log in `src/store.ts`. Every WS frame (in or out), every audio frame (byte counts only — not PCM payloads), and every outbound send is recorded with a timestamp:

```ts
type WsEvent =
  | { kind: "ws_in";     ts: number; msg: EgressMessage }
  | { kind: "ws_out";    ts: number; msg: IngressMessage }
  | { kind: "audio_in";  ts: number; bytes: number }
  | { kind: "audio_out"; ts: number; bytes: number };
```

Direction is browser-relative (`in` = browser RECEIVES, `out` = browser SENDS). The log is a capped ring buffer (`config.eventBufferCap`, default 2000).

Derived shapes — turn lists, streaming-turn lookups, message counters — are pure selectors over the log (`selectTurns`, `selectStreamingTurn`, etc.). No mirror state, no counters held alongside the log; the log is the single source of truth.

**Inference policy.** Anything that can be accurately and computationally derived from the log is derived (turn durations, first-sentence latencies, ingress/egress counters, streaming state). Anything that the gateway knows but doesn't send (tool calls, reasoner steps, session id) is *not* faked — the regions where those would render are left empty (see "Future work" below).

## Modules

`wired` = end-to-end on the existing wire. `skeleton` = layout region exists, no content until the underlying event lands. `partial` = best-effort with documented caveats.

| Region / Module | File(s) | Wire surface | Status |
|---|---|---|---|
| TopBar — brand + version chip | `regions/TopBar/TopBar.tsx` | `package.json` via Vite `define` | wired |
| TopBar — WS status pulse | `regions/TopBar/TopBar.tsx` | `ws.onopen` / `onclose` | wired |
| TopBar — WS uptime | `regions/TopBar/TopBar.tsx` | `wsOpenedAt` observation | wired |
| TopBar — msg counters (↑/↓) | `regions/TopBar/TopBar.tsx` | `selectWsInCount` / `selectWsOutCount` | wired |
| TopBar — Shutdown button | `regions/TopBar/TopBar.tsx` | `{type:"control", command:"shutdown"}` ingress | wired |
| TopBar — session id | — | needs `session_init` egress event | **skeleton (omitted)** |
| LeftRail — Mic strip (meter) | `regions/LeftRail/AudioStrip.tsx`, `audio/capture.ts`, `audio/meter.ts` | MediaStream → AnalyserNode | wired |
| LeftRail — TTS strip (meter) | `regions/LeftRail/AudioStrip.tsx`, `audio/playback.ts`, `audio/meter.ts` | Playback worklet → AnalyserNode — front-end side ready, **but** no TTS audio reaches the browser today ([#36](https://github.com/styin/kaguya/issues/36)) | **bug: no audio source** |
| LeftRail — Open-mic toggle | `regions/LeftRail/LeftRail.tsx` | `startCapture` lifecycle | wired |
| LeftRail — Space-hold PTT | — | needs gateway-side PTT/open-mic VAD policy split | **skeleton (control dropped)** |
| LeftRail — Process cards | `regions/LeftRail/ProcessCard.tsx`, `regions/LeftRail/LeftRail.tsx` | `GET /api/process/status` (1s poll) + `POST /api/process/:name/restart` | wired |
| LeftRail — Device name on audio strips | — | needs device enumeration via `mediaDevices.enumerateDevices()` | **skeleton (generic labels)** |
| TurnsPanel — User lane (timeline strip) | `regions/TurnsPanel/TimelineStrip.tsx` | `user_input` events + `ws_out` text sends | wired |
| TurnsPanel — Talker lane (timeline strip) | `regions/TurnsPanel/TimelineStrip.tsx` | `response_started` / `sentence` / `response_complete` | wired |
| TurnsPanel — Tool lane (timeline strip) | `regions/TurnsPanel/TimelineStrip.tsx` | needs `tool_call` egress event | **skeleton (empty lane)** |
| TurnsPanel — Reasoner lane (timeline strip) | `regions/TurnsPanel/TimelineStrip.tsx` | needs `reasoner_step` / `delegate_*` egress events | **skeleton (empty lane)** |
| TurnsPanel — Turn rows | `regions/TurnsPanel/TurnRow.tsx`, `regions/TurnsPanel/TurnsPanel.tsx` | `selectTurns()` over event log | wired |
| TurnsPanel — Search | `regions/TurnsPanel/TurnsPanel.tsx` | client-side filter on concatenated sentences | wired |
| TurnsPanel — Export | — | not implemented | **deferred** |
| Inspector — Live monitor | `regions/Inspector/LiveMonitor.tsx`, `regions/Inspector/Inspector.tsx` | `selectStreamingTurn()` + latest settled turn | wired |
| Inspector — Turn detail (header + sentence list) | `regions/Inspector/TurnDetail.tsx` | per-turn `sentence` events with relative ts | wired |
| Inspector — Turn detail (tool/reasoner blocks) | — | needs the same events as the timeline strip lanes | **skeleton (sections omitted)** |
| LogsPanel — Log rows | `regions/LogsPanel/LogRow.tsx`, `regions/LogsPanel/LogsPanel.tsx` | `/api/logs/stream` SSE | wired |
| LogsPanel — Source filter chips | `regions/LogsPanel/LogsPanel.tsx` | client-side filter on `entry.source` | wired |
| LogsPanel — Level filter chips | `regions/LogsPanel/LogRow.tsx#parseLevel` | best-effort regex parse; unknown → keep (no fabrication) | partial |
| LogsPanel — Prompt bar | `regions/LogsPanel/PromptBar.tsx` | `{type:"text", content}` ingress | wired |
| LogsPanel — Collapse + Ctrl+\` | `regions/LogsPanel/LogsPanel.tsx` | local state in store | wired |
| LogsPanel — Clear / Save buttons | — | UI not yet wired (icons in design only) | **deferred** |

## Future work — to fully realize the design

For each **skeleton** row above, the underlying protocol expansion required. Pickable in any order.

- [ ] **TTS audio path: talker → gateway → browser.** Today RealtimeTTS in `talker/voice/speaker.py` plays audio directly on the talker host's audio device — bytes never leave the Python process. `gateway/src/output.rs` has `OutputManager::send_audio()` and the WS-binary fanout in `gateway/src/endpoint.rs` is already wired, but nothing ever calls `send_audio()` because there's no source. Fix: (1) tap RealtimeTTS's `on_audio_chunk` callback (or wrap KokoroEngine) to copy synthesized PCM to an `asyncio.Queue`; (2) expose those bytes on a new TCP socket (e.g. `speaker_audio_port = 50057`), mirroring the mic-input direction on `:50056`; (3) add a Rust client in `gateway/src/listener.rs` (sibling of the existing audio forwarder) that connects, reads length-prefixed frames, and calls `output.send_audio(bytes)`. Once that lands the LeftRail TTS strip animates, TTFS resolves to real ms, and per-turn audio KB starts counting — no front-end changes needed. Tracked as [#36](https://github.com/styin/kaguya/issues/36).
- [ ] **`session_init` egress event.** Gateway emits once at WS-open with `{ conversation_id, started_at }`. TopBar binds the session chip (currently shows WS uptime in its place).
- [ ] **`tool_call` egress event.** Emit from `gateway/src/main.rs` `ToolRequest` arm with `{ request_id, tool_name, args_json? }`. Timeline Tool lane plots a block at that ts; Inspector turn-detail surfaces a Tools section.
- [ ] **`tool_result` egress event.** Emit when the P3 `ToolResult` arm processes a result. Same lane / section close out the block.
- [ ] **`delegate_started` / `delegate_completed` egress events.** Emit from `DelegateRequest` and the reasoner-completed paths.
- [ ] **`reasoner_step` egress event.** Forward `InputEvent::ReasonerStep` (currently consumed at P3 only). Reasoner lane gets per-step blocks; Inspector turn-detail gets a per-turn reasoner block.
- [ ] **PTT/open-mic split at gateway VAD policy.** Today the gateway always runs in open-mic-VAD mode. A server-authoritative PTT mode would let the LeftRail restore the segmented control with real semantics.
- [ ] **Device enumeration on audio strips.** Call `mediaDevices.enumerateDevices()` + render a dropdown on each strip (mic input device, output device). Pure browser work — no protocol change.
- [ ] **Configurable buffer caps in UI.** `config.eventBufferCap` and `config.logBufferCap` are constants today; surface them in a settings panel.
- [ ] **Stable log levels.** Either gateway/talker emit a per-entry level field on `/api/logs/stream` (preferred), or document a more reliable line-format convention so `parseLevel()` can promote from `partial` to `wired`.
- [ ] **Export.** Turn list as JSON / markdown download. Same affordance for logs (Save .log button).
- [ ] **Token-level transcript deltas.** Sub-sentence streaming would let the inspector show inter-token latency and the timeline strip show finer-grained Talker activity. Requires `transcript_delta` egress.
- [ ] **LogsPanel UX — resizable panel + buffer-cap control.** Drag handle on the top edge of the logs region with localStorage-persisted height (replaces today's fixed 240px expanded state). Settings affordance to bump `config.logBufferCap` at runtime — today it's a 5000-entry constant; long sessions silently drop older entries even though the counter is honest about the buffer-relative count.
- [ ] **Talker structured logs visible in LogsPanel.** Symptom on the console side: the `talker` source filter shows almost nothing — only bare `print()` and the recording spinner survive. Root cause is in `talker/main.py`: `RealtimeSTT` / `RealtimeTTS` import side effects clobber the root logger's `basicConfig`. Fix is `logging.basicConfig(force=True, ...)` *after* those heavy imports inside `main()`. Backend change, console-visible benefit.
- [ ] **Opus encoding on the audio path.** Today the mic upload is raw int16 PCM at 16 kHz over the WS (~256 kbps). Opus-encoding in `audio/capture.ts`'s worklet (with a matching decoder in `talker/voice/listener.py` — codec already gated by `audio_input_codec`) drops bandwidth ~10×. Symmetric work on the TTS download once [#36](https://github.com/styin/kaguya/issues/36) lands the audio path: encode in `talker/voice/speaker.py`, decode in `audio/playback.ts`'s worklet.
- [ ] **Inspector debug surfaces + `/ws/debug` channel.** Gateway adds a Cargo-feature-gated `/ws/debug` egress carrying instrumentation events — Input Stream P0–P5 dispatches, raw `TalkerContext` per turn, raw `TalkerOutput` payload variants. Console adds a Debug tab in the right inspector that subscribes to that channel and renders structured viewers. Combined entry because the wire side and the UI side ship together.

## Tree

```
console/
├── index.html                — Google Fonts preconnect, root div
├── package.json              — Vite + React 19 + TypeScript
├── vite.config.ts            — proxies /ws, /health; mounts supervisor plugin; injects __APP_VERSION__
├── tsconfig.json
├── supervisor.json           — managed/unmanaged process definitions
├── server/
│   ├── plugin.ts             — Vite plugin: /api/process/*, /api/logs/stream
│   └── supervisor.ts         — child_process spawn/kill/restart, log ring buffer
├── src/
│   ├── main.tsx
│   ├── App.tsx               — region orchestration + WS / audio lifecycles
│   ├── App.css               — grid layout
│   ├── tokens.css            — design tokens + keyframes
│   ├── types.ts              — WS message types, WsEvent envelope, Turn projection, supervisor types
│   ├── config.ts             — runtime config (reconnect delays, buffer caps)
│   ├── ws.ts                 — WebSocket client (status + JSON + binary)
│   ├── store.ts              — event log + selectors + actions
│   ├── audio/
│   │   ├── capture.ts        — getUserMedia → worklet → onChunk; AnalyserNode tap
│   │   ├── playback.ts       — WS binary → worklet → speakers; AnalyserNode tap
│   │   ├── worklet.ts        — float32↔int16 conversion (capture + playback)
│   │   ├── refs.ts           — module-level AnalyserNode refs for the meters
│   │   └── meter.ts          — RAF-driven imperative bar updater
│   └── regions/
│       ├── TopBar/{TopBar.tsx, topbar.css}
│       ├── LeftRail/{LeftRail.tsx, leftrail.css, AudioStrip.tsx, ProcessCard.tsx}
│       ├── TurnsPanel/{TurnsPanel.tsx, turnspanel.css, TimelineStrip.tsx, TurnRow.tsx}
│       ├── Inspector/{Inspector.tsx, inspector.css, LiveMonitor.tsx, TurnDetail.tsx}
│       └── LogsPanel/{LogsPanel.tsx, logspanel.css, PromptBar.tsx, LogRow.tsx}
```

Each region is a self-contained directory — adapting to a new event type or a library update touches one folder.
