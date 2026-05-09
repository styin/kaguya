# Project Kaguya — Algorithmic Design References

This document records the empirical basis for explicit algorithmic and numerical decisions made during Phase 1 design. Each entry links a decision in the implementation plan to its source rationale.

Maintain this file whenever a numeric threshold, algorithm choice, or non-obvious design decision is introduced or changed. Add a new entry; do not edit existing ones unless correcting an error.

---

## REF-000 — The Dual Path Architecture of Project Kaguya

- **Mind-Paced Speaking (MPS):** Dual-brain architecture for concurrent reasoning and speech generation. arXiv, October 2025.
- **Interactive ReAct (Bojie Li):** Continuous thinking mechanism — think while listening, speak while thinking, filler speech for latency reduction. December 2025.
- **LTS-VoiceAgent:** Listen-Think-Speak framework with Dynamic Semantic Trigger and Dual-Role Stream Orchestrator. arXiv, January 2026.
- **PredGen:** Predictive Generation — speculative decoding at input time for voice pipelines. arXiv, June 2025.
- **StreamingThinker:** Streaming thinking paradigm — LLMs that reason while receiving input. arXiv, October 2025.
- **Qwen2.5-Omni:** Thinker-Talker architecture for multimodal perception and streaming speech generation. Alibaba, 2025.
- **Neuro-sama (Vedal987):** Event-driven AI VTuber architecture with multi-channel input aggregation, module-based prompt injection, silence detection, and always-listening behavior. Production reference for persistent AI presence.
- **Project Airi (moeru-ai/airi):** Open-source Neuro-sama recreation (17.5K stars). Reference for soul container post-processing pattern (deterministic persona enforcement between LLM and TTS), unspeech provider abstraction (unified TTS/STT API across vendors), and Web-first multiplatform deployment. TypeScript monorepo with Rust native acceleration. Does not implement delegation, event-driven conductors, or proactive speech.

## REF-001 — Silence Timer Thresholds (Gateway, M1.6)

**Decision:** Three semantic post-response silence tiers with defaults `SILENCE_SHORT=3s`, `SILENCE_MEDIUM=8s`, `SILENCE_LONG=30s`.

**Scope clarification:** These timers fire _after_ the Talker finishes speaking, governing when Kaguya proactively re-engages. They are distinct from the Listener's end-of-turn detection thresholds (see REF-004), which operate during speech at ~300–800ms. Post-response timers operate on a longer human-engagement timescale.

**Rationale:**

- **Academic basis for conversational silence semantics (Jefferson, 1989):** Silences beyond ~1 second are "socially marked" in human conversation. The "standard maximum silence" before an absence becomes notable is approximately 3 seconds. This directly grounds the SHORT tier.
- **IVR / telephony no-input timeouts:** Production IVR systems (Genesys, Avaya) use 3–5s as the first "are you still there?" prompt, and 8–10s before session renegotiation. This grounds the MEDIUM tier.
- **AFK / session teardown:** Telephony IVR session teardown occurs at 8–10s of silence; smart speaker "disengagement" state is typically triggered at 15–30s. 30s is conservative and appropriate for a desktop AI Chief of Staff context where the user may be reading, typing, or thinking.
- **3s (SILENCE_SHORT / "user thinking"):** User is processing the response or paused before their next thought. Soft conversational follow-up. Grounded in Jefferson's 3-second "standard maximum" and IVR first-prompt timing.
- **8s (SILENCE_MEDIUM / "user intent unclear"):** User may have been distracted, is multitasking, or has finished but hasn't spoken. Open-ended check-in. Grounded in IVR extended no-input defaults (Genesys: 5–8s, Avaya: ~8s).
- **30s (SILENCE_LONG / "user likely AFK"):** User has clearly disengaged. Context-shift acknowledgement or go quiet. Grounded in smart speaker disengagement timing.

**Key academic sources:**

- Jefferson, G. (1989). "Preliminary Notes on a Possible Metric Which Provides for a 'Standard Maximum' Silence of Approximately One Second in Conversation." In _Conversation: An Interdisciplinary Perspective_, pp. 166–196. [3-second marked silence threshold]
- Stivers, T. et al. (2009). "Universals and cultural variation in turn-taking in conversation." _PNAS_ 106(26): 10587–10592. [Median inter-turn gap ~200ms; 700ms+ signals floor transfer]
- Levinson, S.C. & Torreira, F. (2015). "Timing in turn-taking and its implications for processing models of language." _Frontiers in Psychology_ 6:731. [700ms floor-transfer threshold]

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

3. **Independent testability:** `server.py` (TalkerServiceServicer) can be tested with mock `inference/` and `voice/speaker.py` without instantiating any gRPC client. `voice/listener.py` can be tested with a mock Gateway stub. A combined file requires both roles to be active for either to be tested.

4. **Domain-first naming is superior to role-first naming:** `voice/listener.py` is named for what it does (listen, stream audio events to Gateway) rather than `grpc_client.py`. A developer reading `voice/` understands the module's purpose without knowing its gRPC role. This is the pattern used by **LiveKit Agents Python SDK**: `_agent.py` (server-side servicer equivalent) and `_worker.py` (outbound client to LiveKit server) are separate modules named by domain role, not by "server" vs. "client". The gRPC stub is an implementation detail internal to `_worker.py`, not surfaced as a top-level `client.py`.

5. **Scale threshold:** The Talker is ~700–1150 lines total. RealtimeVoiceChat (the reference single-process script) uses ~400 lines in one file because it has no separate gRPC server and client roles. Kaguya's architecture is fundamentally different — the Talker has a four-method gRPC server servicer plus a streaming client with reconnect logic. A single IPC file would exceed 200 lines of unrelated concerns.

**File boundary table:**

| File                           | Domain role                            | gRPC role                                                                                  |
| ------------------------------ | -------------------------------------- | ------------------------------------------------------------------------------------------ |
| `talker/server.py`         | Wires inference ↔ voice ↔ Gateway per-turn | SERVER — receives `ProcessPrompt`, `Prepare`, `PrefillCache`, `UpdatePersona` from Gateway |
| `talker/voice/listener.py` | Captures speech, detects turns         | CLIENT — dials Gateway, streams `ListenerEvent` messages                                   |
| `talker/main.py`           | Process entrypoint                     | Orchestrates both as `asyncio` tasks                                                       |

**Sources:**

- grpc.io Python basics tutorial: https://grpc.io/docs/languages/python/basics/
- Real Python gRPC guide: https://realpython.com/python-microservices-grpc/
- LiveKit Agents Python SDK — `_agent.py` / `_worker.py` split: https://github.com/livekit/agents
- Python gRPC structure guide: https://github.com/viktorvillalobos/python-grpc-structure

---

## REF-004 — Turn Detection Thresholds (Listener, M2.3)

**Decision:** `SILENCE_THRESHOLD_MS=800ms` (emit `final_transcript` regardless of syntax), `SYNTAX_SILENCE_THRESHOLD_MS=300ms` (begin syntax check at 300ms; emit early if syntactically complete before 800ms is reached).

**Scope clarification:** These govern the Listener's end-of-turn detection _during_ speech, determining when to emit `final_transcript`. Distinct from REF-001 (Gateway post-response timers, which operate at 3–30s).

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

The PREPARE mechanism prevents history corruption (only spoken text is appended) but cannot prevent the response to the partial utterance from being wrong or confusing. The worst case is if the Talker _finishes_ responding before the user resumes: history then contains a full response to an incomplete thought, and the continuation arrives as a new, context-broken turn.

This is a known limitation of Phase 1's rule-based approach. It is the primary motivation for the Phase 2 learned turn detection model.

### How production systems avoid this

Production systems do not primarily use silence duration as the turn-end signal — silence is a fallback:

- **Amazon Alexa:** Multi-signal classifier combining VAD + prosodic terminal boundary detection (falling pitch, terminal vowel lengthening) + ASR confidence plateau. Prosody distinguishes a thinking pause (flat pitch, no lengthening) from a turn-final pause (falling pitch). Silence fires only as a fallback when acoustic signals are ambiguous.
- **Google Dialogflow CX "smart endpointing":** Streaming ASR confidence-based suppression. If the ASR model predicts more tokens are likely (confidence still rising), end-of-turn is suppressed even at 1000ms+ of silence. Silence timeout is a fallback.
- **LiveKit open-weights turn detection model (Phase 2):** 85% true-positive, 97% true-negative. Trained on acoustic + linguistic features. Referenced in `spec-agent-v0.1.0.md §2.4` as the Phase 2 replacement.

In short: Alexa and Dialogflow CX both achieve low fragmentation rates because their primary signal is acoustic/model-based, not temporal. Phase 1's 800ms unconditional rule will produce occasional fragmentation for slow or deliberate speakers. This is accepted as a Phase 1 limitation.

**Academic sources:**

- Stivers, T. et al. (2009). _PNAS_ 106(26): 10587–10592. [~700ms as floor-transfer threshold]
- Levinson, S.C. & Torreira, F. (2015). _Frontiers in Psychology_ 6:731. [Sub-200ms responses; silence semantics]
- Goldman-Eisler, F. (1968). _Psycholinguistics: Experiments in Spontaneous Speech_. Academic Press. [Within-utterance pause duration 150–400ms]
- Grosjean, F. & Deschamps, A. (1975). "Analyse contrastive des variables temporelles de l'anglais et du français." _Phonetica_ 31: 144–184. [Clause-boundary pause duration corroboration]

**Industry sources:**

- Amazon Alexa developer documentation — multi-signal endpointing, prosodic boundary detection
- Google Dialogflow CX documentation — smart endpointing, ASR confidence suppression
- AssemblyAI endpointing article: https://www.assemblyai.com/blog/turn-detection-endpointing-voice-agent
- LiveKit turn detection model: https://github.com/livekit/agents (85% TP / 97% TN)

---

## REF-005 — Proto Stub Generation Asymmetry (M0, polyglot build system)

**Decision:** Python proto stubs are **committed** to git. Rust proto stubs are **generated** via `build.rs` during `cargo build`. TypeScript uses **runtime loading** via `@grpc/proto-loader` with no code generation.

**Rationale:**

This asymmetry respects each language ecosystem's conventions while optimizing for end-user experience:

### Python: Committed stubs (talker/proto/)

1. **Small size:** `kaguya_pb2.py` + `kaguya_pb2_grpc.py` + `__init__.py` = ~33KB total. Negligible repository bloat.

2. **Zero setup for end users:** With stubs committed, end users can clone, `uv sync`, and run tests immediately. No protoc installation, no grpcio-tools invocation, no Windows/Linux/macOS proto generation compatibility issues.

3. **Stable output:** grpcio-tools generates deterministic Python code. Committed stubs never cause "generated code mismatch" errors across Python versions or platforms. (Contrast with C++ protobuf where codegen varies by protoc version.)

4. **Ecosystem standard for libraries:** Python gRPC libraries (google-cloud-*, grpc-gateway-*, Confluent Kafka Python) all commit generated stubs. The pattern is: "proto schema in version control → stubs in version control → import and use." This is because Python packaging does not have a first-class "build step" like Rust's `build.rs` — `setup.py` custom commands are fragile and discouraged (PEP 517/518 shift to declarative builds).

5. **Patch requirement:** grpcio-tools generates bare imports (`import kaguya_pb2`), which break when proto/ is a package. We patch to relative imports (`from . import kaguya_pb2`). Committing the patched output ensures this works for all users without requiring them to run `gen_proto.py`.

**Size reference:** At 33KB for kaguya.proto (~500 lines), even a 10× schema expansion would only be 330KB — still negligible in a repository with bundled native libraries.

### Rust: Generated stubs (gateway/src/proto/, gitignored)

1. **Large size:** tonic-build generates 150–300KB of Rust code for a medium-sized proto schema. This bloats diffs and creates merge conflicts when the schema changes.

2. **Ecosystem standard for services:** Every major Rust gRPC project uses `build.rs` with tonic-build: TiKV (PingCAP's distributed database), Vector (Datadog's observability pipeline), and all official Tonic examples. The pattern is: "proto schema in version control → build.rs generates code → `.gitignore src/proto/`."

3. **Build system integration:** Rust's `build.rs` is a first-class feature designed exactly for codegen. Cargo automatically re-runs `build.rs` when `proto/kaguya/v1/kaguya.proto` changes (via `println!("cargo:rerun-if-changed=...")` directives). This is seamless and requires no manual steps.

4. **No patch requirement:** tonic-build generates idiomatic Rust with correct module paths. No post-processing needed.

**Size reference:** TiKV's proto/ generates ~2MB of Rust code (gitignored). Vector's proto/ generates ~500KB (gitignored). Our 150–300KB estimate is conservative.

### TypeScript: Runtime loading (reasoner/, no generation)

1. **No codegen in Node.js norm:** The Node.js gRPC ecosystem strongly prefers runtime loading via `@grpc/proto-loader`. Official Google examples, grpc.io tutorials, and production systems (e.g., Uber's service meshes) load `.proto` files at runtime. Static codegen (`grpc-tools`, `ts-proto`) exists but is niche.

2. **Zero build step:** `@grpc/proto-loader` reads `proto/kaguya/v1/kaguya.proto` directly at runtime and generates TypeScript types on-the-fly via Protobuf.js reflection. No compilation, no stubs, no gitignore rules.

3. **Instant schema iteration:** Changing `kaguya.proto` requires only restarting the Node.js process. No regeneration command, no build cache invalidation.

4. **Trade-off accepted:** Runtime loading sacrifices compile-time type safety (types are inferred at runtime, not statically checked). This is acceptable for the Reasoner (a single-purpose service with a small API surface) but would not scale to a large microservice mesh.

### Cross-language consistency via buf (CI only)

**buf is used exclusively for CI validation** (`buf lint`, `buf breaking`) in `.github/workflows/proto-lint.yml`. It does **not** generate code. This ensures schema consistency across all three languages without forcing a single codegen tool.

**File organization:**

```
proto/
  buf.yaml               # CI-only linting and breaking change detection
  kaguya/v1/kaguya.proto # Single source of truth
talker/
  proto/                 # COMMITTED Python stubs (33KB)
  scripts/gen_proto.py   # Dev-only regeneration via grpcio-tools
gateway/
  build.rs               # Invokes tonic-build automatically on cargo build
  src/proto/             # GITIGNORED Rust stubs (150-300KB)
reasoner/
  # No proto/ directory — runtime loads ../proto/kaguya/v1/kaguya.proto
```

**Developer workflow:**

```sh
# After editing proto/kaguya/v1/kaguya.proto:
make proto                # Regenerates Python stubs (talker/proto/)
cargo build               # Automatically regenerates Rust stubs (gateway/src/proto/)
# Reasoner: no action needed (runtime loading)
git add proto/ talker/proto/  # Commit schema + Python stubs
git commit -m "proto: add new RPC method"
```

**Sources:**

- Python gRPC packaging survey: google-cloud-python, grpc-gateway, confluent-kafka-python all commit stubs
- Rust build.rs + tonic-build canonical examples: TiKV (https://github.com/tikv/tikv), Vector (https://github.com/vectordotdev/vector), Tonic examples (https://github.com/hyperium/tonic/tree/master/examples)
- Node.js gRPC runtime loading: grpc.io Node.js tutorial (https://grpc.io/docs/languages/node/basics/), `@grpc/proto-loader` README (https://github.com/grpc/grpc-node/tree/master/packages/proto-loader)
- PEP 517/518 (declarative Python builds, discouraging setup.py custom commands): https://peps.python.org/pep-0517/, https://peps.python.org/pep-0518/

---

## REF-006 — Max Response Sentences for Voice Brevity (Talker, M3.4)

**Decision:** `MAX_RESPONSE_SENTENCES=4` — the soul container stops emitting spoken sentences after this limit per turn. Configurable in `config.py`.

**Rationale:**

Voice responses must be concise. Long monologues break conversational flow, cause the user to lose track of content, and delay their ability to respond or interrupt.

- **Conversational analysis (Clark & Schaefer, 1989):** Conversational contributions are structured in "installments" — speakers chunk information into 1-4 sentence units and check for understanding signals before continuing. Turns exceeding ~4 sentences without a pause point degrade grounding accuracy.
- **Voice UI guidelines (Amazon Alexa, Google Assistant):** Both platforms recommend responses under 3-4 sentences for informational answers. Alexa's "brief mode" limits to 1-2 sentences. Google's conversational design guidelines cap at ~30 seconds of speech (~4 sentences at conversational pace).
- **Kokoro TTS synthesis window:** At ~210x real-time, a 4-sentence response (~40-80 tokens, ~8-15 seconds of audio) synthesizes in <100ms. Longer responses offer no latency benefit and risk user disengagement.
- **Kaguya's delegation model:** If the answer requires more than 4 sentences, the Talker should acknowledge and delegate to a Reasoner rather than monologue. This enforces Axiom II (Orchestration & Delegation).

**Range:** 2-4 sentences. Default 4 (upper bound for informational answers). Set to 2 for terse personas.

**Sources:**

- Clark, H.H. & Schaefer, E.F. (1989). "Contributing to Discourse." _Cognitive Science_ 13(2): 259-294.
- Amazon Alexa Design Guide — Response Length: https://developer.amazon.com/en-US/docs/alexa/alexa-design/get-started.html
- Google Conversational Design — Response Brevity: https://developers.google.com/assistant/conversational/design

---

## REF-007 — Hybrid Retrieval via Reciprocal Rank Fusion (Gateway RAG)

**Decision:** The RAG retriever fuses BM25 (FTS5) and vector-similarity rankings using Reciprocal Rank Fusion (RRF) with `k = 60`.

**Implementation:** [gateway/src/rag/ranker.rs](gateway/src/rag/ranker.rs) — `RRF_K: f64 = 60.0`. Score per item: `1 / (k + rank + 1)` summed across sources, then sorted descending. Used by [gateway/src/rag/retriever.rs](gateway/src/rag/retriever.rs) to combine the two retrieval modalities.

**Rationale:**

- **RRF is the canonical late-fusion method for combining heterogeneous rankers (Cormack, Clarke & Buettcher, 2009):** The original SIGIR paper showed RRF outperforming Condorcet voting and learned rank-aggregation methods on TREC tasks. It requires no per-source score normalization (BM25 ranks and cosine similarities live in different units), no training, and is robust to differing list lengths — all properties we need since the optional embedder may be absent.
- **Choice of `k = 60`:** This is the constant used in the original paper and widely adopted as the default in production hybrid-search stacks (Elasticsearch, Vespa, Weaviate, LangChain). The constant smooths out the contribution of very high ranks; smaller `k` (e.g. 1) makes top-1 hits dominate, larger `k` (e.g. 1000) flattens the curve. Empirically `k=60` is the well-tuned middle ground for IR-style retrieval.
- **Why RRF over linear combination:** Linear-weighted score combination requires calibrating BM25's range against cosine similarity's `[-1, 1]` range — calibration that drifts as the corpus changes. RRF only uses ordinal rank, so it's stable against any monotonic rescoring on either side.

**Configurability:** The constant is currently hardcoded; promote to `RagConfig::rrf_k` if tuning becomes necessary.

**Sources:**

- Cormack, G.V., Clarke, C.L.A. & Buettcher, S. (2009). "Reciprocal Rank Fusion outperforms Condorcet and individual Rank Learning Methods." _Proceedings of SIGIR 2009_, pp. 758–759. https://dl.acm.org/doi/10.1145/1571941.1572114
- Elasticsearch reference — Hybrid search with RRF: https://www.elastic.co/docs/reference/elasticsearch/rest-apis/reciprocal-rank-fusion

---

## REF-008 — Default `top_k = 10` for Hybrid Retrieval (Gateway RAG)

**Decision:** [gateway/src/config.rs](gateway/src/config.rs) sets `RagConfig::top_k = 10` as the default number of memories returned per turn. The retriever requests `top_k * 2` from each source, fuses via RRF (REF-007), then truncates to `top_k`.

**Rationale:**

- **Standard RAG retrieval window before re-ranking (Lewis et al., 2020):** The original RAG paper for knowledge-intensive NLP used 5–10 retrieved passages as the augmentation set. Production RAG systems (Pinecone, Weaviate, LangChain) default to 5–20 for similar reasons.
- **Token budget:** Kaguya memories are short (capped at ~200 characters by `truncate_chars` in [gateway/src/rag/mod.rs](gateway/src/rag/mod.rs)). 10 retrieved entries ≈ 2000 chars ≈ 500–700 tokens — fits inside `TalkerContext` without crowding history or persona.
- **Why over-fetch then fuse:** Pulling `top_k * 2` from each modality before RRF gives the fusion stage signal from items that might rank low in one source but high in the other. Truncating to `top_k` post-fusion preserves the diversity benefit.

**Configurability:** Adjust `[rag] top_k` in `config/gateway.toml` if context length budgets change.

**Sources:**

- Lewis, P. et al. (2020). "Retrieval-Augmented Generation for Knowledge-Intensive NLP Tasks." _NeurIPS 2020_. https://arxiv.org/abs/2005.11401
- Pinecone — RAG retrieval defaults: https://www.pinecone.io/learn/retrieval-augmented-generation/

---

## REF-009 — BM25 via SQLite FTS5 with `porter unicode61` Tokenizer (Gateway RAG)

**Decision:** Memory full-text search uses SQLite's FTS5 virtual table with the tokenizer string `porter unicode61` ([gateway/src/rag/store.rs](gateway/src/rag/store.rs) — table `memories_fts`).

**Rationale:**

- **FTS5's BM25 ranking is the default and battle-tested:** SQLite ships Okapi BM25 (Robertson/Spärck Jones IDF) as the FTS5 default ranking function. No extension or external library needed. Identical scoring semantics to Lucene's `BM25Similarity` for English queries.
- **`unicode61` for international text:** SQLite's default `simple` tokenizer is ASCII-only and splits non-ASCII text as a single token, which kills recall on Chinese/Japanese/Korean. `unicode61` applies Unicode-aware folding (case + diacritics) and respects character classes — matching the Chinese trigger keywords (`我喜欢`, `项目`, …) that the memory extractor itself emits.
- **`porter` stemmer for English:** Folds `like`/`liked`/`liking` to a common stem so a memory recorded as "I like coffee" matches a query for "what does the user like". Porter stemming is the standard IR stemmer; FTS5 supports stacking it on top of `unicode61`.
- **Why not a custom CJK tokenizer (e.g. ICU):** Adds a build-time dependency and a per-platform binary. SQLite-bundled `unicode61` covers the substring matching we need at this corpus size (memory entries are short and few). Re-evaluate when memory grows past ~100k entries or recall on multi-character CJK queries degrades.

**Sources:**

- SQLite FTS5 documentation — Tokenizers: https://www.sqlite.org/fts5.html#tokenizers
- SQLite FTS5 — BM25 ranking: https://www.sqlite.org/fts5.html#the_bm25_function
- Porter, M.F. (1980). "An algorithm for suffix stripping." _Program_ 14(3): 130–137.

---

## REF-010 — RAG Memory Truncation: Storage-Time vs. Output-Time (Gateway RAG)

**Decision:** The RAG store keeps full-fidelity memory content. Truncation only happens at *output* time — when retrieval results or the exported `memory_md` document are assembled for the Talker prompt. The store side has only a defensive sanity bound.

**Configuration ([gateway/src/config.rs](gateway/src/config.rs#L48), `[rag]` block in `config/gateway.toml`):**

| Knob                       | Default     | Layer       | Purpose                                                                                                |
| -------------------------- | ----------- | ----------- | ------------------------------------------------------------------------------------------------------ |
| `max_storage_chars`        | `Some(4096)` | Storage     | Defensive cap on the `memories.content` row. Real voice utterances never approach this. Prevents pathological pastes / adversarial inputs from poisoning the index. |
| `max_chars_per_result`     | `None`      | Retrieval   | Cap on each `RetrievalResult.content` injected into the per-turn Talker prompt. `None` = let the model context budget govern. Set to bound per-turn prompt cost when many retrievals fire. |
| `max_chars_per_md_entry`   | `None`      | Persona MD  | Cap on each row of the "Recent Context" section in `RagStore::export_as_markdown` (delivered via `UpdatePersona`). Independent of retrieval; affects the long-term-prefix cost. |

**Rationale:**

- **Truncation at storage time is information loss that cascades.** Once a row is written truncated, every future BM25 search, every vector embedding, every exported `memory_md` row gets the damaged version. The original phrasing — including the *reason* a preference exists ("...because we're migrating off Java") — is gone forever. Keep storage authoritative; let consumers decide their own budget.

- **Output-time caps are reversible.** A future change to `max_chars_per_result` (or removing the cap entirely) immediately benefits all stored memories on the next retrieval. No migration, no re-extraction.

- **The 4 KB storage sanity bound is defensive only.** A 60-second monologue at conversational pace is ~1000 chars (English) or ~250–400 chars (Chinese, denser). 4096 chars is ~10–15× that, so legitimate voice input never hits the cap. Its only job is to bound the worst case if a user pastes raw text into a future text-input path or if an upstream component sends pathological content.

- **Why `None` (unlimited) for output caps:** The 8B fast-path Talker has an 8K–32K context window depending on the `llama.cpp` config; with `top_k=10` retrievals at typical voice-utterance length (~150–300 chars each), the retrieval section is ~1.5–3 KB. That's well within budget. Capping at the output layer is *available* for deployments that hit budget pressure (large `top_k`, long memories, smaller-context models) but isn't needed for the default Phase-1 setup.

- **Asymmetry with REF-006 (`max_response_sentences = 4`):** That sets a hard cap on assistant output for voice brevity. RAG truncation defaults to unlimited because input retrieval already passes through `top_k` (count-based) and BM25 ranking (relevance-based) — those are the right knobs for budget. Adding an additional length cap by default would over-constrain.

**Phase-2 work that supersedes this:** Replace keyword-trigger extraction with LLM-based extraction (see `docs/spec-gateway-v0.1.0.md` Section 4). LLM extraction will produce concise, normalized memory entries directly — at which point `max_storage_chars` becomes vestigial defense and the output caps may shift to operate on token counts rather than char counts.

**Sources:**

- "Retrieve and refine, don't truncate" — general RAG-pipeline practice; e.g. LlamaIndex / LangChain documentation on chunking-vs-truncation tradeoffs.
- SQLite cell-size limits: https://www.sqlite.org/limits.html (default 1 GB; 4 KB is far below any practical concern).

---

_Add new entries below this line. Format: `## REF-NNN — Short Title (component, milestone)`_
