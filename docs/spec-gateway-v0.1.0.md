# spec-gateway-v0.1.0.md

# Project Kaguya — Gateway Specification

**Component:** Gateway (formerly "Router")
**Version:** 0.1.0
**Date:** March 2026
**Audience:** Developers working on the Gateway module

---

## 1. Role and Mandate

The Gateway is the central conductor of Project Kaguya. It is the only component that speaks OpenPod's protobuf protocol. It owns the Input Stream priority queue, all conversation state, the memory store interface, persona delivery, process lifecycle, and all IPC coordination between every other internal component.

The Gateway does **not**:

- Run LLM inference for conversational responses. (It may call llama.cpp for background history compaction — a simple summarization task, not conversational.)
- Inspect or decode audio content. It forwards audio bytes between OpenPod and the Listener/Talker without reading them.
- Classify queries or decide whether to delegate. That is the Talker's exclusive decision.
- Format LLM prompts. It assembles a structured context package; the Talker formats it into the model's expected prompt structure.

---

## 2. Responsibilities (Exhaustive)

1. **Endpoint I/O.** Receive audio frames, text commands, and control signals from the local endpoint; forward Kaguya's audio and metadata output back to it. **Phase 1:** The endpoint is a local dev-GUI/TUI connected via a simple local interface (e.g. WebSocket or stdio). The Gateway demuxes incoming frames into the appropriate Internal paths (audio → Listener, text → Input Stream P1, control → direct handling) and muxes outgoing audio and metadata back to the dev-GUI/TUI. **Phase 2:** The endpoint interface is replaced by the OpenPod protobuf protocol. The Gateway becomes the sole component speaking OpenPod, with Channel A (metadata) and Channel D (audio) muxed into OpenPod's wire format. The dev-GUI/TUI is retired or retained as a local debug tool.
2. **Conversation history management.** Maintain the rolling conversation log (in-memory). Append new turns (user input + Talker response) after each exchange. Perform context window compaction: keep recent N turns in full, summarize older turns via background LLM call, discard beyond the summary horizon. Single continuous thread per user — no session management. This is distinct from `MEMORY.md` — history is the short-term rolling log; `MEMORY.md` is the distilled long-term knowledge base.
3. **Context package assembly.** Before every Talker dispatch, assemble a structured context package containing: user input, `MEMORY.md` contents (cached in memory), conversation history, active task state, current tool list, any tool/reasoner results, and metadata (current time, etc.). The Talker formats this into a prompt; the Gateway has no knowledge of prompt format.
4. **Memory file management.** Read `MEMORY.md` from disk at startup. Include its contents in every context package. Watch for file changes (user manual edits) and re-read on update. After each exchange, evaluate whether anything memory-worthy occurred; if yes, append new facts to `MEMORY.md` on disk and send the updated config to the Talker via `UpdatePersona`. The Talker has no filesystem access — the Gateway is the sole writer and reader of all persistent memory.
5. **Persona file delivery.** Read `SOUL.md`, `IDENTITY.md`, and `MEMORY.md` at startup; bundle and send to Talker via gRPC `UpdatePersona`. Watch for file changes on all three and re-send on any update. The Talker has no filesystem access — the Gateway is the sole source of all persona and memory configuration.
6. **Privilege management.** Enforce access control on tool invocations and agent spin-ups.
7. **Poll Input Stream.** Continuously consume events from the priority queue.
8. **Tool registry and dispatch.** Maintain the Toolkit registry. Dispatch tool calls received from the Talker. Return results as Input Stream events (P3, non-blocking). Manage MCP server connections and expose available MCP tools through the Tool Registry.
9. **Control signal interception.** Process `STOP`, `APPROVAL`, `SHUTDOWN` from the endpoint's control path. These bypass the Input Stream entirely — no event may delay a STOP.
10. **Thread/process management.** Spawn, monitor, and terminate the Listener, Talker, and Reasoner processes.
11. **Sandbox management.** Ensure all tool executions run in appropriate isolation (sandboxed TypeScript processes). Enforce the workspace root: all Toolkit file paths are resolved relative to the configured workspace root directory. The workspace root is set at startup and can be reconfigured via control signals.
12. **Reasoner lifecycle.** Start Reasoner Agents when the Talker requests delegation via `[DELEGATE:...]`. Manage multiple concurrent Reasoner Agents, each with a unique `task_id`. Monitor lifecycle. Adapt Reasoner output into Input Stream events (P3).
13. **Reasoner output filtering.** Decide which intermediate Reasoner steps are worth forwarding to the Talker for narration — dropping noise, rate-limiting, merging. One utterance per meaningful state transition; rate-limited to prevent manic narration.
14. **Timing management.** Silence timers, scheduled reminders, memory triggers. Emit timed events (P4) into the Input Stream.
15. **Speculative prefill trigger.** After each Talker response completes, send the updated context package to the Talker marked as prefill-only (`n_predict: 0`, `cache_prompt: true`). On partial transcripts (Phase 2), send incremental prefill requests.
16. **Output Stream mux.** Stop forwarding Talker audio to the endpoint on receipt of `PREPARE` acknowledgment. Resume forwarding when a new inference round begins.

---

## 3. The Input Stream

The Input Stream is a unified priority queue internal to the Gateway process. It aggregates all events from all sources into a single ordered stream for the Gateway's event loop to consume.

### 3.1 Priority Levels

| Priority | Level                | Event Types                                                                      | Handling                                                                                                                                                                            |
| -------- | -------------------- | -------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| P0       | Control              | `STOP`, `APPROVAL`, `SHUTDOWN`                                                   | Bypass Input Stream entirely. Direct to Gateway event loop. No event may delay a STOP.                                                                                              |
| P1       | Complete user intent | `final_transcript`, `text_command`                                               | Trigger context package assembly + Talker dispatch. Highest normal priority — shortest acceptable latency, triggers barge-in, can invalidate lower-priority work.                   |
| P2       | Partial user signals | `partial_transcript`, `vad_speech_start`, `vad_speech_end`                       | `vad_speech_start` triggers PREPARE signal to Talker immediately. Partials feed speculative prefill (Phase 2). If P1 and P2 arrive simultaneously, P1 wins.                         |
| P3       | Async results        | `reasoner_intermediate_step`, `reasoner_output`, `tool_result`, `reasoner_error` | Results from work Kaguya initiated. Trigger new inference rounds. Never preempt the user — if a tool result arrives while user is mid-sentence (P1/P2 in queue), tool result waits. |
| P4       | Timed                | `silence_exceeded(duration)`, `scheduled_reminder`, `memory_trigger`             | Self-generated. Speculative — moot if user is speaking (P1/P2 in queue) or a tool result is pending (P3). Only act when queue is otherwise quiet.                                   |
| P5       | Ambient              | `openpod_telemetry`, `screen_context_change`, MCP triggers                       | Background context. Process when truly idle. Never trigger immediate action.                                                                                                        |

Within a priority level, events are FIFO. Across levels, higher priority preempts lower.

### 3.2 Priority Rationale

**Ordering principle: human intent > human state > Kaguya's work results > Kaguya's proactive impulses > background context.**

- **P0** is existential. `STOP` must take effect even if the queue is full.
- **P1** represents complete user intent. It has the shortest acceptable latency (the user is waiting), triggers barge-in (anything Kaguya is doing becomes secondary), and can invalidate lower-priority work.
- **P2** optimizes latency but triggers no visible actions alone. `vad_speech_start` enables preemptive barge-in detection before the full transcript arrives. Partials feed speculative prefill — every millisecond matters.
- **P3** results are from work Kaguya initiated. Important, but they never preempt the user.
- **P4** timers are speculative — "maybe I should say something." A silence timer is moot if the user is speaking.
- **P5** enriches passive awareness but never triggers immediate action.

### 3.3 Implementation

- Language: Rust (part of the Gateway binary).
- Data structure: Per-level `tokio::sync::mpsc` channels, polled via `tokio::select!` with priority ordering.
- Event format: Rust enum/struct internally; protobuf for cross-process boundaries.

---

## 4. Memory System

### 4.1 Phase 1: File-Based Memory (MEMORY.md)

Phase 1 uses a plain-text file (`MEMORY.md`) managed entirely by the Gateway. ChromaDB is deferred to Phase 2.

**Rationale:** ChromaDB is a full vector database — a separate Docker container, an embedding model dependency, an HTTP service process, and network latency on every query. Kaguya's Phase 1 memory needs are small: a few dozen project facts, a user profile, and recent conversational context. At that scale, vector similarity search adds complexity with no benefit. A file read takes ~1ms and gives the LLM the full memory context on every turn — including connections that similarity search might miss. It is also directly human-readable and editable.

**Structure:** A structured markdown file managed by the Gateway. Example:

```markdown
## User Profile

- Name: Sebastian
- Working on: Goedel pipeline, Kaguya, OpenPod
- Prefers: concise responses, technical depth

## Project Context

- Goedel pipeline: runs nightly at 2am, owned by infra team
- OpenPod: Rust-based transport layer, protobuf protocol
- Kaguya: this project, voice-first AI Chief of Staff

## Recent Context

- [2026-03-09] Discussed Phase 1 architecture, finalized PREPARE signal model
- [2026-03-08] Evaluated Airi project, adopted soul container pattern
- [2026-03-07] Goedel pipeline had a config issue, resolved by infra team
```

**Memory ownership:** The Gateway is the sole owner of `MEMORY.md` — both reads and writes. The Talker never touches the filesystem.

### 4.2 Dual Memory Structure

| Memory Type                           | Contents                                                                      | Phase 1 Implementation                                                                                        |
| ------------------------------------- | ----------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| **Long-term (MEMORY.md)**             | User profile, project facts, architectural rules, distilled long-term context | File read at startup; injected into every context package; appended post-turn when memory-worthy events occur |
| **Short-term (conversation history)** | Recent turns, rolling log, compacted older turns                              | In-memory state in Gateway; included in context package                                                       |

### 4.3 Memory Hydration Flow (Phase 1)

```
Startup:
  Gateway reads SOUL.md, IDENTITY.md, MEMORY.md from disk
  Gateway bundles all three → sends to Talker via UpdatePersona gRPC

Every turn:
  final_transcript arrives → Gateway assembles context package:
    → MEMORY.md contents (already in memory, ~1ms file-read at startup)
    → Conversation history (in-memory)
    → User input, tools, metadata
  → Gateway forwards context package to Talker
  → Talker formats prompt with memory already included

User manually edits MEMORY.md:
  Gateway file watcher detects change
  Gateway re-reads MEMORY.md
  Gateway sends updated config to Talker via UpdatePersona gRPC
```

### 4.4 Memory Indexing (Post-Turn)

```
Talker completes response → signals Gateway "response complete"
  → Gateway appends new turn to conversation history (in-memory):
      { user: "Can you check the Goedel pipeline status",
        assistant: "The pipeline is healthy — last run 2h ago." }
  → Gateway evaluates: did anything memory-worthy happen?
      (new project fact, preference expressed, significant event)
    If yes → Gateway appends to MEMORY.md on disk
             → Gateway sends updated config to Talker via UpdatePersona gRPC
  → Gateway performs history compaction if needed (background LLM call)
  → Gateway sends updated context package to Talker for prefix prefill
```

### 4.5 Phase 2: ChromaDB Vector Store

When `MEMORY.md` grows to the point where its full content consumes too many context window tokens, episodic memory migrates to ChromaDB:

- **ChromaDB** handles episodic memory (past conversations, preferences, emotional context) via vector similarity search.
- **MEMORY.md** is retained for semantic facts (user profile, project knowledge) — this stays small and fits in context indefinitely.
- The Gateway gains an async HTTP client (`reqwest`) to ChromaDB.
- RAG queries fire on `final_transcript`; pre-emptive queries fire on `partial_transcript`.
- ChromaDB target: <100K entries, <50-100ms query latency under voice pipeline load.

---

## 5. Turn Lifecycle (Unified PREPARE Signal)

Every turn begins the same way, regardless of whether the Talker is currently speaking or idle. There is no special "barge-in" mode.

### 5.1 Voice Input Flow

```
EVERY turn, regardless of whether Talker is speaking or idle:

  t=0ms    Listener: VAD speech onset → vad_speech_start [P2] → Input Stream
  t=1ms    Gateway:  Processes vad_speech_start
                     → Sends PREPARE signal to Talker (gRPC, fire-and-forget)
                     → Cancels active silence timers

  t=1ms    Talker receives PREPARE:
                     IF speaking:
                       → Stop TTS playback (mid-word if necessary)
                       → Cancel in-flight LLM generation
                       → Send partial_response metadata to Gateway:
                         { spoken: "Got it. The pipeline is health",
                           unspoken: "y — last run was two hours ago." }
                       → Now idle, waiting for next context package
                     IF idle:
                       → No-op (Talker already ready from prefix prefill)

  t=1ms    Gateway:  IF received partial_response from Talker:
                       → Append truncated response (spoken portion only) to history
                       → Discard unspoken text
                     → Stops muxing Talker audio to OpenPod Channel D

  t=50ms+  Listener: partial_transcript events [P2] → Input Stream
  t=50ms+  Gateway:  Forwards partials to Talker for incremental KV prefill (Phase 2)

  t=500ms+ Listener: final_transcript [P1] → Input Stream
  t=501ms  Gateway:  Processes final_transcript
                     → Assembles context package (MEMORY.md contents already
                       cached in memory, history, tools, input — no query needed)
                     → Dispatches context package to Talker

  t=502ms  Talker:   Formats prompt → LLM → soul container → TTS → Channel D
                     metadata → gRPC → Gateway → Output Stream → Channel A
                     [TOOL:...] → gRPC → Gateway → tool dispatch
                     [DELEGATE:...] → gRPC → Gateway → Reasoner spin-up
                     → On completion: signals Gateway → prefix prefill → silence timer starts
```

### 5.2 Text Input Flow

```
Text input (no voice):

  t=0ms    OpenPod: text_command → demux → Input Stream [P1]
  t=1ms    Gateway: Processes text_command
                    → Sends PREPARE to Talker (cancel if busy)
                    → Fires RAG query
                    → Assembles context package
                    → Dispatches to Talker
  t=2ms    Talker:  Same flow as voice — format, infer, post-process, TTS (or text-only)
```

The only asymmetry: voice has a `vad_speech_start` → delay → `final_transcript` gap (prefill opportunity). Text arrives as a complete input instantly — PREPARE and context package are dispatched in quick succession.

### 5.3 False Positive VAD (Phase 1)

If VAD fires on a cough or background noise, the Gateway sends PREPARE and the Talker stops (if speaking), but no `final_transcript` ever arrives. The silence timer fires after 3 seconds and Kaguya resumes or rephrases. False interruptions are less bad than missed interruptions.

Phase 2 refinement: two-stage PREPARE (soft fade on `vad_speech_start`, hard stop on first `partial_transcript`).

---

## 6. Delegation Flow

```
1. final_transcript arrives in Input Stream                      [P1]
2. Gateway assembles context package (memory, history, tasks, tools)
3. Gateway forwards context package to Talker
4. Talker formats prompt, runs LLM inference
5a. Talker determines: "I can handle this"
    → generates response → soul container → TTS → Channel D
    → transcript + tags → gRPC → Gateway → Output Stream → Channel A
5b. OR Talker determines: "This needs deeper work"
    → generates acknowledgment → TTS → Channel D
    → emits [DELEGATE:task_description] → gRPC → Gateway
6. Gateway starts Reasoner Agent with task (unique task_id)
7. Reasoner output → Input Stream events                         [P3]
8. Gateway filters, forwards significant steps to Talker → narration
9. Reasoner completes → Input Stream → Gateway → Talker → summary
```

---

## 7. Tool Dispatch Flow (Non-Blocking)

```
Round 1:
  Talker LLM: "Let me check that for you. [TOOL:web_fetch(...)]" → stops
  Soul container:
    → "Let me check that for you." → TTS → Channel D (user hears immediately)
    → TOOL request → gRPC → Gateway

  Gateway dispatches to Toolkit (TypeScript, sandboxed)
  [Tool executes asynchronously, ~200-2000ms]

  Tool result → Input Stream event [P3]
  Gateway assembles new context package with tool result included
  Gateway forwards to Talker for new inference round

Round 2:
  Talker LLM: "The pipeline is healthy — last run was two hours ago." → TTS → Channel D
```

No blocking. Each LLM invocation is a complete, independent round. Tool results are async events, same as Reasoner results.

---

## 8. Silence Timer Management

- After Talker signals response complete, Gateway starts silence timer.
- `silence_exceeded(3s)`: Soft prompt opportunity → forward to Talker as P4 event.
- `silence_exceeded(8s)`: Follow-up opportunity → forward to Talker as P4 event.
- `silence_exceeded(30s)`: Context shift → forward to Talker as P4 event.
- All timers canceled on `vad_speech_start` or `text_command`.

---

## 9. Deliberative Narration Protocol

The Gateway orchestrates the three phases of Deliberative Narration during slow-path Reasoner work.

**Phase 1 — Immediate Acknowledgment (500-900ms):** Gateway dispatches first inference round to Talker immediately upon delegation decision. Talker generates hedged response ("Let me check on that.") and sends to TTS — user hears something within the normal response window.

**Phase 2 — Narration (every 3-8s during Reasoner work):** Gateway filters Reasoner intermediate steps and forwards significant state transitions to Talker. Cadence: one utterance per meaningful state transition, not on a fixed timer. Rate-limited by Gateway to prevent manic narration.

**Phase 3 — Resolution (on Reasoner completion):** Reasoner completion event arrives at Input Stream [P3]. Gateway assembles final context package and dispatches to Talker for summary generation.

---

## 10. Speculative Prefill Orchestration

The Gateway triggers two phases of KV cache prefill.

**Phase 1 — Always-On Prefix Prefill.** Immediately after the Talker finishes generating a response for turn N, the Gateway sends an updated context package to the Talker marked as prefill-only (`n_predict: 0`, `cache_prompt: true`). The GPU is idle between turns — this costs nothing. Cache invalidation: if memory context changes between turns (e.g., a Reasoner completes and the Gateway appends new facts to `MEMORY.md` and sends an `UpdatePersona`), the Gateway signals the Talker to re-trigger prefix prefill with the updated context.

**Phase 2 — Partial Transcript Prefill.** The Gateway forwards each `partial_transcript` event to the Talker as an incremental prefill request, extending the cached prefix word-by-word during user speech.

---

## 11. Endpoint Ingress/Egress Routing

### Phase 1 (dev-GUI/TUI)

```
Ingress:  dev-GUI/TUI → Gateway (demux) → audio frames forwarded to Listener
                                         → text commands to Input Stream [P1]
                                         → control signals handled directly [P0]

Egress:   Talker audio → Gateway → dev-GUI/TUI → speakers
          Gateway metadata (transcript, emotion tags) → dev-GUI/TUI → display
```

The dev-GUI/TUI is a local development interface — no network transport, no protobuf protocol. It connects to the Gateway via a simple local mechanism (e.g. WebSocket or stdio). Its purpose is to make the full voice pipeline testable before OpenPod is ready.

### Phase 2 (OpenPod)

```
Ingress:  OpenPod → Gateway (demux) → audio frames forwarded to Listener
                                     → text commands to Input Stream [P1]
                                     → telemetry to Input Stream [P5]
                                     → control signals handled directly [P0]

Egress:   Talker audio → Gateway (mux into OpenPod protocol) → OpenPod → Channel D
          Gateway metadata → Gateway (mux into OpenPod protocol) → OpenPod → Channel A
```

The Gateway becomes the sole component speaking OpenPod's protobuf protocol. It moves audio bytes without inspecting their content.

---

## 12. Output Stream

Kaguya's output flows through two parallel paths back to the endpoint.

**Audio:** Synthesized speech from the Talker's TTS, Opus-encoded. Forwarded by the Gateway to the endpoint without inspection. Phase 1: streamed to the dev-GUI/TUI for local playback. Phase 2: muxed into OpenPod Channel D.

**Metadata:** Text transcript, emotion tags (`[EMOTION:...]`), task status updates, typing indicators, presence signals. Flows from Talker post-process → gRPC → Gateway → endpoint display. Phase 1: sent to the dev-GUI/TUI for rendering. Phase 2: muxed into OpenPod Channel A.

Audio and metadata do not require frame-level synchronization at the endpoint. Emotion tags may slightly lead audio (expression before speech), which is intentional.

On `PREPARE` signal: the Gateway stops forwarding Talker audio to the endpoint. The Talker handles TTS/LLM cancellation internally.

---

## 13. Talker Egress — What the Gateway Receives from the Talker

```
Talker post-process produces (all arrive at Gateway via gRPC):
  → Transcript text       → Output Stream → endpoint display
  → Emotion tags          → Output Stream → endpoint display
  → [TOOL:...] request    → tool dispatch
  → [DELEGATE:...] req    → Reasoner spin-up
  → Response complete     → triggers prefix prefill, history append, silence timer start
  → partial_response      → on PREPARE (if Talker was speaking):
                             { spoken: "Got it. The pipeline is health",
                               unspoken: "y — last run was two hours ago." }
                             Gateway appends only the spoken portion to history
                             and discards the unspoken text
```

Spoken audio itself stays in the Talker → TTS → endpoint directly (forwarded by Gateway without inspection).

---

## 14. Workspace Management

The Gateway enforces the workspace root and manages all Toolkit tools.

**Toolkit tools (Phase 1).** The Gateway's Toolkit exposes filesystem and environment tools: `list_files(path)`, `read_file(path)`, `write_file(path, content)`, `search_files(query)`, `run_command(cmd)`. These execute in sandboxed TypeScript processes managed by the Gateway. All file paths are resolved relative to the configured workspace root. The workspace root is set at startup (e.g., the user's current project directory) and can be reconfigured via OpenPod control signals.

**MCP servers (Phase 1).** The Gateway manages MCP server connections and exposes available MCP tools through the Tool Registry. The LLM discovers and calls MCP tools via `[TOOL:search_tools(...)]` → `[TOOL:mcp_tool_name(...)]`.

**Reasoner agent workspace access (Phase 1).** When Kaguya delegates to OpenClaw or Claude Code, the Reasoner Agent has its own workspace access model — potentially broader than the Talker's direct tools. The Reasoner Adapter manages this scope. Kaguya delegates the task and the Reasoner reports results back through the Input Stream.

**User host ambient telemetry (Phase 2).** OpenPod's telemetry channel carries ambient activity from the user's host: current working directory, active processes, stdout/stderr from running builds, screen context. These arrive as P5 events in the Input Stream. Phase 2 because it requires OpenPod telemetry channel implementation.

---

## 15. IPC Protocol

gRPC with Protocol Buffers over Unix domain sockets. Schema enforcement catches cross-language breakage at compile time.

```protobuf
service ListenerService {
  rpc StreamEvents(stream ListenerEvent) returns (ListenerAck);
}

service TalkerService {
  // Gateway dispatches context package for LLM inference
  rpc ProcessPrompt(TalkerContext) returns (stream TalkerOutput);
  // Gateway signals new turn (cancel if busy, warm up if idle)
  // PrepareAck carries partial_response when Talker was mid-speech:
  //   { spoken_text: string, unspoken_text: string }
  // Both fields are empty if Talker was already idle.
  rpc Prepare(PrepareSignal) returns (PrepareAck);
  // Gateway sends prefill-only context (no generation)
  rpc PrefillCache(PrefillRequest) returns (PrefillAck);
  // Gateway delivers persona config at startup and on change
  rpc UpdatePersona(PersonaConfig) returns (PersonaAck);
}

service ReasonerService {
  rpc ExecuteTask(TaskRequest) returns (stream ReasonerEvent);
  rpc CancelTask(CancelRequest) returns (CancelAck);
}

service RouterControl {
  rpc SendControl(ControlSignal) returns (ControlAck);
}
```

Audio frames use a dedicated low-overhead path (raw bytes over Unix socket or minimal-wrapping gRPC stream) to avoid protobuf overhead at 50fps.

---

## 16. Implementation

| Attribute                                  | Value                                                                                                                  |
| ------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------- |
| Language                                   | Rust                                                                                                                   |
| Async runtime                              | `tokio` single-threaded async                                                                                          |
| gRPC server                                | `tonic`                                                                                                                |
| File I/O (MEMORY.md, SOUL.md, IDENTITY.md) | `tokio::fs` + file watcher                                                                                             |
| HTTP client (ChromaDB — Phase 2)           | `reqwest` (async)                                                                                                      |
| IPC transport                              | Unix domain sockets                                                                                                    |
| OpenPod connection                         | TCP/local socket per OpenPod spec                                                                                      |
| State                                      | In-memory state machine: conversation history, active tasks, pending timers, tool list, cached persona + memory config |

### 16.1 Process Layout

```
┌───────────────────────────────────────────────────────────────┐
│  Process 1: Gateway (Rust)                                     │
│  - tokio async runtime                                         │
│  - Input Stream (priority queue)                               │
│  - gRPC server (tonic)                                         │
│  - Silence timers, prefill orchestration, Reasoner lifecycle   │
└────────────────────────────────┬──────────────────────────────┘
          gRPC (Unix socket)     │     gRPC (Unix socket)
          ┌──────────────────────┘     └──────────────────────┐
          ▼                                                    ▼
  Process 2: Listener + Talker (Python)          Process 4+: Reasoner(s) (TypeScript)
```

---

## 17. Phased Delivery

### Phase 1 Deliverables (Gateway scope)

- Event loop, priority queue, silence timers.
- **Local endpoint I/O via dev-GUI/TUI** (audio, text, control — simple local interface, no OpenPod protocol).
- Context package assembly and Talker dispatch.
- File-based memory: read `MEMORY.md`, `SOUL.md`, `IDENTITY.md` at startup; bundle and deliver to Talker via `UpdatePersona`. File watcher for all three; re-send on change.
- Post-turn memory evaluation: append memory-worthy facts to `MEMORY.md`; send updated config to Talker.
- Conversation history management (in-memory rolling log, compaction).
- Persona file delivery (`SOUL.md` + `IDENTITY.md` + `MEMORY.md`) to Talker at startup and on change.
- Tool dispatch (Toolkit registry, sandboxed TypeScript).
- Reasoner lifecycle management (spawn, monitor, cancel).
- Reasoner output filtering and narration dispatch.
- PREPARE signal dispatch on `vad_speech_start` and `text_command`.
- Always-on prefix prefill trigger (post-turn).
- Silence timer management.
- Workspace root enforcement.
- MCP server connections.

### Phase 2 Deliverables (Gateway scope)

- **OpenPod protocol integration** — replace dev-GUI/TUI local interface with OpenPod demux/mux (Channel A metadata, Channel D audio, Control channel, telemetry P5 events).
- ChromaDB vector store for episodic memory (when `MEMORY.md` exceeds context window budget).
- Pre-emptive RAG on `partial_transcript` events (ChromaDB queries during user speech).
- Partial transcript prefill forwarding to Talker.
- Two-stage PREPARE signal (soft fade / hard stop).
- Tool Search Tool for large MCP registries.
- Programmatic Tool Calling (multi-tool scripts in sandboxed environment).
- User host ambient telemetry ingestion from OpenPod (P5 events).

---

## 18. Open Questions (Gateway-Relevant)

- **MEMORY.md growth threshold.** At what entry count does `MEMORY.md` consume enough context window tokens to justify migrating to ChromaDB? Needs empirical measurement against Qwen3-8B's context window and prompt structure.
- **Post-turn memory evaluation heuristic.** What criteria does the Gateway use to decide if something is "memory-worthy"? Rule-based (new project name, preference expressed) vs. a lightweight LLM classification call. Needs design.
- **OpenPod audio integration.** Opus frame handling within OpenPod's existing Raw channel needs implementation. Verify that protobuf wrapping overhead at 50fps (~20ms per frame) is negligible compared to frame processing time.
- **Speculative prefill invalidation.** Strategy when user's meaning reverses at end of utterance. How to efficiently discard/rebuild KV cache state signaled to Talker.
- **GPU contention profiling.** VRAM budget fits on paper. Actual compute contention needs empirical benchmarking (faster-whisper + llama.cpp + Kokoro concurrent on RTX 5070 Ti).
