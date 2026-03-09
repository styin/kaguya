# spec-agent-v0.1.0.md

# Project Kaguya — Agent Specification (Listener + Talker)

**Components:** Listener Agent, Talker Agent
**Version:** 0.1.0
**Date:** March 2026
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
│  - Custom PREPARE signal handling, sentence boundary detection  │
│  - gRPC client → Gateway                                       │
│  - HTTP client → llama.cpp server                              │
└───────────────────────────────────────────────────────────────┘
```

Both run as persistent `asyncio` tasks. Custom code total: ~700-1150 lines of Python.

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
- Provides immediate state signals to the Gateway for PREPARE dispatch and silence timer cancellation.

**Turn detection** answers: "Has the user finished their conversational turn?" Uses richer signals:

- STT transcript content (syntactic completeness).
- Accumulated silence duration.
- Optionally a learned turn detection model (Phase 2).
- Only fires `final_transcript` when a genuine end-of-turn is detected.

### 2.4 Turn Detection — Phase 1 (Rule-Based)

Custom turn detection using silence duration and syntactic analysis. Implementation: ~50-100 lines of Python combining silence duration tracking with regex-based syntactic completeness checks on the current STT buffer.

- Silence < 300ms: likely mid-sentence pause. Continue accumulating.
- Silence 300-800ms with incomplete syntax (no terminal punctuation, dangling preposition, open clause): likely still speaking. Wait.
- Silence 300-800ms with complete syntax (terminal punctuation, syntactically closed): emit `final_transcript`.
- Silence > 800ms regardless of syntax: emit `final_transcript`.

Thresholds are configurable per deployment environment.

Phase 2 replaces rule-based thresholds with a learned turn detection model (e.g., LiveKit's open-weights model: 85% true positive, 97% true negative for end-of-turn).

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

The Gateway uses `vad_speech_start/end` for PREPARE signal dispatch and silence timer cancellation. It uses `partial_transcript` for speculative prefill (Phase 2). It uses `final_transcript` as the trigger to assemble a context package and invoke the Talker.

### 2.6 Why VAD Lives in the Listener (Not in the Transport Layer)

The transport layer (dev-GUI/TUI in Phase 1, OpenPod in Phase 2) is task-agnostic. VAD is content-specific AI inference that understands what speech is — placing it in the transport layer would violate transport-layer agnosticism. The transport streams all audio frames (including silence) as a dumb pipe; the Listener handles all speech-awareness logic.

The bandwidth cost of streaming silence is ~4KB/s at voice-quality Opus bitrates — negligible on a local interface. The endpoint application may optionally pre-filter audio with its own VAD as a bandwidth optimization, but Kaguya's Listener always runs its own VAD regardless.

### 2.7 Implementation — RealtimeSTT

The Listener is built on **RealtimeSTT** (KoljaB/RealtimeSTT).

- Dual-layer VAD: WebRTC VAD for initial detection, Silero VAD for accurate verification.
- faster-whisper integration for GPU-accelerated streaming transcription.
- Configurable silence thresholds and recording parameters.
- Callback-based architecture: `on_vad_detect_start`, `on_vad_detect_stop`, `on_realtime_transcription_update`, `on_transcription_complete`.
- Wake word detection support (Porcupine, OpenWakeWord) for optional "Hey Kaguya" activation.

RealtimeSTT handles the audio frame → VAD → STT → partial/final transcript pipeline. The Listener wraps RealtimeSTT's callbacks, translating them into gRPC events sent to the Gateway, and adding the custom turn detection logic on top.

- Language: Python.
- Runs as a persistent `asyncio` task within Process 2 (shared with Talker).
- Exposes events to the Gateway via gRPC streaming (`ListenerService.StreamEvents`).
- ~200-300 lines of custom code wrapping RealtimeSTT callbacks + turn detection logic.

### 2.8 Component Selection (STT + VAD)

| Component       | Selection                              | VRAM     | Key Metric              | Rationale                                                 |
| --------------- | -------------------------------------- | -------- | ----------------------- | --------------------------------------------------------- |
| **STT**         | faster-whisper (distil-large-v3, INT8) | ~1 GB    | WER 6-7%, RTF ~0.3×     | Community standard, CTranslate2 backend, streams partials |
| **VAD**         | Silero VAD                             | CPU only | ~30-50ms onset          | Endpoint-side (ONNX) + Listener-side                      |
| **Turn detect** | Rule-based (Phase 1)                   | N/A      | Configurable thresholds | LiveKit model (85% TP, 97% TN) for Phase 2                |

Cloud fallback (STT): Deepgram Nova-3.

### 2.9 IPC: Listener → Gateway

```protobuf
service ListenerService {
  rpc StreamEvents(stream ListenerEvent) returns (ListenerAck);
}
```

Events streamed via gRPC over Unix domain socket. Each `ListenerEvent` carries one of: `vad_speech_start`, `vad_speech_end`, `partial_transcript`, `final_transcript`.

---

## 3. The Talker Agent

### 3.1 Role and Mandate

The Talker Agent is Kaguya's voice. It owns the fast-path LLM and TTS. It is the embodiment of Axiom I (Persona Fidelity) and Axiom II (Orchestration & Delegation).

**The Talker does NOT own:**

- Conversation history management (owned by Gateway).
- Memory retrieval (Gateway reads `MEMORY.md` and includes its contents in the context package; ChromaDB in Phase 2).
- Tool execution (Talker emits `[TOOL:...]` tags; Gateway dispatches to Toolkit).
- Filesystem access (Talker receives all configuration and context via gRPC from Gateway).

**The Talker is stateless.** It receives all context via gRPC from the Gateway every turn.

### 3.2 Responsibilities

- Receive context packages from the Gateway containing: user input, memory fragments, conversation history, active tasks, tool list, tool results, and current metadata.
- Receive persona configuration (`SOUL.md` + `IDENTITY.md` + `MEMORY.md`) from the Gateway at startup and on change via `UpdatePersona` gRPC call. Cache all three in memory. Never access the filesystem directly.
- Format the final LLM prompt string from the context package + cached persona config.
- Run streaming LLM inference on the fast-path local model (Qwen3-8B via llama.cpp HTTP).
- Detect sentence boundaries in the token stream.
- Post-process LLM output through the soul container.
- Stream spoken text to TTS for synthesis → audio out to OpenPod (Channel D, via Gateway mux).
- Send post-process metadata to the Gateway via gRPC: transcript text, emotion tags, tool requests, delegation requests, response completion signal.
- Trigger always-on prefix KV cache prefill when the Gateway sends a context package marked as prefill-only (`PrefillCache` gRPC call).

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
[Memory: MEMORY.md contents — user profile, project facts, long-term context]
[Conversation: Recent turns]
[User: {final_transcript}]
```

The Gateway assembles the context package including `MEMORY.md` contents; the Talker formats this into the above prompt structure. The Gateway has no knowledge of the prompt format. The Talker caches `MEMORY.md` content in memory (received via `UpdatePersona`); it does not read any file directly.

### 3.4 The Soul Container (Post-Processing Middleware)

Between LLM output and TTS synthesis, a deterministic post-processing layer enforces persona consistency and validates structured output. Inspired by Project Airi's "soul container" pattern. Operates on complete sentences after boundary detection — not on individual tokens. Pure function: stateless, deterministic, no LLM calls. ~50-100 lines of Python.

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
4. Gateway dispatches to TypeScript Toolkit
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

LLM tokens accumulate in a buffer. Flushed to TTS when a sentence-ending pattern is detected: `.`, `?`, `!` followed by a space and uppercase letter (or end-of-generation). Edge cases (abbreviations like "Dr.", decimals like "3.14", URLs) handled by regex. Implementation: ~50-80 lines of Python.

Phase 1: sentence-level boundaries for correct TTS intonation. Phase 2: clause-level exploration.

### 3.8 PREPARE Signal Handling (Custom)

Received via `TalkerService.Prepare` gRPC call from the Gateway on every `vad_speech_start` or `text_command`.

**IF the Talker is currently speaking/generating:**

1. Immediately stop RealtimeTTS playback (mid-word if necessary).
2. Cancel any in-flight LLM generation.
3. Record which text was already spoken vs. still pending.
4. Send `partial_response` metadata to Gateway (spoken text + unspoken text). Gateway uses this to append only the spoken portion to conversation history.
5. Talker is now idle, waiting for the next context package.

**IF the Talker is already idle:**

1. No-op. Talker is already ready (KV cache warm from prefix prefill).

Implementation: ~100-150 lines of Python, tracking playback position against the LLM token stream. RealtimeTTS supports playback interruption natively, which simplifies the audio cutoff. Token accounting — knowing exactly which sentence was playing when PREPARE arrived — is the harder part.

### 3.9 KV Cache Prefill Strategy

The LLM prompt has two parts: a _stable prefix_ (system prompt + memory + conversation history) that changes only between turns, and a _variable suffix_ (the user's current input) that changes per request. The stable prefix is typically large (~1000-3000 tokens); user input is small (~10-50 tokens).

**Phase 1 — Always-On Prefix Prefill.** Immediately after the Talker finishes generating a response for turn N, the Gateway sends an updated context package via `PrefillCache` gRPC call. The Talker issues:

```json
{
  "prompt": "[system + memory + history including turn N] User: ",
  "n_predict": 0,
  "cache_prompt": true
}
```

GPU is idle between turns — this costs nothing. When `final_transcript` arrives, the Talker sends the complete prompt — llama.cpp detects the cached prefix, processes only the user's ~10-50 new tokens, and begins generation almost immediately. Reduces first-token latency from ~200-400ms to ~50-100ms.

Cache invalidation: if memory context changes between turns (e.g., a Reasoner completes and the Gateway appends new facts to `MEMORY.md` and sends `UpdatePersona`), the Gateway signals the Talker to re-trigger prefix prefill via a new `PrefillCache` call with updated context.

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

```
Talker post-process produces:
  → Spoken audio          → stays in Talker → TTS → endpoint (Gateway mux)
  → Transcript text       → gRPC → Gateway → endpoint display
  → Emotion tags          → gRPC → Gateway → endpoint display
  → [TOOL:...] request    → gRPC → Gateway → tool dispatch
  → [DELEGATE:...] req    → gRPC → Gateway → Reasoner spin-up
  → Response complete     → gRPC → Gateway (triggers prefix prefill, history append)
  → partial_response      → gRPC → Gateway on PREPARE (if was mid-speech):
                             { spoken_text: string, unspoken_text: string }
                             Gateway appends spoken_text to history only;
                             unspoken_text is discarded
```

### 3.11 Implementation — RealtimeTTS

The Talker's TTS layer is built on **RealtimeTTS** (KoljaB/RealtimeTTS).

- Multiple TTS engine support: Kokoro (primary), Coqui/XTTS, Orpheus, Piper, ElevenLabs, Edge TTS.
- Streams audio output as it is generated — does not wait for full text before starting synthesis.
- Built-in sentence boundary detection (used as a baseline; custom detection can override).
- Playback interruption support for PREPARE signal handling.
- Engine fallback: automatically switches to alternative TTS engines if the primary encounters errors.

The Talker wraps RealtimeTTS, feeding it complete sentences from the LLM token stream. RealtimeTTS handles TTS inference and audio streaming. Custom code handles the LLM → sentence boundary detection → RealtimeTTS feed pipeline and PREPARE signal token accounting.

- Language: Python. Shares process with Listener for GPU context sharing.
- LLM: HTTP client to local llama.cpp server (OpenAI-compatible completions API, localhost, ~0.1ms overhead).
- TTS: RealtimeTTS with Kokoro engine (in-process GPU inference).
- gRPC client connecting to Gateway.

Cloud fallback (TTS): ElevenLabs Flash.

### 3.12 Component Selection (LLM + TTS)

| Component | Selection                 | VRAM    | Key Metric      | Rationale                                             |
| --------- | ------------------------- | ------- | --------------- | ----------------------------------------------------- |
| **LLM**   | llama.cpp + Qwen3-8B (Q4) | ~5-6 GB | ~400-600 tok/s  | Best single-user latency, KV cache reuse, GGUF format |
| **TTS**   | Kokoro-82M                | ~0.5 GB | ~210× real-time | Apache 2.0, sub-0.3s all lengths, HF TTS Arena winner |

### 3.13 No Pipeline Framework Rationale

The Listener and Talker use **component libraries** (RealtimeSTT, RealtimeTTS) rather than a pipeline orchestration framework (Pipecat, LiveKit Agents, etc.). Rationale: Kaguya's architecture splits the voice pipeline across Listener and Talker with the Gateway as an intermediary — a non-standard topology that pipeline frameworks assume they control end-to-end. Using a framework would require fighting its pipeline model. Component libraries impose no topology; they provide building blocks that fit into Kaguya's conductor architecture.

### 3.14 Custom Code Breakdown

| Capability                                 | Implementation                                                     | Effort              |
| ------------------------------------------ | ------------------------------------------------------------------ | ------------------- |
| VAD + STT pipeline                         | RealtimeSTT (wraps faster-whisper + Silero VAD)                    | ~200-300 lines      |
| Turn detection                             | Custom rule-based (silence + syntax), upgradeable to learned model | ~50-100 lines       |
| LLM → sentence detection → TTS streaming   | Custom async pipeline feeding RealtimeTTS                          | ~200-300 lines      |
| Soul container (post-processing)           | Tag normalization, validation, interception, vocab enforcement     | ~50-100 lines       |
| Tool call interception + LLM re-injection  | Non-blocking: extract tags, dispatch, new inference on result      | ~100-200 lines      |
| PREPARE signal handling + token accounting | Cancel TTS/LLM, track spoken vs. unspoken, report to Gateway       | ~100-150 lines      |
| **Total**                                  |                                                                    | **~700-1150 lines** |

Reference implementation: KoljaB/RealtimeVoiceChat demonstrates the full Listener + LLM + TTS pipeline with interruption handling in ~400 lines using these same libraries.

---

## 4. The Reasoner Agent

### 4.1 Role and Mandate

The Reasoner Agent handles slow-path tasks via existing orchestration frameworks. It is spawned on demand by the Gateway when the Talker emits a `[DELEGATE:...]` tag.

**Output flows through Gateway only.** The Reasoner never communicates directly with the Talker or the user.

### 4.2 Responsibilities

- Receive task descriptions from the Gateway (originated from Talker's `[DELEGATE:...]` tags).
- Invoke the underlying framework (OpenClaw, Claude Code, custom agents).
- Monitor framework output (stdout, logs, API responses).
- Emit structured events back to Gateway via gRPC: `reasoner_started`, `reasoner_intermediate_step`, `reasoner_output`, `reasoner_completed`, `reasoner_error`.

### 4.3 Multi-Agent Support

The Gateway manages multiple concurrent Reasoner Agents, each with a unique `task_id`. The Talker can spin up multiple agents simultaneously.

### 4.4 Implementation

- Language: TypeScript (ecosystem compatibility with OpenClaw/Node.js).
- Spawned on demand by Gateway, one process per task.
- gRPC client connecting to Gateway.
- Adapter pattern: swapping frameworks (OpenClaw → Claude Code → custom) requires only a new adapter, not architectural changes.

### 4.5 IPC: Reasoner → Gateway

```protobuf
service ReasonerService {
  rpc ExecuteTask(TaskRequest) returns (stream ReasonerEvent);
  rpc CancelTask(CancelRequest) returns (CancelAck);
}
```

---

## 5. IPC: Agents → Gateway (Full Protocol)

All agent-to-Gateway communication uses gRPC with Protocol Buffers over Unix domain sockets.

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
```

Audio frames (Opus, 50fps) use a dedicated low-overhead path (raw bytes over Unix socket or minimal-wrapping gRPC stream) to avoid protobuf overhead at 50fps.

---

## 6. VRAM Budget (RTX 5070 Ti — 16 GB)

| Component                 | VRAM         |
| ------------------------- | ------------ |
| LLM (Qwen3-8B Q4)         | ~5-6 GB      |
| STT (faster-whisper INT8) | ~1 GB        |
| TTS (Kokoro-82M)          | ~0.5 GB      |
| KV cache + overhead       | ~2-3 GB      |
| **Total**                 | **~9-11 GB** |
| **Headroom**              | **~5-7 GB**  |

---

## 7. Latency Budget

| Stage                                  | Latency        | Notes                                          |
| -------------------------------------- | -------------- | ---------------------------------------------- |
| Endpoint VAD onset                     | ~30-50ms       | Silero CPU (optional, at endpoint)             |
| Network (local)                        | ~1-5ms         | Opus over OpenPod                              |
| Listener VAD onset                     | ~30-50ms       | Silero in Listener                             |
| STT partial                            | ~200-300ms     | faster-whisper streaming                       |
| Prefix prefill (P1)                    | pre-warmed     | System + memory + history cached between turns |
| Partial prefill (P2)                   | overlapped     | Extends cache during user speech               |
| LLM first token                        | ~50-100ms      | llama.cpp, warm prefix cache (P1)              |
| LLM first sentence                     | ~200-500ms     | ~50 tok/s × 10-15 tokens                       |
| TTS first audio                        | ~50-150ms      | Kokoro                                         |
| **Total (user stops → Kaguya speaks)** | **~400-700ms** | With prefix prefill (P1)                       |

---

## 8. Persona System

### 8.1 Phase 1: System Prompt Steering + File-Based Memory

Persona enforced via `SOUL.md` + `IDENTITY.md` loaded by the Gateway at startup and delivered to the Talker via `UpdatePersona` gRPC. `MEMORY.md` (user profile, project facts, long-term context) is bundled into the same `UpdatePersona` call and cached in the Talker alongside the persona files. The Talker injects all three as the fixed prefix of every LLM call. Action tags (`[EMOTION:...]`, `[DELEGATE:...]`, `[TOOL:...]`) instructed in the system prompt.

When the Gateway appends new facts to `MEMORY.md` after a turn, it sends a new `UpdatePersona` call to the Talker with the updated content. The Talker replaces its cached copy. The Talker never reads or writes any file directly.

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

### Phase 1 Deliverables (Agent scope)

- Listener: RealtimeSTT (faster-whisper + Silero VAD) with custom rule-based turn detection.
- Talker: Qwen3-8B via llama.cpp + RealtimeTTS with Kokoro, sentence-level streaming.
- Soul container post-processing (tag normalization, validation, interception).
- Tool call flow: LLM emits [TOOL:...] → soul container intercepts → gRPC to Gateway → result → new inference round.
- PREPARE signal handling with token accounting.
- Always-on prefix KV cache prefill (system + memory + history pre-warmed between turns).
- Reasoner Adapter for OpenClaw with output interception.
- Persona + memory enforcement via system prompt (`SOUL.md` + `IDENTITY.md` + `MEMORY.md` delivered by Gateway via `UpdatePersona`).
- Text-only fallback mode.
- Basic Deliberative Narration (acknowledge + wait + summarize).
- Tool use examples in system prompt.
- **Audio source: dev-GUI/TUI local endpoint (no OpenPod dependency).**

### Phase 2 Deliverables (Agent scope)

- **OpenPod audio integration** — Listener receives Opus frames via OpenPod (forwarded by Gateway); Talker sends audio to Gateway for OpenPod Channel D mux. No Listener/Talker code change required if Gateway handles the transport switch transparently.
- Partial transcript prefill (word-level KV cache extension on partials).
- Speculative decoding (clause-level draft generation before final transcript).
- Two-stage PREPARE signal (soft fade on `vad_speech_start`, hard stop on first `partial_transcript`).
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
