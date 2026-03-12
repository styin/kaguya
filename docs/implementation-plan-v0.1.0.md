# Project Kaguya — Phase 1 Implementation Plan

**Version:** 0.1.0
**Date:** March 2026
**Audience:** Claude agents building Phase 1 locally
**Source specs:** `spec-agent-v0.1.0.md`, `spec-gateway-v0.1.0.md`

---

## 0. How to Use This Document

This plan is the single source of truth for Phase 1 implementation. Each section
is self-contained. Work top to bottom: proto → gateway → talker → reasoner → tools.
Do not skip phases within a section. Notes marked **[DECISION]** record explicit
architectural choices made during design; do not relitigate them. Notes marked
**[OPEN]** are unanswered questions to be resolved empirically during implementation.

---

## 1. Architecture Overview

```
┌──────────────────────────────────────────────────────────────────┐
│  Process 1: Gateway (Rust / tokio / tonic)                        │
│  - Input Stream priority queue (P0–P5)                            │
│  - gRPC server: ListenerService, TalkerService, ReasonerService   │
│  - Conversation history, MEMORY.md, SOUL.md, IDENTITY.md          │
│  - Tool dispatch (Toolkit), Reasoner lifecycle                    │
│  - Silence timers, prefix prefill orchestration                   │
│  - Phase 1 endpoint: dev-GUI/TUI via local WebSocket              │
└────────────────────────────┬─────────────────────────────────────┘
              gRPC (Unix socket)          gRPC (Unix socket)
         ┌────────────────────┘          └──────────────────────┐
         ▼                                                       ▼
┌────────────────────────────────┐          ┌───────────────────────────┐
│  Process 2: Talker Agent (Python)│          │  Process 4+: Reasoner(s)  │
│  Shared asyncio event loop      │          │  (TypeScript, on-demand)  │
│  ├── voice/listener.py          │          │  One process per task_id  │
│  │   RealtimeSTT (faster-       │          │  Adapter pattern:         │
│  │   whisper + Silero VAD)      │          │  OpenClaw / Claude Code   │
│  │   + custom turn detection    │          └───────────────────────────┘
│  ├── voice/speaker.py           │
│  │   RealtimeTTS (Kokoro)       │
│  └── brain/ LLM pipeline        │
│      HTTP → llama.cpp (Process 3)│
└────────────────────────────────┘
              ▲
              │ HTTP (localhost, ~0.1ms)
              ▼
┌────────────────────────────────┐
│  Process 3: llama.cpp server    │
│  Qwen3-8B Q4, KV cache,         │
│  OpenAI-compatible API          │
└────────────────────────────────┘
```

**Key invariants:**
- The Gateway is the only component that touches the filesystem (MEMORY.md, SOUL.md, IDENTITY.md, workspace tools).
- The Talker is fully stateless. It receives all context via gRPC every turn.
- Audio frames never enter protobuf serialization at 50fps. Raw bytes over Unix socket.
- Tokens never cross the gRPC boundary. The soul container absorbs them; only complete semantic units (sentences, tags) exit via gRPC.

---

## 2. Design Principles

These were agreed during design and must be preserved:

1. **Store structured, format late.** Conversation history travels as `repeated ChatMessage` with a `Role` enum. The Talker formats it into the Qwen3 chat template (`<|im_start|>user\n...<|im_end|>`) at inference time. The Gateway never touches prompt format.

2. **Typed proto fields for structure; `string` for natural language.** `memory_contents`, `tool_result_content`, and `user_input` are `string` because they are natural-language text that goes directly into the LLM prompt — no benefit in parsing them as structured proto. Everything else (task IDs, timestamps, roles, sequence numbers) is typed.

3. **Sentence is the streaming granularity.** The soul container buffers tokens and emits one `TalkerOutput` message per sentence boundary or tag extraction. Never stream individual tokens over gRPC. This maps to ~13s TTFA vs 26s for full-response batching, and matches Kokoro's minimum stable synthesis window.

4. **`oneof` for all multiplexed event streams.** `TalkerOutput`, `ListenerEvent`, and `ReasonerEvent` all use `oneof` at the top level to multiplex event types. No separate streams per type.

5. **Sequence numbers on all output messages.** Include `seq uint32` on `TalkerOutput` for ordering. Stream ordering bugs are hard to diagnose without them.

6. **No pipeline framework.** RealtimeSTT and RealtimeTTS are component libraries, not orchestrators. The topology (Gateway as conductor, Listener and Talker as separate asyncio tasks) cannot be expressed in Pipecat/LiveKit Agents without fighting their end-to-end pipeline model.

7. **buf for proto linting, enforced in CI.** buf lint + buf breaking as a GitHub Actions workflow on every push. `buf breaking` catches backwards-incompatible changes (deleted fields, changed types) automatically.

8. **`request_id` on every async dispatch.** `ToolRequest` carries a `request_id`; `DelegateRequest` carries a `task_id`. These IDs flow through the entire async lifecycle so Gateway can correlate results to requests. The `task_id` on `DelegateRequest` is generated by the Talker, not the Gateway, so the Talker can later recognize its own delegated tasks in narration context.

9. **Message size limits enforced.** gRPC has a default 4 MiB per-message limit. `TalkerContext` packs both conversation history and memory contents into a single synchronous message. History compaction (M1.3) and MEMORY.md growth must be bounded: history truncated at N turns (configurable, Phase 1 default N=20), MEMORY.md validated at load time to reject files >2 MiB (migration to ChromaDB deferred to Phase 2). Gateway must fail-fast with clear error if context assembly would exceed 3.5 MiB (safety margin below gRPC limit).

---

## 3. Proto Schema (Canonical — Do Not Modify Without Updating This Document)

File: `proto/kaguya/v1/kaguya.proto`

```protobuf
syntax = "proto3";
package kaguya.v1;

// ─────────────────────────────────────────────
// SHARED TYPES
// ─────────────────────────────────────────────

enum Role {
  ROLE_UNSPECIFIED = 0;
  ROLE_SYSTEM      = 1;
  ROLE_USER        = 2;
  ROLE_ASSISTANT   = 3;
  ROLE_TOOL        = 4;  // tool result injection round
}

message ChatMessage {
  Role   role         = 1;
  string content      = 2;
  string name         = 3;  // tool name when role=ROLE_TOOL
  int64  timestamp_ms = 4;
}

message ToolDefinition {
  string name        = 1;
  string description = 2;
  string args_schema = 3;  // JSON Schema string
}

// ─────────────────────────────────────────────
// LISTENER SERVICE  (Listener → Gateway)
// ─────────────────────────────────────────────

service ListenerService {
  rpc StreamEvents(stream ListenerEvent) returns (ListenerAck);
}

message ListenerEvent {
  int64 timestamp_ms = 1;
  oneof event {
    VadSpeechStart    vad_speech_start   = 2;
    VadSpeechEnd      vad_speech_end     = 3;
    PartialTranscript partial_transcript = 4;
    FinalTranscript   final_transcript   = 5;
  }
}

message VadSpeechStart    {}
message VadSpeechEnd      { float silence_duration_ms = 1; }
message PartialTranscript { string text = 1; }
message FinalTranscript   { string text = 1; float confidence = 2; }

message ListenerAck {}

// ─────────────────────────────────────────────
// TALKER SERVICE  (Gateway → Talker)
// ─────────────────────────────────────────────

service TalkerService {
  rpc ProcessPrompt(TalkerContext)  returns (stream TalkerOutput);
  rpc Prepare(PrepareSignal)        returns (PrepareAck);
  rpc PrefillCache(PrefillRequest)  returns (PrefillAck);
  rpc UpdatePersona(PersonaConfig)  returns (PersonaAck);
}

// Full context package assembled by Gateway each turn.
// Gateway assembles; Talker formats. Gateway has no knowledge of prompt format.
message TalkerContext {
  string   conversation_id          = 1;
  string   turn_id                  = 2;   // unique per ProcessPrompt call
  string   user_input               = 3;   // final_transcript text (natural language)
  repeated ChatMessage history      = 4;   // recent turns, Gateway manages window
  string   memory_contents          = 5;   // full MEMORY.md text (natural language → prompt)
  repeated ToolDefinition tools     = 6;
  string   active_tasks_json        = 7;   // JSON: ongoing Reasoner task summaries
  string   tool_result_content      = 8;   // natural-language tool result (if tool-result round)
  string   tool_request_id          = 9;   // correlates to ToolRequest.request_id
  int64    timestamp_ms             = 10;
  string   reasoner_task_id         = 11;  // correlates to DelegateRequest.task_id
  string   reasoner_result_content  = 12;  // natural-language reasoner result (if reasoner-result round)
}

// Streamed back to Gateway during generation.
// One message per sentence boundary or extracted tag.
// seq is monotonically increasing within a single ProcessPrompt call.
message TalkerOutput {
  uint32 seq = 1;
  oneof payload {
    ResponseStarted  response_started  = 2;
    SentenceEvent    sentence          = 3;
    EmotionEvent     emotion           = 4;
    ToolRequest      tool_request      = 5;
    DelegateRequest  delegate_request  = 6;
    ResponseComplete response_complete = 7;
  }
}

message ResponseStarted  { string turn_id = 1; }
message SentenceEvent    { string text = 1; }
message EmotionEvent     { string emotion = 1; }  // joy|concern|thinking|surprise|neutral|determined

message ToolRequest {
  string request_id = 1;   // Gateway correlates async result back to this
  string tool_name  = 2;
  string args_json  = 3;   // dynamic schema — string (not Struct) for simplicity
}

message DelegateRequest {
  string task_id    = 1;   // generated by Talker; stable through full Reasoner lifecycle
  string description = 2;
}

message ResponseComplete {
  string turn_id       = 1;
  bool   was_interrupted = 2;
}

message PrepareSignal   { string conversation_id = 1; }

// spoken_text and unspoken_text are both empty if Talker was already idle.
message PrepareAck {
  string spoken_text   = 1;
  string unspoken_text = 2;
}

message PrefillRequest {
  string conversation_id = 1;
  TalkerContext context  = 2;  // same shape; n_predict=0 implied by call type
}
message PrefillAck {}

message PersonaConfig {
  string soul_md     = 1;
  string identity_md = 2;
  string memory_md   = 3;
}
message PersonaAck {}

// ─────────────────────────────────────────────
// REASONER SERVICE  (Gateway → Reasoner)
// ─────────────────────────────────────────────

service ReasonerService {
  rpc ExecuteTask(TaskRequest)  returns (stream ReasonerEvent);
  rpc CancelTask(CancelRequest) returns (CancelAck);
}

message TaskRequest {
  string task_id     = 1;   // same ID from DelegateRequest.task_id
  string description = 2;
  map<string, string> metadata = 3;
}

message ReasonerEvent {
  string task_id      = 1;
  int64  timestamp_ms = 2;
  oneof event {
    ReasonerStarted          started   = 3;
    ReasonerIntermediateStep step      = 4;
    ReasonerOutput           output    = 5;
    ReasonerCompleted        completed = 6;
    ReasonerError            error     = 7;
  }
}

message ReasonerStarted          { string framework = 1; }
message ReasonerIntermediateStep { string description = 1; }
message ReasonerOutput           { string content = 1; }
message ReasonerCompleted        { string summary = 1; }
message ReasonerError            { string message = 1; int32 code = 2; }

message CancelRequest { string task_id = 1; }
message CancelAck {}

// ─────────────────────────────────────────────
// GATEWAY CONTROL  (Endpoint → Gateway)
// ─────────────────────────────────────────────

service RouterControl {
  rpc SendControl(ControlSignal) returns (ControlAck);
}

message ControlSignal {
  oneof signal {
    StopSignal     stop     = 1;
    ApprovalSignal approval = 2;
    ShutdownSignal shutdown = 3;
  }
}
message StopSignal     {}
message ApprovalSignal { string context = 1; }
message ShutdownSignal {}
message ControlAck {}
```

---

## 4. Implementation TODOs

Work in this order. Each milestone produces a runnable/testable artifact.

---

### Milestone 0 — Scaffolding and Proto (do first, blocks everything)

**Goal:** All stubs generated, buf lint passing, CI running.

- [ ] **M0.1** Create `proto/buf.yaml`:
  ```yaml
  version: v1
  name: buf.build/kaguya/kaguya
  lint:
    use: [DEFAULT]
  breaking:
    use: [FILE]
  ```

- [ ] **M0.2** Write `proto/kaguya/v1/kaguya.proto` from the schema in Section 3 above.

- [ ] **M0.3** Create `proto/buf.gen.yaml` for all three target languages:
  ```yaml
  version: v1
  plugins:
    - plugin: buf.build/protocolbuffers/python
      out: ../talker/proto
    - plugin: buf.build/grpc/python
      out: ../talker/proto
    - plugin: buf.build/protocolbuffers/js
      out: ../reasoner/proto
    - plugin: buf.build/grpc/node
      out: ../reasoner/proto
  ```
  Rust stubs for Gateway are generated by `tonic-build` in `build.rs`, not by buf.

- [ ] **M0.4** Write `scripts/generate_protos.sh`:
  - Run `buf generate` from `proto/`
  - Run `cargo build` in `gateway/` (triggers tonic-build)
  - Output: stubs in `talker/proto/`, `reasoner/proto/`, `gateway/src/proto/`

- [ ] **M0.5** Create `.github/workflows/proto-lint.yml`:
  ```yaml
  name: Proto Lint
  on: [push, pull_request]
  jobs:
    buf:
      runs-on: ubuntu-latest
      steps:
        - uses: actions/checkout@v4
        - uses: bufbuild/buf-setup-action@v1
        - run: buf lint proto/
        - run: buf breaking proto/ --against '.git#branch=main'
  ```

- [ ] **M0.6** Add generated proto output dirs to `.gitignore`:
  ```
  talker/proto/
  reasoner/proto/
  gateway/src/proto/
  ```

- [ ] **M0.7** Add `proto/` stubs to each service's `pyproject.toml` / `package.json` / `Cargo.toml` build steps.

**Done when:** `buf lint proto/` passes, `make proto` regenerates all stubs without errors.

---

### Milestone 1 — Gateway Core (Rust)

**Goal:** Gateway binary that accepts gRPC connections and processes events through the Input Stream. No real Talker or Listener yet — use stub clients.

#### M1.1 — Crate setup

- [ ] Init `gateway/` as a Cargo workspace with crate `kaguya-gateway`.
- [ ] Add dependencies: `tokio`, `tonic`, `prost`, `tonic-build` (build dep).
- [ ] Write `gateway/build.rs` to compile proto → Rust stubs via tonic.

#### M1.2 — Input Stream

- [ ] Define Rust enum `InputEvent` covering all P0–P5 event types from spec §3.1.
- [ ] Implement priority queue as six `tokio::sync::mpsc` channels (one per priority level).
- [ ] Implement event loop: `tokio::select!` polling all channels in priority order (P0 first, P5 last).
- [ ] **[DECISION]** P0 control signals bypass the queue entirely — they are handled in a separate select branch that preempts all others.
- [ ] Write unit tests: verify P0 preempts P3 when both arrive simultaneously.

#### M1.3 — State machine

- [ ] Define `GatewayState` struct:
  - `conversation_history: Vec<ChatMessage>` (in-memory rolling log)
  - `persona: PersonaConfig` (cached, delivered to Talker on connect)
  - `active_tasks: HashMap<String, TaskState>` (task_id → state)
  - `pending_tool_requests: HashMap<String, ToolRequest>` (request_id → request)
  - `silence_timers: SilenceTimers` — three named handles for the three semantic tiers (see M1.6)
- [ ] Implement history compaction stub (no LLM call yet — just truncate at N turns for Phase 1).

#### M1.4 — Persona file loading

- [ ] Read `SOUL.md`, `IDENTITY.md`, `MEMORY.md` from `config/` at startup.
- [ ] Set up file watcher (`notify` crate) for all three.
- [ ] On change: re-read, update cached `PersonaConfig`, send `UpdatePersona` gRPC to Talker.
- [ ] **[DECISION]** Gateway is the only reader/writer of these files. Talker has no filesystem access.

#### M1.5 — gRPC server (tonic)

- [ ] Implement `ListenerServiceServer`: receive `ListenerEvent` stream, push events into Input Stream at correct priority level.
- [ ] Implement gRPC client stub for `TalkerService` (Gateway is the *client* calling the Talker): connect to Talker's Unix socket, stubs return empty at this stage.
- [ ] Implement `RouterControlServer`: handle `StopSignal`, `ShutdownSignal` directly (P0 bypass).
- [ ] Configure Unix domain socket transport for all services.

#### M1.6 — Turn lifecycle (core loop)

- [ ] On `vad_speech_start` [P2]: dispatch `Prepare` gRPC to Talker; cancel all silence timers.
- [ ] On `final_transcript` [P1]: assemble `TalkerContext`; dispatch `ProcessPrompt` gRPC.
- [ ] On `TalkerOutput.ResponseComplete`: append to history; dispatch `PrefillCache` to Talker; start silence timer cascade.
- [ ] On `TalkerOutput.ToolRequest`: dispatch to Toolkit; inject result as P3 `tool_result` event.
- [ ] On `TalkerOutput.DelegateRequest`: spawn Reasoner; inject events as P3.
- [ ] Implement three-tier silence timer cascade [P4]:
  - `SILENCE_SHORT` (~3s, configurable): "user thinking" — user may still be processing the response. Emit `silence_short` P4 event → soft conversational follow-up.
  - `SILENCE_MEDIUM` (~8s, configurable): "user intent unclear" — user may have paused, be multitasking, or be done. Emit `silence_medium` P4 event → open-ended check-in.
  - `SILENCE_LONG` (~30s, configurable): "user likely AFK" — user has disengaged. Emit `silence_long` P4 event → context-shift acknowledgement or go quiet.
  - All three timers start sequentially after `ResponseComplete`; all are cancelled on `vad_speech_start` or `text_command`.
  - **[DECISION]** Timings are semantic defaults backed by voice AI research: 3s matches IVR short-silence thresholds; 8s is standard extended-pause; 30s is clear AFK territory. All three are configurable in `config.rs` — do not hardcode values.
  - Implement as `struct SilenceTimers { short: Option<JoinHandle>, medium: Option<JoinHandle>, long: Option<JoinHandle> }`.

#### M1.7 — Context package assembly

- [ ] Implement `assemble_context(user_input, history, persona, active_tasks, tool_result?) -> TalkerContext`.
- [ ] History window: include last N turns in full (Phase 1: N=20, configurable).
- [ ] Include `memory_contents` from cached `persona.memory_md`.
- [ ] Include `active_tasks_json` summarising ongoing Reasoner tasks.
- [ ] Set `tool_request_id` and `tool_result_content` when this is a tool-result round.

**Done when:** Gateway starts, loads persona files, accepts gRPC connections, and routes events through the Input Stream. Verified with integration test using a stub Talker that echoes requests.

---

### Milestone 2 — Talker: voice/listener.py

**Goal:** VAD + STT + turn detection running in asyncio, streaming events to Gateway via gRPC.

**[DECISION]** Listener and Talker share a single Python process (Process 2) for GPU context sharing. They run as separate asyncio tasks, not threads.

#### M2.1 — Project setup

- [ ] Init `talker/pyproject.toml` with `uv`. Dependencies:
  - `RealtimeSTT` (KoljaB/RealtimeSTT)
  - `opuslib` (Opus → PCM decoding before feeding RealtimeSTT — see M2.2)
  - `grpcio`, `grpcio-tools` (or rely on buf-generated stubs)
  - `pydantic` (for config)
- [ ] Write `talker/config.py` as a `pydantic.BaseSettings` class:
  - `llama_cpp_url: str = "http://localhost:8080"`
  - `gateway_socket: str = "/tmp/kaguya-gateway.sock"`
  - `silence_threshold_ms: int = 800`
  - `syntax_silence_threshold_ms: int = 300`
  - `kokoro_voice: str = "af_heart"` (placeholder — [OPEN] voice selection needs listening tests)
  - `log_level: str = "INFO"`

#### M2.2 — voice/listener.py

- [ ] Implement Opus → PCM decoder (`voice/opus_decoder.py`, ~20 lines):
  - `OpusDecoder` wraps `opuslib.Decoder(fs=16000, channels=1)` — tell libopus to decode directly to 16kHz mono (libopus handles internal resampling; no separate downsample step needed).
  - `decode(opus_frame: bytes) -> bytes`: call `decoder.decode(opus_frame, frame_size=320)` — 320 samples = 20ms at 16kHz output (16000 × 0.02). Returns 16-bit signed PCM ready for `recorder.feed_audio()`.
  - **[DECISION]** Opus decoding belongs in the Listener (Python), not the Gateway (Rust). See REF-002 for full rationale. Gateway spec §1 prohibits audio decoding in the Gateway; `spec-agent §2.2` lists this as a Listener responsibility explicitly.
- [ ] Wrap `RealtimeSTT.AudioToTextRecorder` with callbacks:
  - `on_vad_detect_start` → emit `ListenerEvent(vad_speech_start)`
  - `on_vad_detect_stop` → emit `ListenerEvent(vad_speech_end)`
  - `on_realtime_transcription_update` → emit `ListenerEvent(partial_transcript)`
  - `on_transcription_complete` → pass to turn detection
- [ ] Feed audio to RealtimeSTT via `recorder.feed_audio(pcm_bytes)` after Opus decode. RealtimeSTT expects 16kHz mono 16-bit PCM chunks via `feed_audio`, not raw Opus.
- [ ] **[DECISION]** RealtimeSTT's own `on_transcription_complete` fires on VAD silence, not on turn detection. Do not emit `final_transcript` here — pass to `turn_detection.py` instead.
- [ ] Configure RealtimeSTT: `model="distil-large-v3"`, `compute_type="int8"`, language autodetect or `"en"`.
- [ ] Run as asyncio task via `asyncio.to_thread` (RealtimeSTT uses blocking callbacks).

#### M2.3 — voice/turn_detection.py

Phase 1 rule-based implementation (~50-100 lines):
- [ ] Track accumulated silence duration from VAD events.
- [ ] On STT buffer update: check syntactic completeness with regex:
  - Terminal punctuation (`.`, `?`, `!`) at end of current buffer → syntactically complete.
  - Open clause markers (dangling prepositions, conjunctions at end: `"and"`, `"but"`, `"the"`, `"of"`) → incomplete.
- [ ] Thresholds (all configurable via `config.py`):
  - silence < 300ms → continue accumulating, no emit.
  - 300ms ≤ silence < 800ms AND syntactically incomplete → wait.
  - 300ms ≤ silence < 800ms AND syntactically complete → emit `final_transcript`.
  - silence ≥ 800ms → emit `final_transcript` regardless of syntax.
- [ ] Reset state on `vad_speech_start` (user resumed mid-turn).

#### M2.4 — gRPC client (Listener side)

- [ ] Implement async gRPC client connecting to Gateway's `ListenerService` via Unix socket.
- [ ] `stream_events()` coroutine: async generator of `ListenerEvent` messages.
- [ ] Reconnect with exponential backoff on connection loss.

**Done when:** `python -m talker.main` starts, VAD fires on microphone input, events appear in Gateway logs.

---

### Milestone 3 — Talker: brain/

**Goal:** LLM inference pipeline — context package → prompt → token stream → soul container → `TalkerOutput` gRPC events.

#### M3.1 — brain/llm_client.py

- [ ] Async HTTP client (`httpx.AsyncClient`) to llama.cpp's OpenAI-compatible API.
- [ ] `stream_completion(prompt: str) -> AsyncIterator[str]`: POST to `/v1/completions` with `stream=True`, parse SSE token chunks.
- [ ] `prefill(prompt: str)`: POST with `n_predict=0, cache_prompt=True`.
- [ ] Handle llama.cpp connection errors with retries (3 attempts, 1s backoff).
- [ ] **[DECISION]** HTTP client, not gRPC. llama.cpp speaks OpenAI-compatible HTTP. Overhead is ~0.1ms on localhost — negligible.

#### M3.2 — brain/prompt_formatter.py

- [ ] `format_prompt(ctx: TalkerContext, persona: PersonaConfig) -> str`
- [ ] Apply Qwen3 chat template: `<|im_start|>system\n...<|im_end|>\n<|im_start|>user\n...<|im_end|>\n<|im_start|>assistant\n`
- [ ] Prompt structure (in order, per spec §3.3):
  1. System: `SOUL.md` + `IDENTITY.md` persona
  2. System: structured output instructions (emotion tags, tool tags, delegate tags)
  3. System: available tools list (from `ctx.tools`)
  4. System: tool use examples (few-shot)
  5. System: current context (timestamp, active tasks from `ctx.active_tasks_json`)
  6. Memory: `ctx.memory_contents` (full MEMORY.md)
  7. Conversation history: `ctx.history` formatted as alternating user/assistant turns
  8. If `ctx.tool_result_content`: inject as ROLE_TOOL turn before user input
  9. User: `ctx.user_input`
- [ ] **[DECISION]** The Gateway assembles the context package; the Talker formats it into the prompt. Gateway has zero knowledge of prompt format. This boundary is strict.

#### M3.3 — brain/sentence_detector.py

~50-80 lines. Accumulates tokens, yields complete sentences:
- [ ] `SentenceDetector` class with `feed(token: str) -> Optional[str]`.
- [ ] Flush on: `.`, `?`, `!` followed by whitespace and uppercase letter, OR end-of-generation.
- [ ] Edge case handling via regex:
  - Abbreviations: `Dr.`, `Mr.`, `Mrs.`, `vs.`, `etc.` → do not flush.
  - Decimals: `3.14`, `$4.99` → do not flush.
  - URLs: `https://` mid-sentence → do not flush.
- [ ] `flush() -> Optional[str]`: force-emit remaining buffer (called on stream end).

#### M3.4 — brain/soul_container.py

~80-120 lines. Processes one complete sentence. Pure function (stateless, deterministic):
- [ ] `process(sentence: str, identity_config: IdentityConfig) -> SoulContainerResult` where result contains:
  - `spoken_text: str` (sentence with all tags stripped — goes to TTS)
  - `emotions: list[str]` (extracted `[EMOTION:...]` values)
  - `tool_requests: list[ToolRequest]` (extracted `[TOOL:...]` calls)
  - `delegate_requests: list[DelegateRequest]` (extracted `[DELEGATE:...]` calls)
- [ ] Tag normalization: `[EMOTION:happy]` → `[EMOTION:joy]`, `[EMOTION:sad]` → `[EMOTION:concern]`.
- [ ] Default injection: if no `[EMOTION:...]` tag in sentence, inject `EMOTION:neutral` in result.
- [ ] Strip hallucinated action tags (anything that doesn't match the known tag schemas).
- [ ] Apply vocabulary rules from `IDENTITY.md`: `IdentityConfig` carries a list of `(pattern: regex, replacement: str)` pairs parsed from an `## Vocabulary` section of `IDENTITY.md`. Apply in order to `spoken_text` before TTS.
- [ ] Enforce max response length: if the current sentence would exceed ~2-4 sentences of total spoken output for this turn (tracked by caller), truncate here and set a `truncated: bool` flag in `SoulContainerResult`. The caller (`server.py`) tracks sentence count per turn and passes it in.
- [ ] **[DECISION]** Soul container operates on complete sentences after boundary detection, never on individual tokens. It is a pure function — no LLM calls, no side effects.

#### M3.5 — server.py (gRPC servicer — wires Gateway ↔ brain ↔ voice)

- [ ] Implement `TalkerServiceServicer`:
  - `ProcessPrompt(ctx, stream_context)`: format prompt → stream tokens → sentence detect → soul container → yield `TalkerOutput` messages.
  - `Prepare(signal, context)`: if generating: stop TTS + cancel LLM; return `PrepareAck` with spoken/unspoken split. If idle: return empty `PrepareAck`.
  - `PrefillCache(req, context)`: call `llm_client.prefill(prompt)`.
  - `UpdatePersona(config, context)`: update cached `PersonaConfig` in memory.
- [ ] Yield order for one sentence: `SentenceEvent` first, then any `EmotionEvent`/`ToolRequest`/`DelegateRequest` extracted from that sentence, then continue to next sentence.
- [ ] `ResponseComplete` is the final yield, always. Set `was_interrupted=True` if cancelled mid-generation.
- [ ] PREPARE cancellation: set a `asyncio.Event` that the `ProcessPrompt` stream checks between sentences. On set: record split point, stop yielding, yield `ResponseComplete(was_interrupted=True)`.

#### M3.6 — voice/speaker.py

- [ ] Wrap `RealtimeTTS.TextToAudioStream` with Kokoro engine.
- [ ] `speak(text: str)`: feed sentence to TTS, stream audio to output device.
- [ ] `stop()`: interrupt playback mid-word (RealtimeTTS supports this natively).
- [ ] Track playback position for PREPARE token accounting:
  - On each sentence fed to TTS: record `(sentence_text, feed_timestamp)`.
  - On `stop()`: estimate how much of current sentence was played using `(now - feed_timestamp) / estimated_duration`. Store as `spoken_fraction`.
  - Return `(spoken_text, unspoken_text)` by splitting at `spoken_fraction` on word boundary.
- [ ] **[OPEN]** Word-boundary split estimation is approximate in Phase 1. Exact accounting requires Kokoro's audio duration output per phoneme — evaluate whether RealtimeTTS exposes this.

#### M3.7 — prepare.py (PREPARE handler — crosses voice + brain)

- [ ] `PrepareHandler.handle(talker_servicer, speaker) -> PrepareAck`:
  - Call `talker_servicer.cancel_generation()` (sets the asyncio.Event).
  - Call `speaker.stop()` → get `(spoken_text, unspoken_text)`.
  - Return `PrepareAck(spoken_text=spoken_text, unspoken_text=unspoken_text)`.
- [ ] Idempotent: if already idle, return `PrepareAck()` with empty strings immediately.

#### M3.8 — main.py

- [ ] `async def main()`:
  1. Load `TalkerConfig`.
  2. Init `InferenceEngine` (start llama.cpp connection check).
  3. Load persona from cached file or wait for `UpdatePersona` gRPC call.
  4. Start gRPC server (Unix socket, `TalkerServiceServicer`).
  5. Start Listener asyncio task (`voice/listener.py`).
  6. `await server.wait_for_termination()`.
- [ ] `if __name__ == "__main__": asyncio.run(main())`

**Done when:** Full voice turn works end-to-end: speak → transcript → LLM → TTS output. Tags extracted. PREPARE interrupts correctly.

---

### Milestone 4 — Toolkit (TypeScript)

**Goal:** Tool execution in sandboxed TypeScript, results returned to Gateway.

- [ ] Init `tools/` as a Node.js/TypeScript project (`package.json`, `tsconfig.json`).
- [ ] Implement Phase 1 tools (all sandboxed, paths enforced relative to workspace root):
  - `web_fetch(url: string) -> string`: fetch URL, return markdown-converted content.
  - `write_file(path: string, content: string) -> string`: write to workspace-relative path.
  - `read_file(path: string) -> string`: read from workspace-relative path.
  - `list_files(path: string) -> string[]`: list directory contents.
  - `search_tools(query: string) -> ToolDefinition[]`: vector similarity over tool descriptions (stub: return top-5 matching tools by name keyword for Phase 1).
- [ ] Tool registry: `Map<string, ToolFn>` — Gateway calls these by name.
- [ ] Workspace root enforcement: all file paths resolved through `path.resolve(workspaceRoot, userPath)`. Reject paths that escape the root.
- [ ] Expose as a gRPC service OR as a simple IPC bridge to Gateway — coordinate with Gateway implementation.
- [ ] **[DECISION]** Tools are TypeScript for MCP ecosystem compatibility. They run in a separate sandboxed process managed by the Gateway.
- [ ] **[OPEN]** The exact tool list is unresolved: `spec-gateway-v0.1.0.md §14` lists `search_files` and `run_command` which are absent here; `spec-agent-v0.1.0.md §3.3` lists `web_fetch` and `search_tools` which are absent from the gateway spec. Finalize the canonical tool list during M4 implementation before writing the registry. Document the decision in `REFERENCES.md`.

---

### Milestone 5 — Reasoner (TypeScript)

**Goal:** Reasoner adapter that wraps OpenClaw (or Claude Code), intercepts output, streams events to Gateway.

- [ ] Init `reasoner/` as Node.js/TypeScript project.
- [ ] Implement `ReasonerServiceClient` (gRPC client connecting to Gateway's `ReasonerService`).
- [ ] Implement `OpenClawAdapter`:
  - Spawn OpenClaw subprocess for given `TaskRequest.description`.
  - Monitor stdout/stderr.
  - Translate to `ReasonerEvent` stream:
    - On spawn: emit `ReasonerStarted`.
    - On meaningful stdout line: emit `ReasonerIntermediateStep`.
    - On partial result: emit `ReasonerOutput`.
    - On process exit 0: emit `ReasonerCompleted`.
    - On process exit non-0: emit `ReasonerError`.
  - Stream all events to Gateway via gRPC.
- [ ] Implement `CancelTask`: send SIGTERM to subprocess, confirm exit.
- [ ] Adapter pattern: `interface ReasonerAdapter { execute(task: TaskRequest): AsyncIterator<ReasonerEvent> }`. OpenClaw and Claude Code are separate adapters — swap without changing Gateway.
- [ ] **[DECISION]** Reasoner is TypeScript for Node.js/OpenClaw ecosystem compatibility.
- [ ] **[DECISION]** One Reasoner process per task. Gateway spawns a new process for each `DelegateRequest`. The `task_id` from the Talker is stable through the entire lifecycle.

---

### Milestone 6 — Dev-GUI/TUI (Phase 1 Endpoint)

**Goal:** Local interface that allows testing the full pipeline without OpenPod.

- [ ] Decide transport: **WebSocket** (recommended — easier browser dev tools debugging than stdio).
- [ ] Gateway side: `tokio-tungstenite` WebSocket server on `ws://localhost:8765`.
- [ ] Demux incoming WebSocket messages:
  - Binary frames → audio bytes → forward to Listener.
  - JSON `{"type": "text", "text": "..."}` → `text_command` → Input Stream [P1].
  - JSON `{"type": "control", "signal": "stop"}` → P0 bypass.
- [ ] Mux outgoing:
  - Audio bytes from Talker TTS → binary WebSocket frame.
  - Transcript/emotion metadata → JSON WebSocket frame.
- [ ] TUI client (Python or shell script): capture microphone → send audio frames; receive audio → play back; display metadata.
- [ ] **[DECISION]** Phase 1 only. Replaced by OpenPod in Phase 2. Design should be a thin shim — no business logic in the TUI/GUI.

---

### Milestone 7 — Integration Tests

- [ ] Test 1: Full voice turn. Speak "What time is it?" → verify `final_transcript` → `TalkerContext` → `ProcessPrompt` → `SentenceEvent` arrives at Gateway → `ResponseComplete`.
- [ ] Test 2: Barge-in. Talker mid-sentence → `vad_speech_start` → `Prepare` → `PrepareAck` with non-empty `spoken_text` → history contains only spoken portion.
- [ ] Test 3: Tool call. Prompt triggers `[TOOL:web_fetch(...)]` → `ToolRequest` in `TalkerOutput` → Gateway dispatches to Toolkit → tool result → new `ProcessPrompt` round.
- [ ] Test 4: Delegation. Prompt triggers `[DELEGATE:...]` → `DelegateRequest` → Reasoner spawned → `ReasonerCompleted` → narration turn.
- [ ] Test 5: Silence timer. No input for 3s after response → `silence_exceeded` → soft prompt Talker turn.
- [ ] Test 6: PREPARE on idle Talker. `vad_speech_start` arrives when Talker is idle → `Prepare` → `PrepareAck` with empty strings → no history corruption.
- [ ] Test 7: Prefix prefill. After `ResponseComplete`, verify `PrefillCache` is dispatched to Talker. Verify next `ProcessPrompt` latency is measurably lower (benchmark test).

---

## 5. Open Questions (Empirical — Resolve During Implementation)

These require implementation and measurement to answer. Do not design around them speculatively.

| # | Question | Resolve By |
|---|----------|------------|
| OQ1 | GPU compute contention: faster-whisper + llama.cpp + Kokoro concurrently on RTX 5070 Ti. VRAM fits on paper (~9-11 GB of 16 GB). Actual compute contention unknown. | M3 integration test — run all three concurrently, measure throughput degradation. |
| OQ2 | Kokoro voice selection. Which of the 14 built-in voices best fits Kaguya? | Listening test after M3.6 lands. |
| OQ3 | LLM action tag reliability at 8B scale. Does Qwen3-8B emit `[EMOTION:...]`/`[DELEGATE:...]` consistently (≥95% of turns) via system prompt alone? If <95%, accelerates QLoRA decision. | Measure during M3 end-to-end testing. |
| OQ4 | RealtimeSTT/TTS built-in vs custom sentence detection. RealtimeTTS has its own sentence detection. Evaluate whether it is sufficient for Phase 1 or custom `sentence_detector.py` must override it from day one. Reference: KoljaB/RealtimeVoiceChat. | M3.3/M3.6 benchmarking. |
| OQ5 | PREPARE token accounting precision. Word-boundary split estimation is approximate without phoneme-level timing from Kokoro. Evaluate whether RealtimeTTS exposes audio duration per phoneme. | M3.6 implementation. |
| OQ6 | MEMORY.md growth threshold. At what entry count does `MEMORY.md` content consume enough context window tokens to warrant ChromaDB migration? | Measure against Qwen3-8B context window after M1 context assembly is running. |
| OQ7 | Post-turn memory evaluation heuristic. What criteria does the Gateway use to decide if something is "memory-worthy"? Rule-based (new project name, preference) vs. lightweight LLM classification call. | Design after M1 turn lifecycle is running — evaluate cost of LLM classification call vs. false-positive rate of rules. |

---

## 6. Phase 2 Scope (Out of Scope for This Plan)

The following are deferred to Phase 2. Do not implement them in Phase 1, even partially:

- OpenPod protocol integration (Channel A, Channel D, telemetry P5 events).
- Partial transcript prefill (word-level KV cache extension).
- Speculative decoding (draft tokens before final_transcript).
- Two-stage PREPARE signal (soft fade / hard stop).
- ChromaDB vector store.
- QLoRA fine-tuning.
- Custom TTS voice (Chatterbox/Qwen3-TTS).
- Tool Search Tool for large MCP registries.
- Programmatic Tool Calling (multi-tool scripts).
- Learned turn detection model.
- Speaker diarization.

---

## 7. VRAM and Latency Budgets (Reference)

### VRAM (RTX 5070 Ti — 16 GB)

| Component | VRAM |
|---|---|
| LLM (Qwen3-8B Q4) | ~5-6 GB |
| STT (faster-whisper distil-large-v3 INT8) | ~1 GB |
| TTS (Kokoro-82M) | ~0.5 GB |
| KV cache + overhead | ~2-3 GB |
| **Total** | **~9-11 GB** |
| **Headroom** | **~5-7 GB** |

### Latency (target with Phase 1 prefix prefill)

| Stage | Target |
|---|---|
| Listener VAD onset | ~30-50ms |
| STT partial | ~200-300ms |
| LLM first token (warm prefix cache) | ~50-100ms |
| LLM first sentence | ~200-500ms |
| TTS first audio | ~50-150ms |
| **Total (user stops → Kaguya speaks)** | **~400-700ms** |

---

## 8. Directory Structure

```
kaguya/
├── .github/
│   └── workflows/
│       └── proto-lint.yml          # buf lint + buf breaking on push/PR
├── config/
│   ├── SOUL.md                     # Kaguya persona — tone, values, voice
│   ├── IDENTITY.md                 # Kaguya identity — name, backstory, rules
│   └── MEMORY.md                   # Long-term memory — user profile, project facts
├── docker/
│   └── docker-compose.yml          # Gateway + Talker + llama.cpp + Reasoner
├── docs/
│   ├── spec-agent-v0.1.0.md        # Agent (Listener + Talker) spec
│   ├── spec-gateway-v0.1.0.md      # Gateway spec
│   └── implementation-plan-v0.1.0.md  # This document
├── gateway/                        # Rust — tokio/tonic
│   ├── src/
│   │   ├── main.rs
│   │   ├── input_stream.rs         # Priority queue (P0–P5)
│   │   ├── state.rs                # GatewayState, history, active tasks
│   │   ├── context.rs              # TalkerContext assembly
│   │   ├── persona.rs              # File loading, file watcher, UpdatePersona dispatch
│   │   ├── services/
│   │   │   ├── listener.rs         # ListenerServiceServer impl
│   │   │   ├── talker.rs           # TalkerServiceServer impl (client to Talker)
│   │   │   ├── reasoner.rs         # ReasonerService + Reasoner lifecycle management
│   │   │   └── control.rs          # RouterControlServer impl
│   │   ├── toolkit.rs              # Tool dispatch, TypeScript subprocess management
│   │   ├── timers.rs               # Silence timers
│   │   └── endpoint/
│   │       └── ws.rs               # Phase 1 dev-GUI/TUI WebSocket endpoint
│   ├── build.rs                    # tonic-build proto compilation
│   └── Cargo.toml
├── models/                         # Model weight storage (gitignored)
│   └── .gitkeep
├── proto/
│   ├── buf.yaml
│   ├── buf.gen.yaml
│   └── kaguya/
│       └── v1/
│           └── kaguya.proto        # Single source of truth — Section 3 above
├── reasoner/                       # TypeScript — OpenClaw adapter
│   ├── src/
│   │   ├── index.ts                # Entry point
│   │   ├── adapter.ts              # ReasonerAdapter interface
│   │   ├── openclaw.ts             # OpenClaw adapter impl
│   │   └── grpc_client.ts          # ReasonerService gRPC client → Gateway
│   ├── proto/                      # Generated stubs — gitignored
│   ├── package.json
│   └── tsconfig.json
├── scripts/
│   ├── dev.sh                      # Start all processes locally
│   └── generate_protos.sh          # buf generate + cargo build
├── talker/                         # Python — asyncio, RealtimeSTT, RealtimeTTS
│   ├── src/
│   │   ├── voice/
│   │   │   ├── __init__.py
│   │   │   ├── listener.py         # RealtimeSTT wrapper + ListenerService gRPC client → Gateway
│   │   │   ├── opus_decoder.py     # Opus → 16kHz PCM decoder (opuslib wrapper, ~20 lines)
│   │   │   ├── turn_detection.py   # Rule-based: silence + syntactic completeness
│   │   │   └── speaker.py          # RealtimeTTS wrapper: Kokoro + playback tracking
│   │   ├── brain/
│   │   │   ├── __init__.py
│   │   │   ├── llm_client.py       # Async HTTP client → llama.cpp
│   │   │   ├── prompt_formatter.py # TalkerContext + PersonaConfig → Qwen3 prompt string
│   │   │   ├── soul_container.py   # Tag extraction, normalization, spoken/action split
│   │   │   └── sentence_detector.py # Token buffer → sentence boundaries (regex)
│   │   ├── server.py               # TalkerServiceServicer — wires Gateway ↔ brain ↔ voice
│   │   ├── prepare.py              # PREPARE handler — crosses voice + brain
│   │   ├── config.py               # pydantic BaseSettings
│   │   └── main.py                 # asyncio entrypoint: start gRPC server + Listener task
│   ├── proto/                      # Generated stubs — gitignored
│   ├── tests/
│   └── pyproject.toml
├── tools/                          # TypeScript — Toolkit (sandboxed tool execution)
│   ├── src/
│   │   ├── index.ts
│   │   ├── registry.ts             # Tool registry: name → ToolFn
│   │   ├── sandbox.ts              # Workspace root enforcement
│   │   └── tools/
│   │       ├── web_fetch.ts
│   │       ├── file_ops.ts         # read_file, write_file, list_files
│   │       └── search_tools.ts     # Tool discovery (keyword stub for Phase 1)
│   ├── package.json
│   └── tsconfig.json
├── Makefile
└── README.md
```

### Notes on structure

- `config/` holds the three persona/memory files. Gateway reads these at startup and watches for changes.
- `models/` is gitignored. Populate manually with GGUF model weights.
- `proto/` contains only hand-written `.proto` sources. The `proto/` dirs inside `talker/`, `reasoner/`, and the Rust stubs in `gateway/src/` are all generated artifacts — gitignored, rebuilt by `make proto`.
- `gateway/src/endpoint/` is isolated as a subdirectory because it will be replaced wholesale in Phase 2 (WebSocket → OpenPod). All other Gateway code remains unchanged.
- `talker/src/` uses snake_case packages per PEP 8 convention for Python.
- `voice/` and `brain/` reflect the conceptual split: `voice/` owns all audio I/O (both directions), `brain/` owns all cognition. `server.py` and `prepare.py` are at root because they are cross-cutting coordinators that touch both.
- **IPC file organization (per-process):** Industry standard (grpc.io tutorials, production Python gRPC projects) is separate files for server and client roles. In the Talker Agent: `server.py` is the gRPC server (TalkerService — Gateway calls us). The gRPC client (ListenerService — we call Gateway) is embedded directly in `voice/listener.py` rather than a separate `client.py` because it is tightly coupled to listener logic and not shared by any other component. If a process has multiple independent gRPC clients, extract them to `ipc/client.py`. For the Gateway (Rust): server and client roles are in separate modules under `services/` per the directory structure above.
