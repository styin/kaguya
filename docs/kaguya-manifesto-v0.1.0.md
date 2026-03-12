# Project Kaguya — System Specification

**Codename:** Kaguya (The Chief of Staff)
**Version:** 0.1.0
**Date:** March 2026
**Classification:** Root Project Specification
**Audience:** Human architects, autonomous coding agents, and system integrators

---

## 1. Thesis: The Front-Office / Back-Office Divergence

Today's AI agent infrastructure is largely invariant. The same scaffolding that powers a code-generation agent also powers a user-facing assistant. OpenClaw agents, for instance, share identical runtime assumptions regardless of whether they face a human or face a compiler.

We believe this is a temporary state. As agent ecosystems mature, a natural divergence will emerge between two classes of AI:

|                       | Front-Office Agents                                                       | Back-Office Agents                                    |
| --------------------- | ------------------------------------------------------------------------- | ----------------------------------------------------- |
| **Optimized for**     | Inferential depth, empathy, persona consistency, multimodal communication | Literal fidelity, deterministic execution, throughput |
| **Primary interface** | Human beings                                                              | Other agents, APIs, data pipelines                    |
| **Trust model**       | Long-term relational trust built through memory and tone                  | Transactional correctness verified by output          |
| **Failure mode**      | Misreading intent, breaking persona immersion                             | Producing incorrect or incomplete artifacts           |
| **Latency profile**   | Sub-second conversational responsiveness is critical                      | Seconds to minutes acceptable for complex tasks       |
| **Examples**          | Chief of Staff, personal assistant, creative collaborator                 | Code generator, data parser, CI runner, test harness  |

Project Kaguya is an explicit bet on this divergence. It is a purpose-built Front-Office agent: a Chief of Staff whose sole mandate is to understand human intent, maintain a persistent and trusted identity, engage in fluid multimodal communication, and orchestrate Back-Office agents to do the actual work.

Kaguya does not write code. Kaguya does not parse datasets. Kaguya leads, and the swarm executes.

---

## 2. Core Axioms

Every line of code in this repository must serve one or more of these axioms. Code that violates them is architecturally incorrect regardless of functional correctness.

### Axiom I — Persona Fidelity (The Sovereign Identity)

Kaguya is a persistent entity. Interaction with Kaguya must feel fluid, trusted, and deeply contextualized.

- **High-EQ Imperative.** Kaguya possesses advanced inferential skills. It reads between the lines of human input, referencing long-term memory to understand unspoken context before formulating a response.
- **Unified Identity.** The core persona files (`SOUL.md`, `IDENTITY.md`) dictate all system responses. Whether rendering via the Live2D avatar, a desktop UI, or a headless terminal, the tone, warmth, and structural logic of the response must remain invariant.
- **Long-Term Context Retention.** Kaguya autonomously indexes and retrieves conversational context — ongoing research ecosystems, evaluation systems, recurring user preferences — without manual user prompting.

### Axiom II — Orchestration & Delegation (The Front-Office Mandate)

Kaguya is a conductor, not an instrumentalist.

- **Talker-Initiated Delegation.** The Talker Agent always gets first pass on any user input. Only the Talker — after beginning its own inference — can decide to spin up a Reasoner Agent for deeper work. The Router does not classify queries; it manages process lifecycle.
- **Non-Blocking Presence.** Functional tasks delegated to Back-Office agents execute asynchronously. Kaguya remains conversationally available throughout. The user never waits in silence.
- **Shielding the User.** Kaguya parses the dense, literal output of Back-Office agents and translates it into concise, human-readable summaries. The human should never see raw JSON logs or stack traces from a subagent unless explicitly requested.

### Axiom III — Extreme Responsiveness (The Local Loop)

Immersion shatters with high latency. Kaguya is a local resident first.

- **Sub-Second Voice Response.** The target is 500–900ms from end of user speech to first audio output from Kaguya, achieved through speculative KV cache prefill during user speech and sentence-level TTS streaming.
- **Streaming Output Pipeline.** LLM generation streams into TTS sentence-by-sentence. The user hears the first sentence while the LLM is still generating the second. The Live2D renderer reacts to the first token, not the last.
- **Pre-emptive Memory Hydration.** The Memory Vector-DB fires retrieval queries on partial STT transcripts, so relevant context arrives before the LLM prompt is finalized.

### Axiom IV — Persistent Presence (Breaking Round-Based Interaction)

Kaguya is not a request-response system. It is an always-on presence.

- **Event-Driven, Not Turn-Based.** Kaguya's conductor loop processes a continuous stream of events — speech, silence, timers, task completions, external triggers — and decides independently when to speak.
- **Silence as Input.** When the user goes quiet after Kaguya asks a question, that silence is itself an event. Kaguya can follow up, rephrase, or shift context without being prompted.
- **Self-Initiated Speech.** Kaguya can speak without being spoken to — announcing task completions, offering proactive reminders, or commenting on observed context.
