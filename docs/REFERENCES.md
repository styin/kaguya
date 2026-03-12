# Project Kaguya — Algorithmic Design References

This document records the empirical basis for explicit algorithmic and numerical decisions made during Phase 1 design. Each entry links a decision in the implementation plan to its source rationale.

Maintain this file whenever a numeric threshold, algorithm choice, or non-obvious design decision is introduced or changed. Add a new entry; do not edit existing ones unless correcting an error.

---

## REF-001 — Silence Timer Thresholds (Gateway, M1.6)

**Decision:** Three semantic post-response silence tiers with defaults `SILENCE_SHORT=3s`, `SILENCE_MEDIUM=8s`, `SILENCE_LONG=30s`.

**Scope clarification:** These timers fire *after* the Talker finishes speaking, governing when Kaguya proactively re-engages. They are distinct from the Listener's end-of-turn detection thresholds (see REF-004), which operate during speech at ~300–800ms. Post-response timers operate on a longer human-engagement timescale.

**Rationale:**

- **Academic basis for conversational silence semantics (Jefferson, 1989):** Silences beyond ~1 second are "socially marked" in human conversation. The "standard maximum silence" before an absence becomes notable is approximately 3 seconds. This directly grounds the SHORT tier.
- **IVR / telephony no-input timeouts:** Production IVR systems (Genesys, Avaya) use 3–5s as the first "are you still there?" prompt, and 8–10s before session renegotiation. This grounds the MEDIUM tier.
- **AFK / session teardown:** Telephony IVR session teardown occurs at 8–10s of silence; smart speaker "disengagement" state is typically triggered at 15–30s. 30s is conservative and appropriate for a desktop AI Chief of Staff context where the user may be reading, typing, or thinking.
- **3s (SILENCE_SHORT / "user thinking"):** User is processing the response or paused before their next thought. Soft conversational follow-up. Grounded in Jefferson's 3-second "standard maximum" and IVR first-prompt timing.
- **8s (SILENCE_MEDIUM / "user intent unclear"):** User may have been distracted, is multitasking, or has finished but hasn't spoken. Open-ended check-in. Grounded in IVR extended no-input defaults (Genesys: 5–8s, Avaya: ~8s).
- **30s (SILENCE_LONG / "user likely AFK"):** User has clearly disengaged. Context-shift acknowledgement or go quiet. Grounded in smart speaker disengagement timing.

**Key academic sources:**
- Jefferson, G. (1989). "Preliminary Notes on a Possible Metric Which Provides for a 'Standard Maximum' Silence of Approximately One Second in Conversation." In *Conversation: An Interdisciplinary Perspective*, pp. 166–196. [3-second marked silence threshold]
- Stivers, T. et al. (2009). "Universals and cultural variation in turn-taking in conversation." *PNAS* 106(26): 10587–10592. [Median inter-turn gap ~200ms; 700ms+ signals floor transfer]
- Levinson, S.C. & Torreira, F. (2015). "Timing in turn-taking and its implications for processing models of language." *Frontiers in Psychology* 6:731. [700ms floor-transfer threshold]

**Industry sources:**
- Stanford HAI: "Is It My Turn Yet? Teaching a Voice Assistant When to Speak" — https://hai.stanford.edu/news/it-my-turn-yet-teaching-voice-assistant-when-speak
- AssemblyAI: "How intelligent turn detection (endpointing) solves the biggest challenge in voice agent development" — https://www.assemblyai.com/blog/turn-detection-endpointing-voice-agent
- Twilio: "Guide to Core Latency in AI Voice Agents" — https://www.twilio.com/en-us/blog/developers/best-practices/guide-core-latency-ai-voice-agents
- Nuance/Avaya/Genesys IVR engineering guides — end-of-speech detection and no-input timeout parameters

**All three values are configurable in `config.rs`.** Do not hardcode. Adjust based on deployment context and user feedback.

---

## REF-002 — Opus Decoding in Listener (M2.2)

**Decision:** Opus → PCM decoding is performed in the Listener (Python, `opuslib` or `pyogg`) rather than in the Gateway (Rust).

**Rationale:**

1. **Spec mandates it explicitly.** `spec-agent-v0.1.0.md §2.2` lists "Decode Opus frames to PCM audio" as a Listener responsibility. `spec-gateway-v0.1.0.md §1` states the Gateway "does not inspect or decode audio content — it forwards audio bytes between OpenPod and the Listener/Talker without reading them." There is no ambiguity.

2. **RealtimeSTT requires PCM.** RealtimeSTT's `feed_audio()` accepts raw bytes it appends directly to its audio buffer; faster-whisper expects 16kHz/16-bit/mono PCM. Neither library has an Opus decoding layer. Feeding Opus bytes directly produces garbage. (Confirmed via KoljaB/RealtimeSTT issues #22, #52, #137.)

3. **Transport-layer agnosticism (Phase 2 stability).** If the Gateway decoded Opus, switching from dev-GUI/TUI (Phase 1) to OpenPod (Phase 2) could require Gateway changes to accommodate different codec profiles. Listener-side decoding means no Listener code changes when the Gateway's transport switches — the Listener always receives Opus bytes and always decodes them, regardless of what's upstream.

4. **Industry pattern.** LiveKit Agents and Pipecat both decode Opus at the transport-to-processing boundary — the component that takes bytes off the wire and hands PCM to the AI stack. In Kaguya's architecture, that boundary is exactly the Listener's ingress.

5. **Latency is negligible.** libopus decodes a 20ms frame in <0.3ms in Python via opuslib. At 50fps, this is <1% of one CPU core. Not on the critical path.

**Implementation note:** Use `opuslib.Decoder(fs=16000, channels=1)` to decode directly to 16kHz mono PCM (libopus handles internal resampling). `frame_size` for `decode()` is 320 samples for a 20ms frame at 16kHz output (16000 × 0.02 = 320). `pyogg` is an alternative with the same libopus backend.

**Sources:**
- `spec-agent-v0.1.0.md §2.2` (Listener Responsibilities — Opus decode listed explicitly)
- `spec-gateway-v0.1.0.md §1` (Gateway does not decode audio)
- KoljaB/RealtimeSTT `feed_audio` PCM format: https://github.com/KoljaB/RealtimeSTT/issues/22
- KoljaB/RealtimeSTT Opus discussion: https://github.com/KoljaB/RealtimeSTT/discussions/137

---

## REF-003 — IPC File Organization: Separate Server and Client Files, Domain-First Naming (M2.4, §8)

**Decision:** Each process uses separate files for its gRPC server role vs. client role, named by domain purpose rather than gRPC role. The Talker's ListenerService gRPC client is embedded in `voice/listener.py` (not a standalone `client.py`) because it is tightly coupled to listener logic and not shared by other components.

**Rationale:**

1. **Lifecycle incompatibility:** A gRPC server calls `await server.wait_for_termination()` (blocking); a gRPC client runs a long-lived `stub.StreamEvents()` async generator. These are two different asyncio task lifecycles. Combining them in one file forces incompatible control flows into the same module.

2. **Google's canonical pattern:** Every gRPC Python example in `grpc/grpc/examples/python/` uses separate `*_server.py` and `*_client.py` files — `helloworld`, `route_guide`, `multiplex`, `auth`, `compression`. The grpc.io basics tutorial explicitly names these `route_guide_server.py` and `route_guide_client.py`. This is a hard convention, not a preference.

3. **Independent testability:** `server.py` (TalkerServiceServicer) can be tested with mock `brain/` and `voice/speaker.py` without instantiating any gRPC client. `voice/listener.py` can be tested with a mock Gateway stub. A combined file requires both roles to be active for either to be tested.

4. **Domain-first naming is superior to role-first naming:** `voice/listener.py` is named for what it does (listen, stream audio events to Gateway) rather than `grpc_client.py`. A developer reading `voice/` understands the module's purpose without knowing its gRPC role. This is the pattern used by **LiveKit Agents Python SDK**: `_agent.py` (server-side servicer equivalent) and `_worker.py` (outbound client to LiveKit server) are separate modules named by domain role, not by "server" vs. "client". The gRPC stub is an implementation detail internal to `_worker.py`, not surfaced as a top-level `client.py`.

5. **Scale threshold:** The Talker is ~700–1150 lines total. RealtimeVoiceChat (the reference single-process script) uses ~400 lines in one file because it has no separate gRPC server and client roles. Kaguya's architecture is fundamentally different — the Talker has a four-method gRPC server servicer plus a streaming client with reconnect logic. A single IPC file would exceed 200 lines of unrelated concerns.

**File boundary table:**

| File | Domain role | gRPC role |
|---|---|---|
| `talker/src/server.py` | Wires brain ↔ voice ↔ Gateway per-turn | SERVER — receives `ProcessPrompt`, `Prepare`, `PrefillCache`, `UpdatePersona` from Gateway |
| `talker/src/voice/listener.py` | Captures speech, detects turns | CLIENT — dials Gateway, streams `ListenerEvent` messages |
| `talker/src/main.py` | Process entrypoint | Orchestrates both as `asyncio` tasks |

**Sources:**
- grpc.io Python basics tutorial: https://grpc.io/docs/languages/python/basics/
- Real Python gRPC guide: https://realpython.com/python-microservices-grpc/
- LiveKit Agents Python SDK — `_agent.py` / `_worker.py` split: https://github.com/livekit/agents
- Python gRPC structure guide: https://github.com/viktorvillalobos/python-grpc-structure

---

## REF-004 — Turn Detection Thresholds (Listener, M2.3)

**Decision:** `SILENCE_THRESHOLD_MS=800ms` (emit `final_transcript` regardless of syntax), `SYNTAX_SILENCE_THRESHOLD_MS=300ms` (begin syntax check at 300ms; emit early if syntactically complete before 800ms is reached).

**Scope clarification:** These govern the Listener's end-of-turn detection *during* speech, determining when to emit `final_transcript`. Distinct from REF-001 (Gateway post-response timers, which operate at 3–30s).

### What each threshold actually does

`SYNTAX_SILENCE_THRESHOLD_MS=300ms` is **not** a trigger — it is the entry point to the ambiguous zone. The logic is:

```
silence < 300ms          → always wait, don't inspect syntax
300ms ≤ silence < 800ms  → inspect syntax:
                             complete (terminal punct, no open clause) → emit
                             incomplete (dangling "and", "but", etc.)  → keep waiting
silence ≥ 800ms          → emit unconditionally regardless of syntax
```

300ms is chosen as the zone entry because Goldman-Eisler's data shows within-utterance pauses cluster at 150–400ms. Below 300ms we are firmly inside normal clause-boundary pause territory and no syntactic check can save us. At 300ms we enter the probabilistically ambiguous zone where syntactic shape becomes a useful discriminator.

### Known failure mode: fragmented transcripts at >800ms

If a slow speaker or thinker pauses >800ms mid-sentence, the unconditional rule emits a `final_transcript` for the partial utterance. The turn lifecycle then proceeds:
1. Partial `final_transcript` → Gateway assembles context → Talker begins generating a response.
2. User resumes speaking → `vad_speech_start` → PREPARE → Talker is interrupted (only spoken portion appended to history).
3. Second fragment → new `final_transcript` → fresh `ProcessPrompt` with broken context.

The PREPARE mechanism prevents history corruption (only spoken text is appended) but cannot prevent the response to the partial utterance from being wrong or confusing. The worst case is if the Talker *finishes* responding before the user resumes: history then contains a full response to an incomplete thought, and the continuation arrives as a new, context-broken turn.

This is a known limitation of Phase 1's rule-based approach. It is the primary motivation for the Phase 2 learned turn detection model.

### How production systems avoid this

Production systems do not primarily use silence duration as the turn-end signal — silence is a fallback:

- **Amazon Alexa:** Multi-signal classifier combining VAD + prosodic terminal boundary detection (falling pitch, terminal vowel lengthening) + ASR confidence plateau. Prosody distinguishes a thinking pause (flat pitch, no lengthening) from a turn-final pause (falling pitch). Silence fires only as a fallback when acoustic signals are ambiguous.
- **Google Dialogflow CX "smart endpointing":** Streaming ASR confidence-based suppression. If the ASR model predicts more tokens are likely (confidence still rising), end-of-turn is suppressed even at 1000ms+ of silence. Silence timeout is a fallback.
- **LiveKit open-weights turn detection model (Phase 2):** 85% true-positive, 97% true-negative. Trained on acoustic + linguistic features. Referenced in `spec-agent-v0.1.0.md §2.4` as the Phase 2 replacement.

In short: Alexa and Dialogflow CX both achieve low fragmentation rates because their primary signal is acoustic/model-based, not temporal. Phase 1's 800ms unconditional rule will produce occasional fragmentation for slow or deliberate speakers. This is accepted as a Phase 1 limitation.

**Academic sources:**
- Stivers, T. et al. (2009). *PNAS* 106(26): 10587–10592. [~700ms as floor-transfer threshold]
- Levinson, S.C. & Torreira, F. (2015). *Frontiers in Psychology* 6:731. [Sub-200ms responses; silence semantics]
- Goldman-Eisler, F. (1968). *Psycholinguistics: Experiments in Spontaneous Speech*. Academic Press. [Within-utterance pause duration 150–400ms]
- Grosjean, F. & Deschamps, A. (1975). "Analyse contrastive des variables temporelles de l'anglais et du français." *Phonetica* 31: 144–184. [Clause-boundary pause duration corroboration]

**Industry sources:**
- Amazon Alexa developer documentation — multi-signal endpointing, prosodic boundary detection
- Google Dialogflow CX documentation — smart endpointing, ASR confidence suppression
- AssemblyAI endpointing article: https://www.assemblyai.com/blog/turn-detection-endpointing-voice-agent
- LiveKit turn detection model: https://github.com/livekit/agents (85% TP / 97% TN)

---

*Add new entries below this line. Format: `## REF-NNN — Short Title (component, milestone)`*
