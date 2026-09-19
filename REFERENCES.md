# Project Kaguya — Design Bibliography

This bibliography collects external research and industry practice relevant to Kaguya's significant design questions. Entries explain what each source establishes and how it informs the project; a source need not settle the question or validate our chosen defaults.

Keep source findings distinct from project inference. Internal approvals, implementation history, routine tooling choices, configuration inventories, and test receipts belong in specifications, configuration documentation, or the implementation plan. Improve existing entries as evidence develops, preserve their REF IDs, and do not reuse deleted IDs.

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

## REF-008 — Retrieval Breadth and Context Selection

**Question:** How much memory should a turn retrieve, and how much of that
candidate set should reach the model?

**References:**

- Lewis, P. et al. (2020). [Retrieval-Augmented Generation for Knowledge-Intensive NLP Tasks](https://arxiv.org/abs/2005.11401).
  Combines a retrieved, non-parametric memory with a generative model and
  evaluates it on knowledge-intensive NLP tasks. A foundation for retrieval
  augmentation, not evidence for a universal number of conversational memories.
- Anthropic (2024). [Introducing Contextual Retrieval](https://www.anthropic.com/engineering/contextual-retrieval).
  Separates broad candidate retrieval from reranking and final context selection.
  Its experiments compare different final chunk counts and report a trade-off
  between retrieval coverage, distracting context, and latency. The reported
  settings belong to those corpora and models.
- Liu, N. F. et al. (2023). [Lost in the Middle: How Language Models Use Long Contexts](https://arxiv.org/abs/2307.03172).
  Finds that answer quality on multi-document question answering and key-value
  retrieval depends on where relevant information appears in the context.
  Context-window capacity alone does not establish effective use of that context.

**Relevance to Kaguya (design inference):** Compare fixed top-k selection with
relevance thresholds, reranking, and selection under a token budget. Candidate
recall and final answer quality are separate concerns: a larger search pool need
not become a larger prompt. Thresholds require calibration to the chosen scorer;
a token budget bounds size but does not establish relevance. Evaluate these
choices on conversational memories, including turns with no useful match,
alongside voice latency and prompt cost. Rank fusion is covered by REF-007;
representation and compression by REF-010. Current retrieval settings live in
[gateway/gateway.toml](gateway/gateway.toml); these sources do not validate their
defaults or imply that the alternatives are implemented.

---

## REF-009 — Lexical Retrieval for Multilingual Memory

**Question:** Which tokenization and normalization choices preserve useful
lexical matches across English, Chinese, and mixed-language memories?

**References:**

- SQLite. [FTS5 tokenizers](https://www.sqlite.org/fts5.html#tokenizers) and
  [BM25 ranking](https://www.sqlite.org/fts5.html#the_bm25_function).
  FTS5 separates tokenization from ranking. Its default `unicode61` tokenizer
  groups consecutive token characters; Unicode support does not supply Chinese
  word segmentation. Porter stemming is designed for English. The separate
  trigram tokenizer supports substring matching, with limitations for short
  queries. These are distinct retrieval behaviors, not interchangeable forms
  of multilingual support.
- Unicode ICU. [Boundary Analysis](https://unicode-org.github.io/icu/userguide/boundaryanalysis/).
  ICU supplements word-boundary rules with dictionaries for languages including
  Chinese and Japanese. This provides a concrete alternative to treating an
  uninterrupted sequence of characters as a single token.

**Relevance to Kaguya (design inference):** Compare English stemming,
language-aware word segmentation, and character n-grams on names, identifiers,
short Chinese queries, and mixed-language utterances. Assess missed matches and
false matches alongside index size and cross-platform dependency cost. The
current `porter unicode61` choice in [the memory store](gateway/src/rag/store.rs)
is not evidence that Chinese substring retrieval works adequately. Revisit it
based on retrieval failures rather than an arbitrary corpus-size threshold.
Combining lexical and semantic rankings remains REF-007's concern.

---

## REF-010 — Memory Fidelity and Context Compression

**Question:** Where should information reduction happen: memory formation,
indexing, retrieval, or prompt assembly? What information remains recoverable?

**References:**

- Park, J. S. et al. (2023). [Generative Agents: Interactive Simulacra of Human Behavior](https://arxiv.org/abs/2304.03442).
  Stores observations and higher-level reflections in a memory stream;
  reflections retain pointers to supporting memories. This is an example of
  derived abstractions coexisting with their evidence, evaluated for simulated
  agents rather than factual reliability in a personal assistant.
- Anthropic (2024). [Introducing Contextual Retrieval](https://www.anthropic.com/engineering/contextual-retrieval).
  Shows how document chunks can lose referents and situational context. It adds
  chunk-specific context before embedding and lexical indexing. This informs
  how to preserve meaning when splitting source material; it does not establish
  that every memory should be shortened or summarized.
- Jiang, H. et al. (2023). [LLMLingua: Compressing Prompts for Accelerated Inference of Large Language Models](https://arxiv.org/abs/2310.05736).
  Evaluates budget-controlled, token-level prompt compression on reasoning,
  conversation, and summarization datasets. It supplies an alternative to a
  simple length cutoff, with measured quality/cost trade-offs in those settings,
  not a guarantee of lossless compression.

**Relevance to Kaguya (design inference):** Distinguish retained source
conversation, derived memories, searchable representations, and the context
selected for one turn. Truncation discards a suffix without judging meaning;
chunking preserves text across pieces but can separate its context; extractive
selection retains chosen wording; summarization creates a compact interpretation
that needs checking against its source. A derived memory can be concise while
the source remains available. Recoverability depends on retaining and linking
that source, not merely on choosing storage-time versus output-time reduction.

Compare these approaches for preservation of names, reasons, qualifications,
temporal changes, and contradictory evidence, as well as retrieval quality and
latency. Changing prompt assembly can recover omitted material only if it was
retained elsewhere. Current character caps in
[gateway/gateway.toml](gateway/gateway.toml) are implementation limits, not a
research-backed fidelity policy or a guarantee of source retention. REF-008
covers how much retrieved material is selected.

---

## REF-014 — Isolation Models for Agent Code Execution

**Question:** What isolation does model-authored execution require, and how do
the available approaches trade access control, resource containment, persistent
state, startup cost, and platform support?

**References:**

- Docker. [Docker Engine security](https://docs.docker.com/engine/security/).
  Describes namespaces, resource controls, capabilities, and the daemon's attack
  surface. Mounts and privileges can weaken containment; resource limits do not
  themselves prevent access to data. This supports evaluating the configured
  boundary rather than treating the container label as a security guarantee.
- Bubblewrap. [Sandbox security](https://github.com/containers/bubblewrap#sandbox-security).
  A Linux tool for constructing sandbox environments whose protection depends
  on the caller's policy and arguments. Filesystem exposure and namespace choices
  are part of the security model, not properties established by invoking the
  executable alone.
- Microsoft. [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects).
  Documents grouped process management, resource limits, and termination.
  Process security restrictions are separate. A Job Object by itself should
  not be treated as a filesystem or network access boundary.
- gVisor. [Introduction to gVisor security](https://gvisor.dev/docs/architecture_guide/intro/).
  Explains an application kernel that handles workload system calls in
  userspace, and contrasts that boundary with host-kernel primitives and virtual
  machines. A reference for alternative isolation models, not a claim that
  Kaguya implements gVisor or a recommendation to adopt it.

**Relevance to Kaguya (design inference):** Compare filesystem and network
authority separately from CPU/memory limits and process cleanup. Ordinary host
execution needs an explicit account of its inherited authority. Persistent
execution state may support multi-step work, but its scope and reset policy
need separate consideration from process isolation. Assess startup/runtime
cost and compatibility on each intended host without assuming equal guarantees
across backends or a universally strongest option. Provider ownership, current
backend support, and resource defaults belong in the
[implementation plan](docs/implementation-plan-v0.1.0.md) and
[runtime configuration](config/kaguya.runtime.toml).
