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
- 📝 **Memory system**: Hybrid retrieval — BM25 full-text search over a SQLite-backed memory store, with optional vector embeddings for semantic recall

---

## How it works

### The three-process architecture

**🦀 Gateway (Rust)** — The conductor

- Deterministic orchestrator that routes all events through a priority queue
- Manages conversation state, the RAG memory store, and tool dispatch
- Never does LLM inference—just pure coordination
- Handles barge-in inline on the same bidi stream as inference, no separate RPC roundtrip

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

- ✅ Proto schema finalized; bidi `Converse` and `Stream` topology in place
- ✅ Gateway core: event routing, RAG memory store (SQLite + FTS5 BM25), tool registry, silence timers
- ✅ Listener: VAD + STT (faster-whisper), rule-based turn detection, raw TCP audio socket
- ✅ Talker inference: prompt formatter, soul container, Kokoro TTS, sentence streaming
- ⏳ Reasoner adapters (OpenClaw, Claude Code) and end-to-end smoke flows still in progress
- 📋 Full implementation plan: [`docs/implementation-plan-v0.1.0.md`](./docs/implementation-plan-v0.1.0.md)

**Contributions are welcome** — the core voice pipeline runs locally and we're now hardening it. Open issues if you spot architectural concerns or want to chat about a contribution.

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

- **Rust** (`rustup`) — for the Gateway
- **Python 3.11+** + **uv** — for the Talker (uv manages the Python interpreter and venv)
- **Node.js 20+** with **npm** — for the Reasoner / Toolkit
- **`buf`** + **`protoc`** — for proto generation and lint (Rust uses tonic-build's eager generation; Python regen is optional, stubs are committed)
- **llama.cpp** (or any OpenAI-compatible server) running Qwen3-8B at `http://localhost:8080`
- **CUDA-capable GPU** with 12+ GB VRAM recommended for the LLM

### Installation

#### macOS (Apple Silicon or Intel)

```sh
# 1. System tools via Homebrew
brew install uv buf protobuf portaudio opus
# Rust toolchain (or `brew install rustup-init && rustup-init -y`)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# 2. Optional Python tooling installed as isolated CLIs
uv tool install ruff
uv tool install mypy

# 3. Per-component setup
git clone <this-repo> kaguya && cd kaguya
cd talker   && uv sync --dev   && cd ..
cd reasoner && npm install     && cd ..
cd gateway  && cargo build     && cd ..
```

#### Linux / WSL

Same uv / cargo / npm flow, but install the system libraries via your
distro:

```sh
sudo apt install build-essential portaudio19-dev libopus-dev espeak-ng \
                 protobuf-compiler
curl -LsSf https://astral.sh/uv/install.sh | sh
# rustup as above; buf via https://buf.build/docs/installation
```

#### Windows

`opus.dll` is bundled in `talker/native/win32/`. Install Python 3.11+,
[uv](https://docs.astral.sh/uv/getting-started/installation/),
[rustup](https://rustup.rs/), Node.js 20+, and
[buf](https://buf.build/docs/installation) via your preferred method,
then run the per-component commands above (PowerShell or WSL).

#### Running

In separate terminals:

```sh
# Talker
cd talker && uv run python main.py

# Gateway
cd gateway && cargo run

# Open the dev WebSocket
echo '{"type":"text","content":"hello"}' | websocat -n1 ws://127.0.0.1:8080/ws
```

For per-component details and gotchas (macOS port-binding, audio
passthrough on WSL2, etc.), see [`talker/README.md`](./talker/README.md)
and the specs in [`docs/`](./docs/).

---

## Roadmap

### Phase 1: Core Voice Pipeline _(in progress)_

- [x] Proto schema finalized (bidi `Converse` + `Stream`, RAG retrieval results)
- [x] M0: Proto generation + buf lint
- [x] M1: Gateway core (event routing, state management, turn lifecycle, RAG memory store)
- [x] M2: Listener (VAD, STT, turn detection, raw TCP audio socket)
- [x] M3: Talker inference (LLM, soul container, TTS, sentence streaming)
- [ ] M4: Toolkit (sandboxed tools, MCP integration) — partial; `run_command` disabled pending allowlist
- [ ] M5: Reasoner adapters (OpenClaw, Claude Code)
- [x] M6: Local dev interface (WebSocket endpoint for text + audio)
- [ ] M7: Integration tests (end-to-end smoke flows)

**Goal:** End-to-end voice conversation with tool calling and task delegation, running entirely locally.

### Phase 2: Production Features _(planned)_

- OpenPod protocol integration for multi-modal transport
- Partial transcript prefill (reduce latency by prefilling during user speech)
- LLM-based memory extraction to replace the keyword-trigger pass
- Vector embedder beyond the current optional local endpoint (cloud fallback, batched indexing)
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

**Persona as files, memory as a store:** `SOUL.md` defines personality and `IDENTITY.md` defines constraints — both are plain Markdown that the Gateway watches and re-delivers to the Talker on edit. Long-term memory (user facts, preferences, project context) lives in a SQLite + FTS5 store (`data/kaguya.db`) populated post-turn from the conversation; retrieval uses BM25 with optional vector embeddings, fused via Reciprocal Rank Fusion before being injected into the next turn's context.

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
