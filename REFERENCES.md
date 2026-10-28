# Project Kaguya — Design Bibliography

This bibliography collects external research and industry practice relevant to Kaguya's significant design questions. Entries explain what each source establishes and how it informs the project; a source need not settle the question or validate our chosen defaults.

Keep source findings distinct from project inference. Internal approvals, implementation history, routine tooling choices, configuration inventories, and test receipts belong in specifications, configuration documentation, or the implementation plan. Improve existing entries as evidence develops, preserve their REF IDs, and do not reuse deleted IDs.

---

## REF-000 — Concurrent Reasoning and Speech

**Question:** How can reasoning and speech overlap while keeping conversational
latency, interruption and task ownership explicit?

- [Mind-Paced Speaking (Wu et al., 2025)](https://arxiv.org/abs/2510.09592)
  separates formulation from articulation in a spoken-language model. It supports
  investigating concurrent reasoning/speech, but does not validate Kaguya's
  process topology or its latency on desktop hardware.
- [LTS-VoiceAgent (Zou et al., 2026)](https://arxiv.org/abs/2601.19952) studies
  semantic triggering and incremental reasoning with background and foreground
  roles. Its evaluation motivates testing partial-input handling; Kaguya has not
  implemented or reproduced that framework.
- [PredGen (Li and Grover, 2025)](https://arxiv.org/abs/2506.15556) evaluates
  input-time speculative generation in simulated voice interactions. It informs
  the prefill/speculation question, without establishing a local speedup or a
  safe interruption policy for this repository.
- [StreamingThinker (Tong et al., 2025)](https://arxiv.org/abs/2510.17238) studies
  reasoning while input arrives, including training and inference changes.
  This is research context, not a drop-in property of Kaguya's HTTP LLM client.
- [Qwen2.5-Omni (Xu et al., 2025)](https://arxiv.org/abs/2503.20215) uses a
  Thinker-Talker model architecture for streaming text and speech. Its model-level
  roles are different from Kaguya's service-level Talker/Reasoner split.
- [Project Airi](https://github.com/moeru-ai/airi) is a source reference for a
  local conversational character application and voice integration. Reusing its
  architectural ideas still requires inspecting the relevant implementation;
  project branding or popularity does not establish behavior or performance.

**Kaguya inference:** Separate conversational response from long-running work,
but establish cancellation, provenance and acceptance tests in Kaguya itself.
No cited result validates an end-to-end latency target or a delivered Reasoner.

---

## REF-001 — Post-Response Silence and Re-engagement

**Question:** When should an assistant speak again after its own response?
This differs from deciding whether the user has finished an utterance (REF-004).

- [Stivers et al. (2009), *Universals and cultural variation in turn-taking in
  conversation*](https://pmc.ncbi.nlm.nih.gov/articles/PMC2705608/) analyzes
  human turn transitions across languages. It supports the importance of timing
  and context; it does not prescribe an assistant's re-engagement timers.
- [Levinson and Torreira (2015), *Timing in turn-taking and its implications for
  processing models of language*](https://doi.org/10.3389/fpsyg.2015.00731)
  reviews human conversational timing and prediction. Human gap distributions
  are not interchangeable with desktop assistant inactivity policies.
- [Dialogflow CX advanced speech settings](https://docs.cloud.google.com/dialogflow/cx/docs/concept/advanced-speech)
  distinguishes no-input timeouts from endpointing and barge-in. These are
  configurable service settings, not evidence for Kaguya's timer values.

**Kaguya inference:** Re-engagement needs user/context evaluation independently
of turn detection. Current values and the enable switch belong in
[gateway.toml](gateway/gateway.toml) and [Gateway configuration](gateway/src/config.rs).
The sources do not validate the project's short/medium/long thresholds or imply
that an elapsed timer means the user has disengaged.

---

## REF-004 — Deciding When an Utterance Is Complete

**Question:** How should end-of-turn detection combine silence and incomplete
language without prematurely answering pauses in speech?

- [Levinson and Torreira (2015)](https://doi.org/10.3389/fpsyg.2015.00731)
  reviews prediction and timing in human conversation. It motivates considering
  more than elapsed silence, without establishing a universal numeric cutoff.
- [Dialogflow CX smart endpointing](https://docs.cloud.google.com/dialogflow/cx/docs/concept/advanced-speech#enable_smart_endpointing)
  analyzes partial input and can wait through a pause in an incomplete phrase.
  The documented behavior does not expose the internal classifier or validate
  claims about specific ASR-confidence/prosody mechanisms.
- [LiveKit turn detector documentation](https://docs.livekit.io/agents/logic/turns/turn-detector/)
  describes learned turn detection and its integration with a voice pipeline.
  Provider models, deployment requirements and evaluations must be checked for
  the selected version; they do not establish Kaguya accuracy figures.

**Kaguya inference:** The current [turn detector](talker/voice/turn_detector.py)
uses configurable silence/syntax heuristics from [Talker config](talker/config.py).
An unconditional timeout can split a deliberate pause into two utterances.
Inline barge-in can interrupt the resulting answer, but does not prove that
history contains only played speech or repair the premature interpretation.
Learned detection remains a separate implementation and acceptance decision.

---

## REF-006 — Spoken Response Brevity

**Question:** How much spoken output should a conversational generation produce
before yielding an opportunity for the user to respond?

- [Clark and Schaefer (1989), *Contributing to Discourse*](https://www.sciencedirect.com/science/article/pii/0364021389900086)
  treats contributions as jointly coordinated acts that establish understanding.
  This motivates incremental interaction, not a fixed sentence limit.
- [Amazon Alexa, *Be brief*](https://developer.amazon.com/en-US/alexa/alexa-haus/design-principles/be-brief)
  recommends concise language and responses that are easy to speak and follow.
  This is voice-interface design guidance, not a benchmark of Kaguya's users or
  TTS engine.

**Kaguya inference:** The configurable sentence cap in [Talker config](talker/config.py)
is a local policy enforced by the [soul container](talker/inference/soul_container.py).
Validate it against task completeness and interruption behavior. The sources do
not prove an optimal sentence count, synthesis speed, or that a longer response
should automatically delegate to the unimplemented Reasoner.

---

## REF-007 — Hybrid Retrieval via Reciprocal Rank Fusion

**Question:** How can lexical and vector rankings be combined without treating
their raw scores as comparable?

[Cormack, Clarke and Buettcher (2009), *Reciprocal Rank Fusion outperforms
Condorcet and individual Rank Learning Methods*](https://cormack.uwaterloo.ca/cormacksigir09-rrf.pdf)
evaluates a rank-based fusion method on retrieval datasets. It uses a smoothing
constant selected in a pilot and reports that results were not highly sensitive
to that choice. The study does not establish an optimal value for conversational
memory, multilingual queries, or Kaguya's corpus.

**Kaguya inference:** Rank fusion avoids directly calibrating BM25 and vector
score scales and tolerates an absent optional vector source. The implementation
is in [ranker.rs](gateway/src/rag/ranker.rs) and
[retriever.rs](gateway/src/rag/retriever.rs). Retrieval quality and tuning remain
project-specific measurements; the paper's settings are not validated defaults.

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
