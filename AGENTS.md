# Agent Instructions

## Project Context

Project Kaguya is a voice-first AI Chief of Staff. The canonical spec and implementation plan live in `docs/`:

- `docs/spec-agent-v0.1.0.md` — Listener + Talker Agent specification
- `docs/spec-gateway-v0.1.0.md` — Gateway specification
- `docs/spec-reasoner-v0.1.0.md` — Reasoner contract and adapter specification
- `docs/kaguya-manifesto-v0.1.0.md` — root product specification
- `docs/implementation-plan-v0.1.0.md` — Phase 1 implementation plan (single source of truth for build order)
- `REFERENCES.md` — Algorithmic design decisions with sources (see below)

## REFERENCES.md — Mandatory Maintenance

`REFERENCES.md` records the empirical basis for every explicit algorithmic or numerical decision (thresholds, timer values, algorithm choices, codec decisions, file organization decisions).

**Rules:**

- Before introducing any numeric threshold or non-obvious design decision, check if it already has an entry.
- After introducing one, add a new `## REF-NNN` entry to `REFERENCES.md` with rationale and sources.
- Never hardcode values that appear in REFERENCES.md — they must be configurable, with the REFERENCES.md value as the documented default.
- Do not edit existing REF entries unless correcting a factual error. Superseded decisions get a new entry referencing the old one.

## Architecture Invariants (Do Not Violate)

- Gateway has sole discretion over durable Kaguya-agent workspace and host capabilities. A
  Reasoner backend may access only a Gateway-authorized task workspace; Supervisor supplies
  volatile scratch space and cleanup, never independent host authority.
- Talker Agent is fully stateless — all context arrives via gRPC from Gateway each turn.
- Audio bytes never enter protobuf serialization at 50fps. They use the dedicated raw TCP
  stream between Gateway and Listener.
- Tokens never cross the gRPC boundary — only complete semantic units (sentences, tags).
- Gateway does not inspect or decode audio content.
- P0 control signals bypass the Input Stream entirely.

## Cross-Platform Support

Kaguya is intended to run across multiple desktop platforms, especially Windows and macOS. When writing or testing code, keep cross-OS behavior in scope:

- Treat paths as structured data, never as plain strings. Prefer `PathBuf` / `Path` in Rust, `pathlib.Path` in Python, and `path` / URL helpers in Node.js.
- Use platform abstraction layers for OS-specific behavior, such as `#[cfg(windows)]` / `#[cfg(unix)]` in Rust and `sys.platform` / `os.name` in Python.
- Review transports and process management for cross-OS tolerance: Unix sockets vs TCP/named pipes, signal semantics, process-tree termination, shell invocation, executable suffixes, and environment activation all differ by platform.

## Language per Component

| Component    | Language                            |
| ------------ | ----------------------------------- |
| Gateway      | Rust (tokio, tonic)                 |
| Talker Agent | Python (asyncio, grpcio)            |
| Reasoner     | Adapter/service language is backend-dependent |
| Toolkit      | Gateway-managed tool registry       |
| Proto schema | buf; Python stubs are committed and Rust stubs build with tonic |

## Workflow

- Run `make proto` after editing `proto/kaguya/v1/kaguya.proto` to refresh Python stubs and
  Rust build output. The Reasoner uses its adapter transport rather than generated stubs.
- Proto changes must pass `buf lint proto/` before committing.
- Implement milestones in order (M0 → M7). Each milestone produces a testable artifact.
