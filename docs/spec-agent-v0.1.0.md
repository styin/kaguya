# spec-agent-v0.1.0.md

# Project Kaguya — Agent Specification (Listener + Talker)

**Components:** Listener Agent, Talker Agent
**Version:** 0.1.0
**Updated:** 2026-10-28
**Audience:** Developers working on the Listener and Talker modules

---

## 1. Process Layout

The Listener and Talker share a single Python process to avoid GPU context switching. They run as separate `asyncio` tasks within the same process.

```
┌───────────────────────────────────────────────────────────────┐
│  Process 2: Listener + Talker (Python)                         │
│  - Shared GPU context (faster-whisper + Kokoro on same GPU)    │
│  - Listener: RealtimeSTT (VAD + STT + turn detection)          │
│  - Talker: RealtimeTTS (Kokoro) + LLM HTTP client              │
│  - Inline barge-in handling, sentence boundary detection       │
│  - gRPC servers ← Gateway clients                             │
│  - HTTP client → llama.cpp server                              │
└───────────────────────────────────────────────────────────────┘
```

Both run as persistent `asyncio` tasks.

**Current deployment scope:** One Gateway serves one instance and one selected
session. Its dedicated Supervisor/voice services accept that same fixed pair;
there is no instance/session routing or multiplexing. Shared workers and
concurrent sessions remain a future architecture.

Before loading voice dependencies or opening ports, voice startup requires and
validates `KAGUYA_INSTANCE_ID` and `KAGUYA_SESSION_ID`. Managed and Console
launchers derive them from the selected manifest; standalone startup must supply
them explicitly. Talker, Listener gRPC, and raw audio ingress share one immutable
`InstanceSessionBinding`. Requests assert this configured pair; the first RPC
cannot claim or rebind the process. The voice process never reads the manifest.

The canonical [protobuf schema](../proto/kaguya/v1/kaguya.proto) defines
`SessionRef { InstanceId instance_id, SessionId session_id }`; each named ID
message holds a `string value`. Receivers require both objects and validate
canonical, non-nil, distinct UUIDs before comparing the pair. Rust Gateway and
Supervisor use their generated `proto::SessionRef` internally, with small local
validation helpers; the immutable Python `InstanceSessionBinding` remains the
voice process's configured pair. Identity agreement is not caller
authentication. Hosted authorization remains deferred.


---

## 2. The Listener

### 2.1 Role and Mandate

The Listener is a lightweight, always-on audio sensor. It converts audio to text events. It does not make decisions about what to do with speech.

**The Listener does NOT:**

- Perform LLM inference or decision-making.
- Produce TTS or audio output.

### 2.2 Responsibilities

- Receive Opus-encoded audio frames from the local endpoint. **Phase 1:** frames arrive from the dev-GUI/TUI via the Gateway's local interface. **Phase 2:** frames arrive via OpenPod (forwarded by the Gateway unchanged in either case — the Listener has no awareness of the transport).
- Decode Opus frames to PCM audio.
- Run Voice Activity Detection (Silero VAD) to detect speech onset and offset. Emit `vad_speech_start` and `vad_speech_end` events to the Gateway via gRPC.
- Run streaming STT (faster-whisper) on speech-containing audio to produce partial and final transcripts.
- Perform turn detection: distinguish mid-sentence pauses from genuine end-of-turn using silence duration, syntactic completeness, and optionally a learned turn detection model (Phase 2).
- Emit structured events to the Gateway via gRPC: `vad_speech_start`, `vad_speech_end`, `partial_transcript`, `final_transcript`.

### 2.3 VAD and Turn Detection — Two Layers

Both layers run inside the Listener, solving different problems at different timescales.

**VAD** answers: "Is there sound resembling speech right now?" Binary signal operating on ~30ms audio windows.

- Fires `vad_speech_start` when voice energy appears.
- Fires `vad_speech_end` after ~200-300ms of silence.
- Gates STT processing (faster-whisper only runs on speech-containing frames, saving GPU cycles).
- Provides immediate state signals to the Gateway for inline barge-in and silence timer cancellation.

**Turn detection** answers: "Has the user finished their conversational turn?" Uses richer signals:

- STT transcript content (syntactic completeness).
- Accumulated silence duration.
- Optionally a learned turn detection model (Phase 2).
- Only fires `final_transcript` when a genuine end-of-turn is detected.

### 2.4 Turn Detection — Phase 1 (Rule-Based)

Custom turn detection using silence duration and syntactic analysis. The implementation combines silence tracking with syntactic completeness checks on the current STT buffer.

- Silence < 300ms: likely mid-sentence pause. Continue accumulating.
- Silence 300-800ms with incomplete syntax (no terminal punctuation, dangling preposition, open clause): likely still speaking. Wait.
- Silence 300-800ms with complete syntax (terminal punctuation, syntactically closed): emit `final_transcript`.
- Silence > 800ms regardless of syntax: emit `final_transcript`.

Thresholds are configurable per deployment environment.

A learned turn detector remains a candidate for future evaluation (REF-004). No provider accuracy figure is a Kaguya acceptance result.

### 2.5 Example Event Flow

```
User: "Can you check the... [300ms pause] ...Goedel pipeline status?"

Listener:
  Receives all audio frames (including silence) from OpenPod via Gateway
  VAD detects speech onset → emits vad_speech_start [P2]
  STT processes frames: "Can you check the..."
  VAD detects 300ms silence → emits vad_speech_end [P2]
  Turn detection: incomplete syntax → does NOT emit final_transcript, waits
  VAD detects speech resumes → emits vad_speech_start [P2]
  STT continues: "...Goedel pipeline status"
  VAD detects longer silence → emits vad_speech_end [P2]
  Turn detection: complete sentence + sustained silence → emits final_transcript [P1]
```

Gateway Input Stream (resulting events):

```
14:30:01.000  vad_speech_start                                  [P2]
14:30:01.200  partial_transcript: "Can you check the"           [P2]
14:30:01.500  vad_speech_end                                    [P2]  ← mid-sentence pause
14:30:01.700  vad_speech_start                                  [P2]  ← user resumes
14:30:02.100  vad_speech_end                                    [P2]  ← real end
14:30:02.400  final_transcript: "Can you check the Goedel..."  [P1]  ← turn detected
```

The Gateway uses `vad_speech_start/end` for inline barge-in and silence timer cancellation. It uses `partial_transcript` for speculative prefill (Phase 2). It uses `final_transcript` as the trigger to assemble a context package and invoke the Talker.

### 2.6 Why VAD Lives in the Listener (Not in the Transport Layer)

The transport layer (dev-GUI/TUI in Phase 1, OpenPod in Phase 2) is task-agnostic. VAD is content-specific AI inference that understands what speech is — placing it in the transport layer would violate transport-layer agnosticism. The transport streams all audio frames (including silence) as a dumb pipe; the Listener handles all speech-awareness logic.

The bandwidth cost of streaming silence is ~4KB/s at voice-quality Opus bitrates — negligible on a local interface. The endpoint application may optionally pre-filter audio with its own VAD as a bandwidth optimization, but Kaguya's Listener always runs its own VAD regardless.

### 2.7 Implementation — RealtimeSTT

The Listener is built on **RealtimeSTT** (KoljaB/RealtimeSTT).

- Dual-layer VAD: WebRTC VAD for initial detection, Silero VAD for accurate verification.
- faster-whisper integration for GPU-accelerated streaming transcription.
- Configurable silence thresholds and recording parameters.
- Callback-based architecture: `on_vad_detect_start`, `on_vad_detect_stop`, `on_realtime_transcription_update`, `on_transcription_complete`.

RealtimeSTT handles the audio frame → VAD → STT → partial/final transcript pipeline. The Listener wraps RealtimeSTT's callbacks, translating them into gRPC events sent to the Gateway, and adding the custom turn detection logic on top.

- Language: Python.
- Runs as a persistent `asyncio` task within Process 2 (shared with Talker).
- Exposes a gRPC server (`ListenerService.Stream`); the Gateway connects as the client. Audio bytes do not flow over gRPC — see §2.9.

### 2.8 Component Selection (STT + VAD)

Current defaults use faster-whisper for STT, RealtimeSTT's VAD integration and
Kaguya's rule-based turn detector. Configuration owns model/device selections
and timing thresholds. Recognition quality, memory use and endpointing accuracy
need evaluation on the selected host and speech corpus; no provider benchmark
is adopted as a Kaguya acceptance result.

Planned cloud fallback (STT): Deepgram Nova-3; no adapter is implemented.

### 2.9 IPC: Gateway ↔ Listener

The Listener is a gRPC **server**; the Gateway is the client. Audio bytes do not flow over gRPC — they ride a separate raw TCP socket so that 50fps Opus frames never touch protobuf serialization (architecture invariant: "Audio bytes never enter protobuf serialization at 50fps").

```protobuf
service ListenerService {
  // First input establishes scope; later input carries Listener controls.
  rpc Stream(stream ListenerInput) returns (stream ListenerOutput);
}
```

The first `ListenerInput` must be `init: ListenerInit { session: SessionRef }`.
Gateway queues it before awaiting response headers. Listener validates the
configured scope before accepting the subscription or replacing its prior
subscriber; missing, invalid or mismatched scope fails. Gateway opens the
separate audio connection after the subscription is accepted.

Later inputs use `ListenerInput.control`. `ListenerControl` retains a typed
`reset` variant as an extension point, but reset handling is not implemented:
a valid reset request terminates the RPC with gRPC `UNIMPLEMENTED`. Empty inputs
or controls and repeated initialization are invalid. Closing only the request
side leaves the ASR subscription active; cancellation closes it. Future pause
and reconfiguration controls can extend the same `oneof` once their semantics
are defined; they are not currently declared or implemented.

`ListenerOutput` carries `vad_speech_start`, `vad_speech_end`,
`partial_transcript`, or `final_transcript`; events inherit the validated stream
scope. Audio remains out-of-band: Gateway writes a 39-byte connection prefix
(`KAGUYA1`, 16-byte binary InstanceId, 16-byte binary SessionId), then
`[u32 BE length][bytes]` frames. Audio ingress checks the prefix against the
startup pair before processing frames. It does not lazily establish ownership;
audio bytes never enter protobuf.

---

## 3. The Talker Agent

### 3.1 Role and Mandate

The Talker Agent is Kaguya's voice. It owns the fast-path LLM and TTS. It is the embodiment of Axiom I (Persona Fidelity) and Axiom II (Orchestration & Delegation).

**The Talker does NOT own:**

- Conversation history management (owned by Gateway).
- Memory retrieval (the Gateway runs the RAG engine — SQLite + FTS5 BM25 + optional vector embeddings — and injects retrieved entries into the context package; see spec-gateway §4).
- Tool execution (Talker emits `[TOOL:...]` tags; Gateway dispatches to Toolkit).
- Filesystem access (Talker receives all configuration and context via gRPC from Gateway).

**The Talker is stateless with respect to conversation history and application
tasks.** Each generation receives its prompt snapshot and triggering event from
Gateway. Persona is configuration cached through `UpdatePersona`; Talker does
not own durable conversation state or load instance data itself.

### 3.2 Responsibilities

- Receive `GenerationStart` with the fixed `SessionRef`, `generation_id`, timestamp, reusable `PromptContext`, and exactly one typed trigger.
- Receive persona configuration (`SOUL.md`, `IDENTITY.md`, and a synthesized `memory_md` produced by the Gateway from its RAG store) from the Gateway at startup and on change via scoped `UpdatePersonaRequest { session, persona }` at startup/reconnect and on change. Cache all three in memory. Never access the filesystem directly.
- Format the final LLM prompt string from the context package + cached persona config.
- Run streaming LLM inference on the fast-path local model (Qwen3-8B via llama.cpp HTTP).
- Detect sentence boundaries in the token stream.
- Post-process LLM output through the soul container.
- Stream spoken text to local TTS playback. Endpoint speech transport remains R7 work.
- Send post-process metadata to the Gateway via gRPC: transcript text, emotion tags, tool requests, delegation requests, response completion signal.
- Prefill from `PrefillRequest { session, prompt_context }`; prefill has no synthetic generation ID or trigger.

### 3.3 Prompt Structure

```
[System: SOUL.md + IDENTITY.md persona instructions]
[System: Structured output instructions for tags:
  - [EMOTION:joy|concern|thinking|surprise|neutral|determined]
  - [TOOL:tool_name(param1, param2)]
  - [DELEGATE:task_description]
  - [PROGRAM: ... multi-step tool script ... ]]
[System: Available tools:
  - search_tools(query): Find tools matching a description
  - web_fetch(url): Fetch content from a URL
  - write_file(path, content): Write content to a file
  - (additional tools from Toolkit registry)]
[System: Tool use examples:
  - User asks about a URL → [TOOL:web_fetch("https://...")]
  - User asks to save something → [TOOL:write_file("/path", "content")]
  - User asks something requiring tool discovery → [TOOL:search_tools("query")]
  - User asks a complex multi-step task → [DELEGATE:description]]
[System: Current context — time, active tasks, recent history summary]
[Memory: synthesized memory_md — user profile + project context + recent semantic memories, sourced from the Gateway's RAG store]
[Retrieval: top-k entries returned from this turn's hybrid query (BM25 + optional vector), each tagged with its source]
[Conversation: Recent turns]
[User: {final_transcript}]
```

Gateway assembles `PromptContext`: recent `ChatMessage` history,
`memory_contents`, tool definitions, typed `ActiveTask { task_id, description }`
records, and retrieval results. `GenerationStart` adds one of:

- `UserInput { text }`;
- `ToolResult { request_id, content }`;
- `ReasonerResult { task_id, summary }`;
- `ReasonerProgress { task_id, description }`;
- `ReasonerFailure { task_id, message, code }`;
- `SilenceTrigger { duration_ms }`.

A result reference and its content stay together. Tool results consistently use
`request_id`; Reasoner progress/results/failures use the existing `task_id`. They do not
introduce new task/run identities. Prompt formatting remains Talker's concern;
typed progress and silence retain the existing rendered prompt cues.

Persona remains cached configuration delivered through `UpdatePersonaRequest`.
It is not folded into `PromptContext` or retransmitted as a `PersonaConfig` on
every generation. Existing memory/history inputs remain in the prompt snapshot;
Talker never reads persona files directly.

### 3.4 The Soul Container (Post-Processing Middleware)

Between LLM output and TTS synthesis, a deterministic post-processing layer enforces persona consistency and validates structured output. Inspired by Project Airi's "soul container" pattern. Operates on complete sentences after boundary detection — not on individual tokens. Pure function: stateless, deterministic, no LLM calls.

```
LLM tokens stream in
    → Token buffer accumulates
    → Sentence boundary detected
    → Complete sentence enters Soul Container:
        1. Normalize emotion tags ([EMOTION:happy] → [EMOTION:joy])
        2. Inject default [EMOTION:neutral] if tag missing
        3. Validate/strip hallucinated action tags
        4. Intercept [TOOL:...] tags → extract, route to tool flow
        5. Intercept [DELEGATE:...] tags → extract, route to Gateway
        6. Apply vocabulary rules from IDENTITY.md
        7. Enforce max response length for voice (~2-4 sentences)
    → Processed sentence (tags stripped) → RealtimeTTS
    → Extracted tags → Gateway (gRPC) for tool execution, delegation, Live2D
```

The soul container splits spoken text from action tags. The user hears the natural language portion; the tags are routed to the Gateway for execution. Example: LLM emits `"Let me check that for you. [TOOL:web_fetch("https://api.pipeline.dev/status")]"` — user hears "Let me check that for you" while the tool call executes in parallel.

### 3.5 Tool Call Flow

```
1. Soul container extracts [TOOL:web_fetch("https://...")] from sentence
2. Spoken text ("Let me check that for you.") → TTS → audio out
3. Simultaneously: Talker sends tool request to Gateway via gRPC
4. Gateway dispatches to its Rust tool registry
   [LLM generation is now complete — it stopped after emitting the tool tag]
5. Toolkit executes web_fetch, returns result
6. Tool result → Input Stream as P3 event → Gateway assembles new context
   package with tool result included → dispatches to Talker
7. Talker starts a fresh LLM inference round (not a resume — new generation)
   with enriched context: [TOOL_RESULT: {"status": "healthy", "last_run": "2h ago"}]
8. LLM generates: "The pipeline is healthy — last run was two hours ago."
9. Soul container processes → TTS → audio out
```

For fast tools (<500ms): user hears acknowledgment, then the answer. For slower tools: Talker can emit filler speech while waiting.

Boundary between tool calls and delegation: can the tool complete in under ~2 seconds? Use `[TOOL:...]`. Otherwise, use `[DELEGATE:...]` to hand off to a Reasoner Agent.

### 3.6 Advanced Tool Use (Phase 2)

**Tool Search Tool.** Instead of listing all available tools in the system prompt (which consumes context window), the system prompt lists only a meta-tool: `search_tools(query)`. The LLM calls this first; the Gateway searches the Toolkit registry via vector similarity on tool descriptions and returns the top 3-5 relevant tools. The Talker injects these into context and the LLM calls the specific tool it needs. Keeps the system prompt small and scales to hundreds of MCP-connected tools.

**Programmatic Tool Calling.** Instead of one tool call per LLM inference round, the LLM generates a small program that chains multiple tool calls. The Gateway executes the program in a sandboxed TypeScript environment; only the final aggregated result comes back. Reduces inference rounds from N to 1 for multi-tool workflows.

**Tool Use Examples.** Few-shot demonstrations in the system prompt showing correct tool call formatting. Present in Phase 1 (see prompt structure above); expanded in Phase 2 with more complex multi-tool examples.

### 3.7 Sentence Boundary Detection (Custom)

LLM tokens accumulate in a buffer. Flushed to TTS when a sentence-ending pattern is detected: `.`, `?`, `!` followed by a space and uppercase letter (or end-of-generation). Edge cases (abbreviations like "Dr.", decimals like "3.14", URLs) handled by regex. See [sentence detector](../talker/inference/sentence_detector.py).

Phase 1: sentence-level boundaries for correct TTS intonation. Phase 2: clause-level exploration.

### 3.8 Barge-In Handling (inline on Converse stream)

Barge-in is delivered as a `BargeInSignal { session }` on the active `TalkerService.Converse` bidi stream — there is no separate `Prepare` RPC. The current voice speech-start path sends it on `vad_speech_start`; text input cancels the prior Gateway dispatch token. Complete text interruption/playback accounting remains acceptance work.

**IF the Talker is currently speaking/generating:**

1. Set the cancel event observed by the LLM streaming loop and the sentence emitter.
2. Stop RealtimeTTS playback (mid-word if necessary).
3. Acknowledge only sentence batches whose playback completed naturally without error; interrupted and queued batches remain unspoken.
4. Emit `BargeInAck { spoken_text, unspoken_text }` back on the same Converse stream. Gateway uses `spoken_text` to append only the spoken portion to conversation history, using the acknowledged split.
5. The current Converse stream completes naturally with `TalkerOutput { generation_id, response_complete: { was_interrupted: true } }`.

**IF the Talker is already idle (no active Converse stream):**

1. No-op. The Gateway's `barge_in()` finds no live stream and logs at debug.

Current accounting acknowledges completed playback batches, so an interrupted
batch can undercount speech already heard. It does not establish exact word or
device-latency accounting; those remain live playback acceptance work.

### 3.9 KV Cache Prefill Strategy

The LLM prompt has two parts: a _stable prefix_ (system prompt + memory + conversation history) that changes only between turns, and a _variable suffix_ (the user's current input) that changes per request. The stable prefix is typically large (~1000-3000 tokens); user input is small (~10-50 tokens).

**Phase 1 — Always-On Prefix Prefill.** After a non-interrupted generation completes, Gateway sends a fresh `PromptContext` through scoped `PrefillCache`. It contains updated history and no `GenerationStart` or trigger. The Talker issues:

```json
{
  "prompt": "[system + memory + history including turn N] User: ",
  "n_predict": 0,
  "cache_prompt": true
}
```

Prefill requests consume provider resources and can overlap TTS or other work.
Their benefit depends on provider cache behavior and prefix equality. Measure
first-token/first-sentence latency and contention on the selected deployment; no
local speedup is established by this specification.

Cache invalidation: if memory context changes between turns (e.g., the post-turn `RagEngine::evaluate_and_store` adds entries that change the synthesized `memory_md`, or a Reasoner completes), the Gateway sends a fresh `UpdatePersona` followed by a new `PrefillCache` call with the updated context.

**Phase 2 — Partial Transcript Prefill.** The Gateway forwards each `partial_transcript` event as an incremental prefill request. The Talker extends the cached prefix:

```json
{
  "prompt": "[cached prefix] User: Can you check the",
  "n_predict": 0,
  "cache_prompt": true
}
```

Each partial extends the cache by a few tokens. When `final_transcript` arrives, the cache already includes most of the user's input.

**Phase 2 — Speculative Decoding.** The Talker can begin generating draft response tokens before the user finishes speaking, betting that intent is already clear from the partial transcript (clause-level: subject + verb minimum). If `final_transcript` confirms the bet, draft tokens are already available. If it diverges, drafts are discarded and regeneration starts from the warm cache.

### 3.10 Talker Egress — What the Talker Sends to the Gateway

| Output | Current destination |
| --- | --- |
| Synthesized speech | Voice-service host audio device; browser transport remains R7 |
| Complete sentence / emotion | Scoped generation output over gRPC, then Console metadata |
| Tool request | Gateway tool dispatch |
| Delegation request | Gateway Reasoner client; the service is unimplemented |
| Response completion | Gateway history/persona/prefill handling |
| `BargeInAck` | Completed-batch `spoken_text` / remaining `unspoken_text` split for Gateway history |

All semantic outputs carry `TalkerOutput.generation_id`. Audio bytes never enter
that protobuf envelope.

### 3.11 Implementation — RealtimeTTS

The Talker's TTS layer is built on **RealtimeTTS** (KoljaB/RealtimeTTS).

The current [speaker](../talker/voice/speaker.py) configures Kokoro, queues
complete sentences, plays on the host device and stops playback on interruption.
It does not configure multiple engines, automatic provider fallback, or browser
audio forwarding. Only naturally completed, error-free playback batches are acknowledged.
Interrupted batches remain unspoken; exact played-word accounting is deferred.

The LLM client uses an HTTP completion endpoint and the Talker exposes its gRPC
service to Gateway. Language-model and voice settings live in
[Talker config](../talker/config.py). Cloud TTS adapters remain planned.

### 3.12 Component Selection (LLM + TTS)

The default LLM is Qwen3-8B via a local completion server; the configured TTS
engine is Kokoro. Model memory use, token rate and first-audio latency depend on
the host and configuration. They require measurement; upstream model benchmarks
are not Kaguya performance guarantees.

### 3.13 No Pipeline Framework Rationale

The Listener and Talker use **component libraries** (RealtimeSTT, RealtimeTTS) rather than a pipeline orchestration framework (Pipecat, LiveKit Agents, etc.). Rationale: Kaguya's architecture splits the voice pipeline across Listener and Talker with the Gateway as an intermediary — a non-standard topology that pipeline frameworks assume they control end-to-end. Using a framework would require fighting its pipeline model. Component libraries impose no topology; they provide building blocks that fit into Kaguya's conductor architecture.

### 3.14 Implementation Boundaries

| Responsibility | Source |
| --- | --- |
| VAD/STT callbacks and raw audio ingress | [Listener](../talker/voice/listener.py) |
| Silence/syntax turn detection | [Turn detector](../talker/voice/turn_detector.py) |
| Generation, cancellation and scoped RPCs | [Talker server](../talker/server.py) |
| Prompt formatting and token stream | [Inference modules](../talker/inference/) |
| Sentence/tag normalization | [Soul container](../talker/inference/soul_container.py) |
| Local TTS and interruption estimate | [Speaker](../talker/voice/speaker.py) |

---

## 4. The Reasoner Agent

**Status:** Planned service. Gateway client and protobuf contracts exist, but
there is no Reasoner runtime or backend adapter. The responsibilities below are
target behavior for R6.

### 4.1 Role and Mandate

The target Reasoner handles slow-path tasks through orchestration frameworks.
Currently Gateway has a gRPC delegation client and volatile task tracking; the
Reasoner service itself remains R6 work. Gateway coordinates requests when Talker
emits a `[DELEGATE:...]` tag; Supervisor owns any managed OS process lifecycle.

**Output flows through Gateway only.** The Reasoner never communicates directly with the Talker or the user.

### 4.2 Responsibilities

- Receive task descriptions from the Gateway (originated from Talker's `[DELEGATE:...]` tags).
- Invoke the underlying framework (OpenClaw, Claude Code, custom agents).
- Monitor framework output (stdout, logs, API responses).
- Emit structured events back to Gateway via gRPC: `reasoner_started`, `reasoner_intermediate_step`, `reasoner_output`, `reasoner_completed`, `reasoner_error`.

### 4.3 Multi-Agent Support

Gateway can track multiple initiated tasks in the selected session, each with a unique `task_id`. This does not imply multiple instance/session runtimes, a delivered Reasoner backend, or durable task recovery; those remain separately staged.

### 4.4 Implementation

- Language: TypeScript (ecosystem compatibility with OpenClaw/Node.js).
- Reasoner is the target gRPC server; Gateway connects as its client.
- Backend task execution and durable task/run lifecycle remain R3/R6 work. Managed processes belong to Supervisor.
- Adapter pattern: swapping frameworks (OpenClaw → Claude Code → custom) requires only a new adapter, not architectural changes.

### 4.5 IPC: Gateway ↔ Reasoner

```protobuf
service ReasonerService {
  // Gateway dispatches a task and receives streamed events; can push
  // additional context mid-task on the same stream.
  rpc Delegate(stream DelegateInput) returns (stream DelegateOutput);
  // Gateway sends interrupt signals (cancel a task, global stop, shutdown).
  rpc Interrupt(InterruptRequest) returns (InterruptAck);
  // Server-streaming background telemetry (deferred — proto only in Phase 1).
  rpc Telemetry(TelemetrySubscribe) returns (stream TelemetryEvent);
}
```

---

## 5. IPC: Agents → Gateway (Full Protocol)

All agent-to-Gateway communication uses gRPC with Protocol Buffers. Audio is the one exception: it rides a raw length-prefixed TCP socket (see §2.9), preserving the architecture invariant that audio bytes never enter protobuf serialization at 50fps.

```protobuf
// Listener: Gateway = client, Listener = server
service ListenerService {
  rpc Stream(stream ListenerInput) returns (stream ListenerOutput);
}

// Talker: Gateway = client, Talker = server
service TalkerService {
  // Bidi: Gateway sends TalkerInput.start (GenerationStart) first, then
  // optionally TalkerInput.barge_in to interrupt. Talker streams back
  // TalkerOutput (sentences, emotion tags, tool requests, barge-in acks,
  // response_complete).
  rpc Converse(stream TalkerInput) returns (stream TalkerOutput);
  // Between-turn: speculative prefix prefill on warm cache.
  rpc PrefillCache(PrefillRequest) returns (PrefillAck);
  // Persona delivery — soul + identity + synthesized memory_md.
  rpc UpdatePersona(UpdatePersonaRequest) returns (PersonaAck);
}

// Reasoner: Gateway = client, Reasoner = server
service ReasonerService {
  rpc Delegate(stream DelegateInput) returns (stream DelegateOutput);
  rpc Interrupt(InterruptRequest) returns (InterruptAck);
  rpc Telemetry(TelemetrySubscribe) returns (stream TelemetryEvent);
}
```

The current fixed-scope protocol uses explicit protobuf `SessionRef`, not
`x-kaguya-instance-id` / `x-kaguya-session-id` metadata. A request asserts the
receiver's configured pair; it does not select another runtime. Invalid or
missing references fail validation, and a different valid pair is rejected
before persona, speaker, Listener state, or control queues are mutated.

| RPC boundary | Association |
| --- | --- |
| Listener `Stream` | First `ListenerInput.init.session`; later inputs are controls and streamed events inherit the fixed scope |
| Talker `Converse` | First `GenerationStart.session` and `generation_id`; only scoped inline barge-in may follow |
| `PrefillCache` | `PrefillRequest.session` plus `PromptContext` |
| `UpdatePersona` | `UpdatePersonaRequest.session` plus cached `PersonaConfig` |
| Reasoner `Delegate` | Initial `TaskRequest.session`/`task_id`; context updates retain both |
| Reasoner `Interrupt` / `Telemetry` | Scoped request/subscription; service implementation remains deferred |
| Gateway `SendControl` | `ControlRequest.session` plus `ControlSignal`; scope validation precedes handling, STOP/SHUTDOWN enter P0, approval returns `UNIMPLEMENTED` |

Converse validates the first message, prompt, and trigger before response headers
and generation side effects. Each RPC serves one generation; another Start on
the same stream is invalid. Every `TalkerOutput` echoes its `generation_id` in
the outer envelope, including sentences, emotions, tool/delegation requests,
and barge-in acknowledgments. `ResponseStarted` is a marker and
`ResponseComplete` carries the interruption outcome; neither duplicates the ID.
Gateway rejects a missing or mismatched envelope ID before forwarding the
generated protobuf directly into the shared queue. This does not establish a
new stale-generation rejection or cancellation policy.

Audio still uses the unchanged raw 39-byte connection prefix followed by
length-prefixed frames. The immutable `InstanceSessionBinding` in
`talker/binding.py` checks that prefix against the startup pair. Persona
identity content is separate from instance/session identity. Caller
authentication/tracing and multiplexed worker routing remain deferred.

---

## 6. Planning VRAM Estimate (16 GB host)

These approximate allocations are sizing assumptions, not recorded measurements
or a supported hardware guarantee.

| Component                 | VRAM         |
| ------------------------- | ------------ |
| LLM (Qwen3-8B Q4)         | ~5-6 GB      |
| STT (faster-whisper INT8) | ~1 GB        |
| TTS (Kokoro-82M)          | ~0.5 GB      |
| KV cache + overhead       | ~2-3 GB      |
| **Total**                 | **~9-11 GB** |
| **Headroom**              | **~5-7 GB**  |

---

## 7. Latency Validation

End-to-end latency remains an acceptance measurement. Record VAD/STT timing,
Gateway dispatch, LLM first sentence, synthesis and actual playback separately.
Browser playback and a first-audio metric require a future Talker audio transport. Partial
prefill, OpenPod transport and speculative decoding are deferred; they cannot be
included in a measured current-path latency budget. See R8 in the
[implementation plan](implementation-plan-v0.1.0.md).

---

## 8. Persona System

### 8.1 Phase 1: System Prompt Steering + File-Based Memory

Persona enforced via `SOUL.md` + `IDENTITY.md` loaded by the Gateway at startup and delivered to the Talker via `UpdatePersona` gRPC. The Gateway's RAG engine synthesizes a `memory_md` document (user profile + project context + recent semantic memories from its SQLite store) and bundles it into the same `UpdatePersona` call; the Talker caches all three alongside the persona files. The Talker injects the cached values into every LLM prompt; Gateway re-sends them on reconnect and actual changes, not in every `GenerationStart`. Action tags (`[EMOTION:...]`, `[DELEGATE:...]`, `[TOOL:...]`) instructed in the system prompt.

When the Gateway's post-turn `RagEngine::evaluate_and_store` adds entries that change the synthesized `memory_md`, it sends a fresh `UpdatePersona` call to the Talker with the new content. The Talker replaces its cached copy. The Talker never reads or writes any file directly.

### 8.2 Phase 2: QLoRA Fine-Tuning

When system-prompt steering hits its ceiling (inconsistent tags, persona drift):

- Base: Qwen3-8B. Adapter: QLoRA rank 16-64, ~50-200MB.
- Data: 5-10K synthetic conversations from frontier model role-playing as Kaguya.
- Training: 12-16GB VRAM, fits on RTX 5070 Ti.

### 8.3 Phase 2: Custom TTS Voice

Phase 1: Select one of Kokoro's 14 built-in voices. Phase 2 options:

- **Chatterbox** (MIT): 0-shot cloning from 5-10s reference.
- **Qwen3-TTS:** Seamless Qwen LLM integration.
- **Full fine-tuning:** 1-5 hours of target voice audio. Maximum control.

---

## 9. Audio Modality and Text-Only Mode

Audio can be disabled. In text-only mode: Listener is inactive, Talker returns text (skips TTS), Gateway event loop works identically. Silence timers function based on absence of `text_command` events. Simultaneous text + audio: typed text preempts voice (Phase 1 simplification).

---

## 10. Phased Delivery

### Phase 1 Scope (implementation and acceptance tracked in R0-R8)

- Listener: RealtimeSTT (faster-whisper + Silero VAD) with custom rule-based turn detection.
- Talker: Qwen3-8B via llama.cpp + RealtimeTTS with Kokoro, sentence-level streaming.
- Soul container post-processing (tag normalization, validation, interception).
- Tool call flow: LLM emits [TOOL:...] → soul container intercepts → gRPC to Gateway → result → new inference round.
- Inline barge-in handling with completed-batch accounting; playback validation remains open.
- Always-on prefix KV cache prefill (system + memory + history pre-warmed between turns).
- Planned Reasoner adapter; backend choice and execution binding remain R6.
- Persona + memory enforcement via system prompt (`SOUL.md` + `IDENTITY.md` + RAG-synthesized `memory_md` delivered by Gateway via `UpdatePersona`).
- Text-only fallback mode.
- Basic Deliberative Narration (acknowledge + wait + summarize).
- Tool use examples in system prompt.
- **Audio source: dev-GUI/TUI local endpoint (no OpenPod dependency).**

### Phase 2 Deliverables (Agent scope)

- **OpenPod audio integration** — Listener receives Opus frames via OpenPod (forwarded by Gateway); Talker sends audio to Gateway for OpenPod Channel D mux. The raw audio contract and missing TTS egress must be specified and implemented before claiming endpoint interchangeability.
- Partial transcript prefill (word-level KV cache extension on partials).
- Speculative decoding (clause-level draft generation before final transcript).
- Two-stage barge-in (soft fade on `vad_speech_start`, hard stop on first `partial_transcript`).
- Tool Search Tool for large registries (MCP server scaling).
- Programmatic Tool Calling (multi-tool scripts).
- QLoRA-tuned LLM with consistent action tags.
- Custom TTS voice (Chatterbox/Qwen3-TTS).
- Half-cascading exploration (Audio Encoder → Text LLM → TTS).
- Learned turn detection model.
- Full Deliberative Narration with filtered intermediate narration.
- Pre-emptive RAG on partial transcripts (Gateway-triggered, Talker handles prefill).
- TTS provider abstraction (unified Python protocol for Kokoro, Chatterbox, cloud fallbacks).

### Phase 3 Deliverables (Agent scope)

- Speaker diarization (pyannote.audio).
- Addressee detection (LLM-inferred from context + name detection in STT).
- Multi-party system prompt tuning.
- End-to-end speech-to-speech exploration (Moshi successors with tool-use).

---

## 11. Open Questions (Agent-Relevant)

- **GPU contention profiling.** VRAM budget fits on paper. Actual compute contention needs empirical benchmarking (faster-whisper + llama.cpp + Kokoro concurrent on RTX 5070 Ti).
- **Persona voice selection.** Which of Kokoro's 14 voices best embodies Kaguya? Requires listening tests.
- **Narration cadence.** How often to narrate over Reasoner steps? Needs user testing.
- **Listener VAD sensitivity tuning.** Silero VAD parameters (sensitivity, silence thresholds) need tuning per deployment environment (quiet office vs. noisy). May need user-configurable profiles. Also: verify that streaming silence frames from endpoint to Listener (~4KB/s) causes no issues on target network configurations.
- **LLM action tag reliability.** How consistently does the 8B model emit `[EMOTION:...]` and `[DELEGATE:...]` via system prompt? If <95%, accelerates QLoRA case.
- **RealtimeSTT/TTS integration depth.** Evaluate whether RealtimeSTT's built-in sentence detection and RealtimeTTS's built-in streaming are sufficient for Phase 1, or whether custom overrides are needed from day one. KoljaB/RealtimeVoiceChat serves as the reference integration to benchmark against.
- **Speculative prefill invalidation.** Strategy when user's meaning reverses at end of utterance ("I want to... actually never mind"). How to efficiently discard/rebuild KV cache.
