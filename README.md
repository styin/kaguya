<div align="center">

# Kaguya

**A voice-first AI Chief of Staff that runs on your hardware.**

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
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

- **Fast path** for conversation: A local model streams complete sentences to TTS.
- **Slow path** for real work: A planned Reasoner service will adapt more capable agents for long-running tasks.

The voice pipeline is implemented; Reasoner adapters and system acceptance remain work in progress. Latency depends on the selected models and host and has not been established as a supported performance guarantee.

### Core features

- 🎤 **Voice-first**: Built around natural conversation with robust turn detection and barge-in support
- ⚡ **Local execution**: The voice pipeline runs on your hardware
- 🧠 **Delegation contract**: Talker emits structured delegation requests; the Reasoner service is planned
- 🔧 **Tool integration**: Gateway file tools and Supervisor-owned execution backends; MCP is planned
- 📝 **Memory system**: Hybrid retrieval — BM25 full-text search over a SQLite-backed memory store, with optional vector embeddings for semantic recall

---

## How it works

### The supervised runtime architecture

**🧭 Supervisor (Rust)** — The launcher

- Constructs and monitors the configured process graph
- Owns Sandbox Provider selection, opaque handles, resource limits, and teardown
- Exposes runtime status and lifecycle controls to the frontend

**🦀 Gateway (Rust)** — The conductor

- Deterministic orchestrator that routes all events through a priority queue
- Manages conversation state, the RAG memory store, and tool dispatch
- Never does LLM inference—just pure coordination
- Handles barge-in inline on the same bidi stream as inference, no separate RPC roundtrip

**🐍 Talker (Python)** — The voice

- Fast-path LLM (Qwen3-8B by default) with sentence streaming
- Listens with VAD + STT (faster-whisper), speaks with TTS (Kokoro)
- Emits structured action tags: `[TOOL:web_fetch(...)]`, `[DELEGATE:task]`, `[EMOTION:joy]`
- Soul container pattern inspired by Project Airi

**🟦 Reasoner (TypeScript, planned)** — The deep thinker

- Handles slow-path delegation for complex tasks
- Wraps OpenClaw or Claude Code via an adapter pattern
- Streams progress back to Gateway for conversational narration

**🛠️ Tools (Rust today)** — The hands

- Tool semantics and result correlation stay in Gateway
- Code execution is delegated through a Supervisor-owned sandbox handle
- MCP integration and any standalone TypeScript Toolkit remain planned

### Fast path example

> "What's the time?"

1. VAD detects speech → STT transcribes → Gateway assembles context
2. Qwen3-8B generates complete sentences
3. Kokoro synthesizes speech on the voice-service host

### Planned slow path example

> "Find all TypeScript files in this repo that import React and tell me which components are most commonly used"

**Illustrative Talker response:**

- "Let me analyze the codebase for you."

**Then, in the background:**

1. Qwen3-8B emits: `[DELEGATE:Search TypeScript files for React imports and analyze component usage]`
2. Gateway dispatches to a Supervisor-managed Reasoner service
3. **You can keep talking** — Kaguya remains responsive while the Reasoner works
4. Reasoner searches files, parses imports, and counts component usage
5. Gateway narrates progress: "I'm scanning through the TypeScript files now..."
6. Reasoner completes → Gateway dispatches result to Qwen3-8B
7. **Final response:** "I found 47 React components. The most commonly used are Button (23 imports), Layout (18 imports), and Card (15 imports)..."

This is intended behavior. The Reasoner service is not implemented; unavailable delegation reports an error and does not simulate task completion.

For more architectural details: [`docs/spec-gateway-v0.1.0.md`](./docs/spec-gateway-v0.1.0.md) and [`docs/spec-agent-v0.1.0.md`](./docs/spec-agent-v0.1.0.md)

---

## Current Status

🚧 **Phase 1 is in active development**

- ✅ Shared proto schema; bidi `Converse` and Listener topology in place
- ✅ Gateway core: event routing, RAG memory store (SQLite + FTS5 BM25), tool registry, silence timers
- ✅ Listener: VAD + STT (faster-whisper), rule-based turn detection, raw TCP audio socket
- ✅ Talker inference: prompt formatter, soul container, Kokoro TTS, sentence streaming
- ⏳ Reasoner adapters (OpenClaw, Claude Code) and end-to-end smoke flows still in progress
- 📋 Full implementation plan: [`docs/implementation-plan-v0.1.0.md`](./docs/implementation-plan-v0.1.0.md)

**Contributions are welcome** — the core voice pipeline runs locally and we're now hardening it. Open issues if you spot architectural concerns or want to chat about a contribution.

---

## Hardware Requirements

Kaguya targets local GPU inference. These are planning estimates for a **16 GB VRAM** host; actual memory use depends on the model, context and runtime:

| Component                             | VRAM         |
| ------------------------------------- | ------------ |
| Qwen3-8B (Q4 quantization)            | ~5-6 GB      |
| faster-whisper (distil-large-v3 INT8) | ~1 GB        |
| Kokoro TTS (82M parameters)           | ~0.5 GB      |
| KV cache + overhead                   | ~2-3 GB      |
| **Total**                             | **~9-11 GB** |
| **Headroom**                          | **~5-7 GB**  |

Supported-host and model-specific validation remains open. Cloud fallbacks (Deepgram, ElevenLabs) planned for Phase 2.

---

## Getting Started

### Prerequisites

- **Rust** (`rustup`) — for the Gateway
- **Python 3.11+** + **uv** — for the Talker (uv manages the Python interpreter and venv)
- **Node.js 20+** with **npm** — for the developer Console
- **`buf`** — for proto linting (Rust builds use a vendored `protoc`; Python regen is optional and its stubs are committed)
- **llama.cpp / LM Studio** (or any OpenAI-compatible server) running Qwen3-8B at `http://localhost:1234`
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
cd console  && npm ci          && cd ..
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

Copy `config/kaguya.runtime.toml` beside itself as `config/kaguya.local.toml`.
The runtime root selects Gateway settings, explicit portable data paths, and
the instance-local manifest used by managed and standalone startup. Initialize
once from the repository root; persona templates are copied without modifying
their originals, and the instance starts with an empty memory store:

```sh
cargo run --manifest-path gateway/Cargo.toml -- init-instance --dry-run --runtime-config config/kaguya.local.toml
cargo run --manifest-path gateway/Cargo.toml -- init-instance --runtime-config config/kaguya.local.toml
```

`--dry-run` prints the paths init would use and creates nothing.
`init-instance` never copies a database from an earlier instance or a configured
external path. `fork-instance` is the explicit operation for copying a selected
instance with its existing persona and RAG data under new IDs.

The JSON output contains `manifest_reference`. Set that value as `[instance]
`manifest` in your local runtime file. `init-instance` and `fork-instance`
print their selection handoff and never rewrite deployment configuration.

Inspect the selected paths without launching processes or writing data:

```sh
cargo run --manifest-path gateway/Cargo.toml -- show-config --runtime-config config/kaguya.local.toml
```

For managed startup:

```sh
cargo run --manifest-path supervisor/Cargo.toml -- --runtime-config config/kaguya.local.toml
```

For direct debugging, first supply the voice terminal with the pair validated
from that same selected deployment. For example, in PowerShell from the repository root:

```powershell
$deployment = cargo run --quiet --manifest-path gateway/Cargo.toml -- show-config --runtime-config config/kaguya.local.toml | ConvertFrom-Json
$env:KAGUYA_INSTANCE_ID = $deployment.instance.instance_id
$env:KAGUYA_SESSION_ID = $deployment.instance.default_session_id
```

Missing or invalid IDs stop voice startup. Supervisor and Console launchers
supply these variables automatically. Then run in separate terminals:

```sh
# Talker (in the terminal configured above)
cd talker && uv run python main.py

# Gateway (from repository root)
cargo run --manifest-path gateway/Cargo.toml --features dev-console -- --runtime-config config/kaguya.local.toml

# Open the dev WebSocket
echo '{"type":"text","content":"hello"}' | websocat -n1 ws://127.0.0.1:8080/ws
```

For per-component details and gotchas (macOS port-binding, audio
passthrough on WSL2, etc.), see [`talker/README.md`](./talker/README.md)
and the specs in [`docs/`](./docs/). Deployment selection, validation, restore, and independent-copy behavior is
documented in [`docs/r1a-identity.md`](./docs/r1a-identity.md).

---

## Roadmap

### Phase 1: Core Voice Pipeline _(in progress)_

- [x] Shared proto schema (bidi `Converse` + Listener, RAG retrieval results)
- [x] M0: Proto generation + buf lint
- [x] M1: Gateway core (event routing, state management, turn lifecycle, RAG memory store)
- [x] M2: Listener (VAD, STT, turn detection, raw TCP audio socket)
- [x] M3: Talker inference (LLM, soul container, TTS, sentence streaming)
- [ ] M4: Tools — file tools and `sandbox_exec` exist; MCP and unified policy remain planned
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

**Sentence-level streaming:** The soul container buffers tokens inside the inference service and emits complete sentences over gRPC. TTS can begin before the full response completes.

**No pipeline framework:** We use RealtimeSTT and RealtimeTTS as component libraries, not Pipecat/LiveKit. Kaguya's multi-process topology (Gateway as conductor, Listener and Talker as separate asyncio tasks) doesn't fit pipeline frameworks' end-to-end models.

**gRPC with buf:** CI checks schema lint and formatting. Kaguya is unpublished; contract changes update all consumers together, without backwards-compatibility adapters or migration requirements.

**Persona as files, memory as a store:** `SOUL.md` defines personality and `IDENTITY.md` defines constraints — instance-owned copies are watched and re-delivered to the Talker on edit. Long-term memory (user facts, preferences, project context) lives in a SQLite + FTS5 store under `gateway/data/instances/<InstanceId>/kaguya.db`, populated post-turn from the conversation; retrieval uses BM25 with optional vector embeddings, fused via Reciprocal Rank Fusion before being injected into the next turn's context.

For deeper architectural rationale, see the specs in [`docs/`](./docs/).

---

## Contributing

We're building Phase 1 in the open. The active implementation sequence and acceptance gates are in the implementation plan.

**How to help now:**

- Read the architecture in [`docs/spec-gateway-v0.1.0.md`](./docs/spec-gateway-v0.1.0.md) and [`docs/spec-agent-v0.1.0.md`](./docs/spec-agent-v0.1.0.md)
- Review the [proto schema](./proto/kaguya/v1/kaguya.proto) and [implementation plan](./docs/implementation-plan-v0.1.0.md)
- Open issues if you spot architectural concerns or have questions

See [CONTRIBUTING.md](./CONTRIBUTING.md) for development and verification commands.

![Repobeats analytics](https://repobeats.axiom.co/api/embed/5de0b86f88b1336bcc65a041fce388355abaf37c.svg "Repobeats analytics image")

---

## License

GNU Affero General Public License v3.0 only (AGPL-3.0-only) — see [LICENSE](LICENSE) for details.
