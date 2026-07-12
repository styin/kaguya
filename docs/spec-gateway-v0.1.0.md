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
2. **Conversation history management.** Maintain the rolling conversation log (in-memory). Append new turns (user input + Talker response) after each exchange. Perform context window compaction: keep recent N turns in full, summarize older turns via background LLM call, discard beyond the summary horizon. Single continuous thread per user — no session management. This is distinct from the RAG memory store — history is the short-term rolling log; the RAG store is the distilled long-term knowledge base.
3. **Context package assembly.** Before every Talker dispatch, assemble a structured context package containing: user input, the synthesized `memory_md` exported from the RAG store, per-turn `retrieval_results` from the hybrid retriever, conversation history, active task state, current tool list, any tool/reasoner results, and metadata (current time, etc.). The Talker formats this into a prompt; the Gateway has no knowledge of prompt format.
4. **RAG memory management.** Own a SQLite database (`data/kaguya.db` by default) holding semantic memories with FTS5 BM25 indexes and optional vector embeddings. Per turn, run `RagEngine::retrieve(query)` to get top-k entries (BM25 + vector fused via RRF — see REF-007/008/009). Post-turn, run `evaluate_and_store(user_input, assistant_response)` to extract preference / fact / project / conversation memories from the exchange. The synthesized `memory_md` (user profile + project context + recent semantic memories) is recomputed and pushed to the Talker via `UpdatePersona` only when its content actually changes. The Talker has no filesystem access — the Gateway is the sole owner of the RAG store.
5. **Persona file delivery.** Read `SOUL.md` and `IDENTITY.md` at startup; bundle them with the synthesized `memory_md` from the RAG store and send to Talker via gRPC `UpdatePersona`. Watch `SOUL.md` and `IDENTITY.md` for file changes and re-send on update; `memory_md` is re-pushed when post-turn evaluation changes it. The Talker has no filesystem access — the Gateway is the sole source of all persona and memory configuration.
6. **Privilege management.** Enforce access control on tool invocations and agent spin-ups.
7. **Poll Input Stream.** Continuously consume events from the priority queue.
8. **Tool registry and dispatch.** Maintain the Toolkit registry. Dispatch tool calls received from the Talker. Return results as Input Stream events (P3, non-blocking). Manage MCP server connections and expose available MCP tools through the Tool Registry.
9. **Control signal interception.** Process `STOP`, `APPROVAL`, `SHUTDOWN` from the endpoint's control path. These bypass the Input Stream entirely — no event may delay a STOP.
10. **Thread/process management.** Spawn, monitor, and terminate the Listener, Talker, and Reasoner processes.
11. **Sandbox management.** Ensure all tool executions run in appropriate isolation (sandboxed TypeScript processes). Enforce the workspace root: all Toolkit file paths are resolved relative to the configured workspace root directory. The workspace root is set at startup and can be reconfigured via control signals.
12. **Reasoner lifecycle.** Start Reasoner Agents when the Talker requests delegation via `[DELEGATE:...]`. Manage multiple concurrent Reasoner Agents, each with a unique `task_id`. Monitor lifecycle. Adapt Reasoner output into Input Stream events (P3).
13. **Reasoner task projection.** Fold structured activity and plan updates into `TaskState`; inject its digest into Talker context. Only approval, completion, and error events enter P3.
14. **Timing management.** Silence timers, scheduled reminders, memory triggers. Emit timed events (P4) into the Input Stream.
15. **Speculative prefill trigger.** After each Talker response completes, send the updated context package to the Talker marked as prefill-only (`n_predict: 0`, `cache_prompt: true`). On partial transcripts (Phase 2), send incremental prefill requests.
16. **Output Stream mux.** Stop forwarding Talker audio to the endpoint when `BargeInAck` confirms interruption. Resume forwarding when a new inference round begins.

---

## 3. The Input Stream

The Input Stream is a unified priority queue internal to the Gateway process. It aggregates all events from all sources into a single ordered stream for the Gateway's event loop to consume.

### 3.1 Priority Levels

| Priority | Level                | Event Types                                                                      | Handling                                                                                                                                                                            |
| -------- | -------------------- | -------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| P0       | Control              | `STOP`, `APPROVAL`, `SHUTDOWN`                                                   | Bypass Input Stream entirely. Direct to Gateway event loop. No event may delay a STOP.                                                                                              |
| P1       | Complete user intent | `final_transcript`, `text_command`                                               | Trigger context package assembly + Talker dispatch. Highest normal priority — shortest acceptable latency, triggers barge-in, can invalidate lower-priority work.                   |
| P2       | Partial user signals | `partial_transcript`, `vad_speech_start`, `vad_speech_end`                       | `vad_speech_start` triggers an inline barge-in (`TalkerInput.barge_in`) on the active Converse stream. Partials feed speculative prefill (Phase 2). If P1 and P2 arrive simultaneously, P1 wins. |
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

### 4.1 Hybrid RAG (SQLite + FTS5 BM25 + optional vector)

Kaguya's memory layer is implemented in [gateway/src/rag/](../gateway/src/rag) as `RagEngine`. It is a hybrid retrieval system, not a flat file.

**Storage:** A single SQLite database (default `data/kaguya.db`, configurable via `[rag] db_path`). Schema:
- `memories` — id, content, memory_type (`conversation` | `fact` | `preference` | `project`), source (turn id), timestamps
- `memories_fts` — FTS5 virtual table mirroring `memories.content` for BM25 search; tokenizer `porter unicode61` (REF-009)
- `embeddings` — optional vector blobs (one per memory) populated by an incremental background embedder
- `user_profile`, `projects` — keyed structured tables used to synthesize the `memory_md` document

**Retrieval (per turn):** `RagEngine::retrieve(query)` runs:
1. BM25 against `memories_fts` (always available)
2. Cosine similarity over `embeddings` (only if an embedder is configured)
3. Reciprocal Rank Fusion (RRF, k=60 — REF-007) merges the two rankings
4. Result truncated to `top_k` (default 10 — REF-008)

The fused list is delivered to the Talker as `TalkerContext.retrieval_results` — a list of `{id, content, source ("bm25" | "vector"), score}`. The Talker prompt formatter renders these in a "Relevant context retrieved from memory" system block.

**Post-turn ingestion:** `RagEngine::evaluate_and_store(user_input, assistant_response, turn_id)` runs simple keyword-trigger extraction (English + Chinese) to classify each exchange as `Preference`, `Fact`, `Project`, or generic `Conversation`, and inserts the entries into `memories`. The optional embedder is woken up via `Notify` and back-fills vectors for new entries asynchronously.

**Synthesized `memory_md`:** `RagEngine::export_memory_md()` renders the structured tables (user profile, projects) and the most recent semantic memories (conversations + facts) as a single markdown document. This is what the Gateway sends to the Talker via `UpdatePersona`.

**Embedder (optional):** [gateway/src/rag/embedder.rs](../gateway/src/rag/embedder.rs) is a long-running task that polls for unembedded rows and POSTs them to a local OpenAI-compatible `/v1/embeddings` endpoint. Configurable via `[rag] embedding_url`. Disabled = BM25-only retrieval.

### 4.2 Dual Memory Structure

| Memory Type                           | Contents                                                                          | Implementation                                                                                                                          |
| ------------------------------------- | --------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| **Long-term (RAG store)**             | User profile, project facts, semantic memories from past turns (conversation/fact/preference/project) | SQLite `memories` table with FTS5 BM25 + optional vector embeddings; queried per turn for top-k; synthesized to `memory_md` for Talker  |
| **Short-term (conversation history)** | Recent turns, rolling log, compacted older turns                                  | In-memory state in Gateway (`History`); included in context package                                                                      |

### 4.3 Memory Hydration Flow

```
Startup:
  Gateway opens data/kaguya.db (creates schema if missing)
  Gateway reads SOUL.md, IDENTITY.md from disk
  Gateway calls RagEngine::export_memory_md() → synthesized memory_md
  Gateway sends {soul, identity, memory_md} → Talker via UpdatePersona gRPC

Every turn:
  final_transcript arrives → Gateway runs RagEngine::retrieve(text)
                          → assembles context package:
    → memory_md (cached from last UpdatePersona)
    → retrieval_results (per-turn RAG hits)
    → Conversation history (in-memory)
    → User input, tools, metadata
  → Gateway forwards context package to Talker (Converse stream)

User edits SOUL.md or IDENTITY.md:
  Gateway file watcher detects change
  Gateway re-reads SOUL.md / IDENTITY.md
  Gateway sends updated config (with current memory_md) to Talker via UpdatePersona
  (memory_md is NOT a watched file — it's synthesized from the RAG store)
```

### 4.4 Memory Indexing (Post-Turn)

```
Talker completes response → ResponseComplete arrives
  → Gateway appends new turn to conversation history (in-memory):
      { user: "Can you check the Goedel pipeline status",
        assistant: "The pipeline is healthy — last run 2h ago." }
  → Gateway calls RagEngine::evaluate_and_store(user, assistant, turn_id):
      keyword-trigger extraction → 0..N MemoryEntry rows inserted
      embedder.wake() → background task back-fills vectors
  → Gateway calls export_memory_md() and compares against last sent.
    If changed → Gateway sends updated PersonaConfig via UpdatePersona
  → Gateway sends updated context package to Talker for prefix prefill
```

### 4.5 Future evolution

Phase-1 trigger-based extraction is intentionally crude. Anticipated upgrades (none of which require breaking the storage format):
- LLM-based extraction (a small local model evaluates "is this exchange memory-worthy?" and produces a structured summary).
- Re-ranking after RRF fusion using a cross-encoder.
- Embedding back-end choices: local server, hosted (Voyage/OpenAI), or on-device.
- Migration of `memories` to a dedicated vector store (sqlite-vss extension or external service) when corpus exceeds ~100k entries.

---

## 5. Turn Lifecycle (Inline Barge-In on Converse Stream)

Every turn begins the same way, regardless of whether the Talker is currently speaking or idle. There is no special "barge-in" mode — barge-in is just a `TalkerInput.barge_in` message on the active Converse bidi stream.

### 5.1 Voice Input Flow

```
EVERY turn, regardless of whether Talker is speaking or idle:

  t=0ms    Listener: VAD speech onset → vad_speech_start [P2] → Input Stream
  t=1ms    Gateway:  Processes vad_speech_start
                     → Sends TalkerInput.barge_in on the active Converse stream
                       (fire-and-forget; no-op if no active stream)
                     → Mutes audio output, cancels active silence timers

  t=1ms    Talker receives BargeInSignal:
                     IF speaking:
                       → Stop TTS playback (mid-word if necessary)
                       → Cancel in-flight LLM generation
                       → Emit BargeInAck on the same stream:
                         { spoken_text: "Got it. The pipeline is health",
                           unspoken_text: "y — last run was two hours ago." }
                       → ResponseComplete { was_interrupted = true } follows
                     IF idle:
                       → No-op (no active Converse stream → barge_in finds None)

  t=1ms    Gateway:  IF BargeInAck received:
                       → Append spoken_text to conversation history
                       → Discard unspoken_text
                     → Stops muxing Talker audio to OpenPod Channel D

  t=50ms+  Listener: partial_transcript events [P2] → Input Stream
  t=50ms+  Gateway:  Forwards partials to Talker for incremental KV prefill (Phase 2)

  t=500ms+ Listener: final_transcript [P1] → Input Stream
  t=501ms  Gateway:  Processes final_transcript
                     → RagEngine::retrieve(text) — BM25 (+ vector) + RRF
                     → Assembles context package (cached memory_md, fresh
                       retrieval_results, history, tools, input)
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
                    → Sends inline `TalkerInput.barge_in` to Talker (cancel if busy)
                    → Fires RAG query
                    → Assembles context package
                    → Dispatches to Talker
  t=2ms    Talker:  Same flow as voice — format, infer, post-process, TTS (or text-only)
```

The only asymmetry: voice has a `vad_speech_start` → delay → `final_transcript` gap (prefill opportunity). Text arrives as a complete input instantly — inline barge-in and context package are dispatched in quick succession.

### 5.3 False Positive VAD (Phase 1)

If VAD fires on a cough or background noise, the Gateway sends inline `barge_in` and the Talker stops (if speaking), but no `final_transcript` ever arrives. The silence timer fires after 3 seconds and Kaguya resumes or rephrases. False interruptions are less bad than missed interruptions.

Phase 2 refinement: tune inline barge-in behavior using `vad_speech_start` and, if warranted, the first `partial_transcript`.

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
8. Gateway folds activity and plan updates into `TaskState`; the Talker may mention the task at its next conversational opening
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

## 10. Speculative Prefill Orchestration

The Gateway triggers two phases of KV cache prefill.

**Phase 1 — Always-On Prefix Prefill.** Immediately after the Talker finishes generating a response for turn N, the Gateway sends an updated context package to the Talker marked as prefill-only (`n_predict: 0`, `cache_prompt: true`). The GPU is idle between turns — this costs nothing. Cache invalidation: if memory context changes between turns (e.g., the post-turn `RagEngine::evaluate_and_store` adds entries that change the synthesized `memory_md`, or a Reasoner completes), the Gateway sends an `UpdatePersona` followed by a fresh `PrefillCache` call.

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

On inline `TalkerInput.barge_in`, the Gateway stops forwarding Talker audio to the endpoint. The Talker handles TTS/LLM cancellation internally and returns `BargeInAck`.

---

## 13. Talker Egress — What the Gateway Receives from the Talker

```
Talker post-process produces (all arrive at Gateway via gRPC):
  → Transcript text       → Output Stream → endpoint display
  → Emotion tags          → Output Stream → endpoint display
  → [TOOL:...] request    → tool dispatch
  → [DELEGATE:...] req    → Reasoner spin-up
  → Response complete     → triggers prefix prefill, history append, silence timer start
  → BargeInAck            → on inline `barge_in` (if Talker was speaking):
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

**User host ambient telemetry (Phase 2).** OpenPod's telemetry channel carries ambient activity from the user's host: current working directory, active processes, stdout/stderr from running builds, screen context. These arrive as P5 events in the Input Stream. Phase 2 because it requires OpenPod telemetry channel implementation.

---

## 14A. Reasoner Workspace Leases and Native Tools **[SIGN-OFF]**

The detailed Reasoner contract is authoritative in `spec-reasoner-v0.1.0.md` §§5, 6, and 9A.

**Host-capability authority.** Gateway has sole discretion over durable Kaguya-agent workspace
and host capabilities. It authorizes the scope of a delegated task and issues an opaque,
task-scoped workspace lease. Supervisor creates and cleans up the volatile task scratch
environment needed for that lease. Neither the Reasoner service nor its backend receives a host
path, container ID, or host credential.

**Native execution.** The Reasoner backend keeps its own file, edit, shell, and test tools.
Those tools run within the authorized workspace; Gateway does not force a CLI to substitute its
tool registry with the Talker Toolkit. CLI-local permission settings are useful defense in
depth; Supervisor enforces the volatile task environment and network policy that Gateway
authorized.

**Kaguya extensions.** Gateway may expose additional, namespaced `kaguya.*` capabilities to a
Reasoner through MCP or an adapter-specific extension mechanism. These are plugins into the
native agent loop, not replacements for it. They carry Kaguya policy and approval semantics
independently of a backend's native tools.

**Progress projection.** Gateway folds activity and plan updates into `TaskState` and puts the
digest in Talker context. Only approvals, completion, and errors enter P3; Gateway does not
select intermediate steps for automatic narration. The Talker decides whether to mention an
active task at its next conversational opening.

**Workspace materialization remains open.** Before implementation, choose how Gateway grants a
lease into the durable Kaguya-agent workspace and how Supervisor provides its volatile scratch
space. Any result export to the durable workspace requires Gateway approval; task scratch has an
explicit cleanup path.

---

## 15. IPC Protocol

gRPC with Protocol Buffers. Audio is the one exception — it rides a raw length-prefixed TCP socket so 50fps Opus frames never enter protobuf serialization (architecture invariant).

```protobuf
// Listener: Gateway = client, Listener = server (role-flipped from earlier draft).
service ListenerService {
  rpc Stream(stream ListenerInput) returns (stream ListenerOutput);
}

// Talker: Gateway = client, Talker = server.
service TalkerService {
  // Bidi: Gateway sends TalkerInput.start (context) to begin generation, then
  // optionally TalkerInput.barge_in to interrupt mid-stream. Talker streams
  // back TalkerOutput including BargeInAck { spoken_text, unspoken_text } when
  // a barge-in interrupts mid-speech. Both fields are empty if Talker was
  // already idle when the BargeIn arrived.
  rpc Converse(stream TalkerInput) returns (stream TalkerOutput);
  // Speculative prefix prefill (no generation).
  rpc PrefillCache(PrefillRequest) returns (PrefillAck);
  // Persona delivery — soul_md + identity_md + RAG-synthesized memory_md.
  rpc UpdatePersona(PersonaConfig) returns (PersonaAck);
}

// Reasoner: Gateway = client, Reasoner = server.
service ReasonerService {
  rpc Delegate(stream DelegateInput) returns (stream DelegateOutput);
  rpc Interrupt(InterruptRequest) returns (InterruptAck);
  rpc Telemetry(TelemetrySubscribe) returns (stream TelemetryEvent);
}

// Endpoint → Gateway control plane.
service RouterControlService {
  rpc SendControl(ControlSignal) returns (ControlAck);
}
```

Audio frames flow on a dedicated raw TCP socket from Gateway to Listener at `listener_audio_addr:listener_audio_port`. The Gateway writes `[u32 BE length][bytes]` records; the Listener decodes Opus and feeds RealtimeSTT.

---

## 16. Implementation

| Attribute                                  | Value                                                                                                                  |
| ------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------- |
| Language                                   | Rust                                                                                                                   |
| Async runtime                              | `tokio` single-threaded async                                                                                          |
| gRPC server                                | `tonic`                                                                                                                |
| File I/O (SOUL.md, IDENTITY.md)            | `tokio::fs` + `notify` file watcher                                                                                    |
| RAG store                                  | `rusqlite` (bundled SQLite) + FTS5 BM25 + optional embedder via `reqwest`                                              |
| IPC transport                              | TCP for Phase 1 (cross-platform); Unix domain sockets re-evaluated when ready                                          |
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
          gRPC (TCP)             │     gRPC (TCP)
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
- RAG memory: open `data/kaguya.db` (SQLite + FTS5) at startup; per-turn retrieval via `RagEngine::retrieve`; synthesize `memory_md` via `RagEngine::export_memory_md`. Optional background embedder for vector similarity.
- Post-turn memory evaluation: `RagEngine::evaluate_and_store` extracts preference / fact / project / conversation entries; updated `memory_md` pushed via `UpdatePersona` only when content changes.
- Conversation history management (in-memory rolling log, compaction).
- Persona file delivery (`SOUL.md` + `IDENTITY.md` + RAG-synthesized `memory_md`) to Talker at startup and on change. File watcher tracks `SOUL.md` and `IDENTITY.md`.
- Tool dispatch (Toolkit registry, sandboxed TypeScript).
- Reasoner lifecycle management (spawn, monitor, cancel).
- Reasoner task-state folding and Talker-discretion task presentation.
- Inline barge-in dispatch (`TalkerInput.barge_in`) on `vad_speech_start` and `text_command`.
- Always-on prefix prefill trigger (post-turn).
- Silence timer management.
- Workspace root enforcement.
- MCP server connections.

### Phase 2 Deliverables (Gateway scope)

- **OpenPod protocol integration** — replace dev-GUI/TUI local interface with OpenPod demux/mux (Channel A metadata, Channel D audio, Control channel, telemetry P5 events).
- LLM-based memory extraction replacing the keyword-trigger heuristic in `RagEngine::extract_memories`.
- Pre-emptive RAG retrieval on `partial_transcript` events (during user speech).
- Partial transcript prefill forwarding to Talker.
- Two-stage barge-in (soft fade / hard stop).
- Tool Search Tool for large MCP registries.
- Programmatic Tool Calling (multi-tool scripts in sandboxed environment).
- User host ambient telemetry ingestion from OpenPod (P5 events).

---

## 18. Open Questions (Gateway-Relevant)

- **RAG store growth threshold.** At what entry count does the SQLite `memories` corpus need a dedicated vector backend (sqlite-vss, external store) to keep query latency under voice-pipeline budgets? Needs empirical measurement.
- **Post-turn memory evaluation heuristic.** Phase 1 uses keyword triggers in `RagEngine::extract_memories`. When and how to swap in a small LLM classifier without blocking the post-turn prefill window?
- **OpenPod audio integration.** Opus frame handling within OpenPod's existing Raw channel needs implementation. Verify that protobuf wrapping overhead at 50fps (~20ms per frame) is negligible compared to frame processing time.
- **Speculative prefill invalidation.** Strategy when user's meaning reverses at end of utterance. How to efficiently discard/rebuild KV cache state signaled to Talker.
- **GPU contention profiling.** VRAM budget fits on paper. Actual compute contention needs empirical benchmarking (faster-whisper + llama.cpp + Kokoro concurrent on RTX 5070 Ti).
