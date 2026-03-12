<div align="center">

# Kaguya

**A voice-first AI Chief of Staff that runs on your hardware.**

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/Gateway-Rust-orange.svg)](gateway/)
[![Python](https://img.shields.io/badge/Talker-Python-blue.svg)](talker/)
[![TypeScript](https://img.shields.io/badge/Reasoner-TypeScript-blue.svg)](reasoner/)

</div>

---

## What is Kaguya?

Kaguya is a self-hosted conversational AI designed to be your personal Chief of Staff. Talk to it naturally, delegate complex tasks, and keep everything running locally on your own machine.

### Why build another voice AI?

Most voice assistants fall into two camps:

1. **Cloud services** that are fast but send your data to someone else's servers
2. **Local LLMs** that respect privacy but feel slow and clunky

Kaguya splits the difference with a **two-tier architecture**:

- **Fast path** for conversation: Local 8B model responds in <700ms
- **Slow path** for real work: Delegates complex tasks to more capable agents (OpenClaw, Claude Code) when needed

The result: Responsive voice interaction that doesn't compromise on capability, all running on your GPU.

### Core features

- 🎤 **Voice-first**: Built around natural conversation with robust turn detection and barge-in support
- ⚡ **Local execution**: Everything runs on your hardware (tested on RTX 5070 Ti, 16GB VRAM)
- 🧠 **Smart delegation**: Fast-path LLM (Qwen3-8B) knows when to delegate complex reasoning to specialized agents
- 🔧 **Tool integration**: Sandboxed execution environment with MCP support for extensibility
- 📝 **Memory system**: Persistent context across conversations (file-based in Phase 1, vector DB in Phase 2)

---

## How it works

### The three-process architecture

**🦀 Gateway (Rust)** — The conductor

- Deterministic orchestrator that routes all events through a priority queue
- Manages conversation state, memory files, and tool dispatch
- Never does LLM inference—just pure coordination
- Handles barge-in gracefully: every turn starts with a unified PREPARE signal

**🐍 Talker (Python)** — The voice

- Fast-path LLM (Qwen3-8B) for <700ms response time
- Listens with VAD + STT (faster-whisper), speaks with TTS (Kokoro)
- Emits structured action tags: `[TOOL:web_fetch(...)]`, `[DELEGATE:task]`, `[EMOTION:joy]`
- Soul container pattern inspired by Project Airi

**🟦 Reasoner (TypeScript)** — The deep thinker

- Handles slow-path delegation for complex tasks
- Wraps OpenClaw or Claude Code via an adapter pattern
- Streams progress back to Gateway for conversational narration

**🛠️ Toolkit (TypeScript)** — The hands

- Sandboxed tool execution with workspace isolation
- MCP client for extensibility

### Fast path example

> "What's the time?"

1. VAD detects speech → STT transcribes → Gateway assembles context
2. Qwen3-8B generates response (200-500ms to first sentence)
3. Kokoro synthesizes speech → you hear the answer in ~400-700ms total

### Slow path example

> "Find all TypeScript files in this repo that import React and tell me which components are most commonly used"

**Immediate response (Talker, <700ms):**

- "Let me analyze the codebase for you."

**Then, in the background:**

1. Qwen3-8B emits: `[DELEGATE:Search TypeScript files for React imports and analyze component usage]`
2. Gateway spawns Reasoner Agent (OpenClaw)
3. **You can keep talking** — Kaguya remains responsive while the Reasoner works
4. Reasoner searches files, parses imports, counts component usage (~30-60 seconds)
5. Gateway narrates progress: "I'm scanning through the TypeScript files now..."
6. Reasoner completes → Gateway dispatches result to Qwen3-8B
7. **Final response:** "I found 47 React components. The most commonly used are Button (23 imports), Layout (18 imports), and Card (15 imports)..."

**Key benefit:** The Talker stays responsive during long-running tasks. You're not blocked waiting for the analysis to complete.

For more architectural details: [`docs/spec-gateway-v0.1.0.md`](./docs/spec-gateway-v0.1.0.md) and [`docs/spec-agent-v0.1.0.md`](./docs/spec-agent-v0.1.0.md)

---

## Current Status

🚧 **Phase 1 is in active development**

- ✅ Proto schema finalized with gRPC best practices
- ⏳ Currently on M0 (proto generation + buf lint CI) → M1 (Gateway core)
- 📋 Full implementation plan: [`docs/implementation-plan-v0.1.0.md`](./docs/implementation-plan-v0.1.0.md)

**Contributions are welcome once M1-M3 land** and the core voice pipeline is running. We're building in the open—feel free to follow progress, open issues, or explore the architecture.

---

## Hardware Requirements

Kaguya targets local GPU inference. Tested on **RTX 5070 Ti (16 GB VRAM)**:

| Component                             | VRAM         |
| ------------------------------------- | ------------ |
| Qwen3-8B (Q4 quantization)            | ~5-6 GB      |
| faster-whisper (distil-large-v3 INT8) | ~1 GB        |
| Kokoro TTS (82M parameters)           | ~0.5 GB      |
| KV cache + overhead                   | ~2-3 GB      |
| **Total**                             | **~9-11 GB** |
| **Headroom**                          | **~5-7 GB**  |

Should work on most modern NVIDIA GPUs with 12+ GB VRAM. Cloud fallbacks (Deepgram, ElevenLabs) planned for Phase 2.

---

## Getting Started

### Prerequisites

- **Rust** (for Gateway)
- **Python 3.10+** (for Talker)
- **Node.js 20+ & pnpm** (for Reasoner and Toolkit)
- **llama.cpp** server running Qwen3-8B
- **CUDA-capable GPU** (12+ GB VRAM recommended)

### Installation

_Detailed setup instructions will be added as Phase 1 milestones complete. For now, see the implementation plan for the module-by-module build sequence._

---

## Roadmap

### Phase 1: Core Voice Pipeline _(in progress)_

- [x] Proto schema finalized
- [ ] M0: Proto generation + buf lint CI
- [ ] M1: Gateway core (event routing, state management, turn lifecycle)
- [ ] M2: Listener (VAD, STT, turn detection)
- [ ] M3: Talker inference (LLM, soul container, TTS)
- [ ] M4: Toolkit (sandboxed tools, MCP integration)
- [ ] M5: Reasoner adapters (OpenClaw, Claude Code)
- [ ] M6: Local dev interface (WebSocket TUI/GUI)
- [ ] M7: Integration tests

**Goal:** End-to-end voice conversation with tool calling and task delegation, running entirely locally.

### Phase 2: Production Features _(planned)_

- OpenPod protocol integration for multi-modal transport
- Partial transcript prefill (reduce latency by prefilling during user speech)
- ChromaDB for vector memory when MEMORY.md outgrows context window
- QLoRA fine-tuning for more consistent persona and action tags
- Custom TTS voice (Chatterbox or Qwen3-TTS)
- Learned turn detection model
- Cloud fallbacks (Deepgram STT, ElevenLabs TTS)

### Phase 3: Multi-User _(future)_

- Speaker diarization (pyannote.audio)
- Addressee detection
- Multi-party conversation support

---

## Why These Design Choices?

**Deterministic orchestrator:** The Gateway uses a priority queue and pure state machine logic—no LLM guesswork in the routing layer. This makes the system predictable and debuggable.

**Sentence-level streaming:** Instead of streaming individual tokens over gRPC, the soul container buffers and emits complete sentences. Reduces time-to-first-audio from 26s to ~13s while keeping the protocol clean.

**No pipeline framework:** We use RealtimeSTT and RealtimeTTS as component libraries, not Pipecat/LiveKit. Kaguya's multi-process topology (Gateway as conductor, Listener and Talker as separate asyncio tasks) doesn't fit pipeline frameworks' end-to-end models.

**gRPC with buf:** Proto schemas enforced via `buf lint` and `buf breaking` in CI. Catches backwards-incompatible changes automatically before they cause cross-language breakage.

**Persona as files:** `SOUL.md` defines personality, `IDENTITY.md` defines constraints, `MEMORY.md` stores facts. Gateway watches these files and delivers updates to Talker via gRPC. You can edit Kaguya's personality with a text editor.

For deeper architectural rationale, see the specs in [`docs/`](./docs/).

---

## Contributing

We're building Phase 1 in the open. Once M1-M3 land and the core pipeline is running, we'll be ready for broader contributions.

**How to help now:**

- Read the architecture in [`docs/spec-gateway-v0.1.0.md`](./docs/spec-gateway-v0.1.0.md) and [`docs/spec-agent-v0.1.0.md`](./docs/spec-agent-v0.1.0.md)
- Review the proto schema in [`docs/implementation-plan-v0.1.0.md`](./docs/implementation-plan-v0.1.0.md)
- Open issues if you spot architectural concerns or have questions

**Coming soon:** Contributing guidelines, code style guide, and milestone-specific contribution opportunities.

![Repobeats analytics](https://repobeats.axiom.co/api/embed/5de0b86f88b1336bcc65a041fce388355abaf37c.svg "Repobeats analytics image")

---

## License

MIT License — see [LICENSE](LICENSE) for details.
