# Kaguya Refactor Manifesto: Runtime, Gateway, Pipeline, Plugins

## Purpose

Kaguya is moving toward an unopinionated harness framework: modular, simple at
the core, and friendly to derivative products that can choose opinionated
defaults. The refactor should make ownership boundaries obvious before adding
more extension points.

This document is a local planning checkpoint. It records what has already moved
from plan to status quo, then lists the remaining work.

The high-level sequence remains:

1. Lifecycle and process supervision
2. Runtime config model
3. Gateway TurnPipeline
4. PluginSystem

Items 1–3 are implemented. PluginSystem is next.

## Design Inspirations

Kaguya should borrow selectively from existing harness and agent projects:

- AstrBot: staged turn pipeline, ordered handlers, and hook registry around
  inference-adjacent behavior.
- MaiBot: process supervision lifecycle with readiness, health checks,
  graceful shutdown, and terminate/kill fallback.
- Hermes-agent: minimal manifest plus register mental model for community
  extension.
- OpenClaw: typed capability surfaces and rollback discipline for failed
  plugin registration.

Kaguya is a polyglot monorepo, so runtime ownership, capability readiness, and
plugin boundaries must stay explicit about process boundaries.

## Status Quo

### Runtime Ownership

Process ownership has moved out of Gateway.

Supervisor now owns:

- app-mode process graph startup;
- process stop/restart/shutdown;
- process logs;
- restart policy;
- process snapshots;
- external runtime health polling;
- Gateway graceful drain during app shutdown;
- future sandbox wrapping.

Gateway now owns:

- the agent turn loop;
- P0 STOP semantics;
- Gateway-local shutdown/drain;
- Tokio task cancellation and joining;
- client connection readiness;
- reconnect policy;
- capability endpoint consumption.

This split is the new architectural invariant:

```text
Supervisor
  owns process lifecycle

Gateway
  owns agent loop and capability bindings

Provider runtimes
  own implementation internals
```

Gateway must not regain runtime process management.

### Runtime Config

`config/kaguya.runtime.toml` is now the shared runtime topology contract.

It defines runtime profiles:

- `app`: Supervisor-owned product/app graph.
- `dev_standalone`: debug profile for starting Gateway and Voice Stack
  independently from the dev console or CLI.

The runtime config carries:

- process identity and label;
- `enabled`;
- `launch = eager | on_demand | external`;
- `criticality = required | degraded_usable | optional`;
- `restart = never | on_failure | keep_alive`;
- command, Windows command, cwd, env;
- bind addresses for provider runtimes;
- connect endpoints for Gateway/Supervisor observers;
- provided capabilities;
- external health URLs and poll intervals.

Gateway reads only the topology fields it needs: selected profile, runtime
enabled state, launch mode, criticality, provided capabilities, capability
enablement, and endpoints.

Supervisor reads the process fields and converts runtime `bind` entries into
first-party runtime env vars. For Voice Stack this produces:

- `KAGUYA_TALKER_LISTEN_ADDR`;
- `KAGUYA_LISTENER_GRPC_ADDR`;
- `KAGUYA_LISTENER_AUDIO_ADDR`;
- `KAGUYA_LISTENER_AUDIO_PORT`.

Bind and endpoint ports are validated so provider-side bind config and
consumer-side connect config do not silently drift.

### Gateway Local Config

Gateway-local behavior config now lives in `gateway/gateway.toml`.

That file is for Gateway behavior only:

- Gateway websocket and gRPC bind addresses;
- soul, identity, and workspace paths;
- history limits;
- silence timer behavior;
- RAG storage and retrieval parameters.

It should not contain process graph or cross-runtime topology.

### Talker Local Config

`talker/config.py` is now component-local implementation defaults plus env
targets.

It should not define Gateway topology. Runtime bind addresses are defaults for
direct `python main.py` usage only; app mode and dev-console standalone mode
inject the values generated from `config/kaguya.runtime.toml`.

### Console Model

The console now supports two modes:

- App mode: console starts or talks to Rust Supervisor, and Supervisor manages
  app-owned processes.
- Standalone debug mode: console starts local Gateway or Voice Stack launch
  targets for debugging, using the `dev_standalone` runtime profile.

App mode and standalone mode block each other to avoid double-binding ports and
ambiguous ownership.

### P0 Control Signals

P0 control signals are not turn events. They remain outside the priority queue
and outside the future TurnPipeline.

STOP is Gateway-owned application interruption:

- cancel active Talker generation;
- cancel silence timers;
- cancel active Reasoner work;
- mute or suppress active output;
- clear active dispatch state;
- send Talker barge-in when applicable.

SHUTDOWN has two meanings by layer:

- Gateway SHUTDOWN means drain and exit Gateway-local work.
- Supervisor app shutdown means disable restarts, ask Gateway to drain, then
  stop app-owned runtimes and force-kill stubborn children after timeout.

Console app shutdown should call Supervisor, not Gateway directly.

## Completed Lifecycle Work

### Capability Bindings — Implemented

Recovery loops on `TalkerClient` and `ListenerClient` cover the binding
responsibilities originally planned for a separate `bindings.rs` layer:

- reconnect/rebind after connection loss;
- persona resend after Talker reconnect;
- Listener ASR stream recreation and audio sink install/clear;
- readiness tracking (Starting → Ready → Degraded → Stopped);
- startup grace for expected app-owned runtimes;
- bounded exponential backoff with degradation after policy exhaustion.

The separate `Binding` abstraction was deferred — recovery logic lives
directly on client structs in `gateway/src/clients/`.

### Reconnect Policy — Implemented

Recovery loops implement bounded exponential backoff, startup grace for
expected runtimes, lower-noise polling for external runtimes, and tests
that simulate runtime disappearance and reappearance.

### Lifecycle Tests — Implemented

All five originally listed tests are covered by recovery loop integration
tests (`recovery_loop_reconnects_after_degraded`,
`recovery_loop_reconnects_after_stream_loss`, etc.).

## Remaining Config Work

### Remove Fallback Policy — Done

`FallbackPolicy` removed from Gateway config. Reasoner fallback to stub is
now explicit behavior in `ReasonerManager` (policy exhaustion → stub mode).

### Finalize Profile Semantics

Keep the current profile model, but document the exact precedence:

```text
KAGUYA_RUNTIME_PROFILE
  overrides config/kaguya.runtime.toml profile

config/kaguya.runtime.toml selected profile
  defines process graph, bind addresses, endpoints, capabilities

runtime bind entries
  generate provider-runtime env vars

component-local config files
  provide implementation defaults only
```

### Decide Gateway Behavior Overlays

`gateway/gateway.toml` is correctly Gateway-local now. The remaining question is
whether derivative products need per-profile overlays for Gateway behavior.

For now, keep behavior config separate from topology. Add overlays only if a
real product profile needs different RAG, silence, history, or file-path
behavior.

### Config Tests Still Needed

Add or keep tests for:

- selected profile resolution in Gateway and Supervisor;
- invalid profile rejection;
- bind/endpoint port mismatch rejection;
- generated env overriding stale explicit env for first-party runtimes;
- standalone profile marking Voice Stack as external so Gateway does not spam
  expected-runtime startup logs;
- component-local configs remaining usable without runtime injection.

## Remaining Process Supervision Work

### Restart Exhaustion — Done

Supervisor circuit breaker implemented: sliding-window max restarts, terminal
errored state after exhaustion, manual reset via start/restart, clear log
reason for crash-loop suppression.

### On-Demand Runtime Startup

`launch = "on_demand"` exists as a config concept. It is not yet a full runtime
contract.

Future behavior:

- Gateway requests on-demand runtime start through Supervisor API;
- Supervisor starts the runtime;
- Gateway binding waits for capability readiness;
- idle shutdown can stop the runtime later.

Reasoner and future tool adapters are the first candidates.

### Sandbox Provider Interface

Sandbox config currently supports:

```toml
sandbox = { provider = "none", required = false }
```

Only `none` is implemented. The interface should be ready for future providers
such as OS-level sandboxing, but actual sandbox implementations should remain
per-process, not per-capability.

Future sandbox responsibilities:

- wrap process launch;
- enforce environment/path/network constraints where supported;
- fail startup when `required = true` and provider is unavailable;
- report sandbox status in process snapshots.

### Dependency And Health Policy

Supervisor dependency and health behavior still needs a sharper policy:

- distinguish hard dependency from startup ordering hint;
- make external health polling less chatty and configurable;
- decide how failed external dependencies affect app state;
- expose clear `blockedBy` reasons without expanding unrelated cards in the UI.

### Process Supervision Tests

Implemented:

- `manual_stop_suppresses_restart`;
- `shutdown_app_stops_children_when_gateway_drain_unavailable`;
- `restart_exhaustion_enters_errored_state`;
- `manual_start_resets_restart_exhaustion`.

Still needed:

- on-demand runtime start/stop through API;
- sandbox provider validation (unsupported required provider fails clearly).

## TurnPipeline — Implemented

The Gateway event loop is now structured as handlers + executor, living in
`gateway/src/core/pipeline/`.

### Architecture

Three layers:

1. **Handlers** (`handlers.rs`) — sync, pure decision functions. Each takes
   `&mut TurnState` plus pre-fetched data, returns `Vec<PipelineAction>`.
   Unit-testable without gRPC, SQLite, or tokio runtime.
2. **Executor** (`executor.rs`) — thin `ActionExecutor` struct that maps each
   `PipelineAction` to a real component call. Holds references to all Gateway
   components.
3. **Orchestrator** (`main.rs`) — unchanged `tokio::select! { biased; … }`
   loop. Receives events, pre-fetches async data, calls handlers, feeds
   actions to the executor.

P0 control signals remain inline in the select loop — they bypass the
pipeline entirely.

### Design Decisions

**No `GatewayEvent` envelope.** The manifesto originally proposed a unified
`GatewayEvent` / `InputPriority` enum. This was dropped because the biased
select loop already discriminates events by channel, and handlers are
separate functions per event variant. A unified envelope would be dead
ceremony unless a plugin system later needs a single event queue with
middleware.

**Free functions, not traits.** No plugin hooks yet → traits are premature
abstraction. Each handler is a standalone `pub fn` in `handlers.rs`.

**18 fine-grained `PipelineAction` variants** instead of the manifesto's
original 9. Each variant maps to exactly one I/O call, making test
assertions precise. The full set:

```rust
enum PipelineAction {
    DispatchTalker { context, kind },
    BargeIn,
    SendResponseStarted { turn_id },
    SendSentence { text },
    SendEmotion { emotion },
    SendResponseComplete { turn_id, was_interrupted },
    SendUserInput { text },
    MuteOutput,
    UnmuteOutput,
    AppendUserHistory { text },
    AppendAssistantHistory { text },
    AppendAssistantPartialHistory { spoken_text },
    AppendToolResultHistory { tool_name, content },
    DispatchTool { request_id, tool_name, args_json },
    StartReasoner { task_id, description },
    EvaluateAndStoreMemory { user_input, assistant_response, turn_id },
    UpdatePersonaIfChanged,
    PrefillCache,
    StartSilenceTimers,
}
```

**`TurnState`** holds the mutable per-conversation state that was previously
scattered as local variables in `main()`: `active_gen`, `active_silence`,
`current_response`, `last_turn_id`, `current_dispatch_kind`,
`last_memory_md`, `conversation_id`.

**Context assembly has sync variants.** `context::assemble_from_data()` etc.
take pre-fetched `Vec<ChatMessage>` and `Vec<ToolDefinition>` so handlers
can build `TalkerContext` without holding `&History` or `&ToolRegistry`.

### Priority semantics (unchanged)

- P0: control signals, bypassing the pipeline.
- TalkerOutput: handled before normal queued input (biased select).
- P1: user intent.
- P2: ASR/VAD states.
- P3: tool and Reasoner callbacks.
- P4: silence.
- P5: telemetry.

### Test coverage

28 handler unit tests covering all event variants, edge cases (talker not
ready, narration filtered, generation active, interrupted response, memory
persistence gating by dispatch kind).

## PluginSystem Plan

The v1 public plugin slot remains RAG only.

V1 runtime:

- in-process Rust capability, compiled into Gateway or derivative products;
- no dynamic sidecar loading yet;
- no Python plugin loading in Gateway;
- no dual-track public API yet.

Core interface shape:

```rust
#[async_trait]
pub trait RagProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;

    async fn retrieve(&self, request: RetrievalRequest) -> anyhow::Result<Vec<RetrievalHit>>;
    async fn evaluate_and_store(&self, request: MemoryWriteRequest) -> anyhow::Result<MemoryWriteResult>;
    async fn export_memory_md(&self) -> anyhow::Result<String>;
    async fn health(&self) -> CapabilityHealth;
}
```

Community maintainer flow for v1:

1. Implement a Rust crate or workspace module exposing a `RagProvider`.
2. Provide manifest-like metadata: id, version, description, config schema, and
   capability type.
3. Register through `CapabilityRegistry::register_rag_provider`.
4. Select the provider by config or compile-time feature in Gateway or a
   derivative product.

Future plugin expansion:

- `tool_provider` and `reasoner_adapter` next, because they operate on
  Gateway-owned structured objects.
- `llm_provider`, `tts_provider`, `stt_provider`, and `vad_provider` later,
  selected by runtime config but executed inside Talker or Listener.
- Sidecar plugins only after v1 succeeds, with lazy startup, exclusive-slot
  enforcement, idle shutdown, resource limits, health checks, and Supervisor
  ownership.

Important rule: bindings are framework infrastructure, not per-plugin burden.
Community plugins should implement known capability contracts. They should not
need custom Gateway binding code unless they introduce a new capability kind or
transport.

## Non-Goals For The Current Slice

- No public dynamic plugin loading.
- No sidecar plugin runtime.
- No Python plugin loading inside Gateway.
- No Talker InferencePipeline yet.
- No TurnPipeline public hook registry yet.
- No Gateway-owned process management.

## Near-Term Recommended Sequence

1. ~~Runtime config and Supervisor cleanup.~~ Done.
2. ~~Gateway capability bindings and rebind loops.~~ Done (recovery loops).
3. ~~Remove Gateway FallbackPolicy.~~ Done.
4. ~~Supervisor restart exhaustion.~~ Done.
5. ~~TurnPipeline extraction from main.rs.~~ Done.
6. Introduce RAG provider trait and registry.
