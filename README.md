# Project Kaguya

Kaguya is a multi-modal, agentic Chief of Staff powered by local models and a deterministic routing architecture.

## Repository Setup

Please review `docs/spec-gateway-v0.1.0.md` and `docs/spec-agent-v0.1.0.md` for architectural details.

This workspace comprises:

- `gateway`: Rust conductor and conversational orchestrator.
- `talker`: Python component for listening (VAD/STT) and speaking (LLM/TTS).
- `reasoner`: TypeScript adapters for slow-path cognitive delegation.
- `tools`: TypeScript sandboxed functions and MCP clients.
