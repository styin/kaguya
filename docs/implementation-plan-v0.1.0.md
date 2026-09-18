# Project Kaguya — Implementation Plan

**Version:** 0.1.0

**Updated:** 2026-09-18

**Scope:** Existing voice-stack baseline, Gateway refactor, and the next application systems

**Progress evidence:** Original baseline inspection used `633ec49`. The approved R0 extraction is now applied on `gateway-refactor` after documentation commit `b4eecd4`; its Windows build/test evidence is recorded below. Other stages have not received fresh runtime acceptance.

## 0. How to Use This Plan

This document owns implementation order and the remaining-work checklist.
The original M0–M7 identifiers are retained below as historical baseline labels;
the active sequence is now R0–R8 (REF-025). This replaces the original linear
build order, including the instruction to build Reasoner before the new session,
policy, task, workspace, and execution foundations.

- `[x]` means the specifically described implementation or test code exists and was inspected. It does **not** certify a passing test run or a complete milestone.
- `[ ]` means implementation, a design decision, or acceptance validation remains.
- **[OPEN]** marks an unsettled choice. Resolve it before implementing dependent behavior; do not treat illustrative APIs, provider names, states, or thresholds as defaults.
- Each active stage includes an acceptance gate. Close it only with recorded validation evidence.
- New numerical defaults and non-obvious design decisions require an entry in [REFERENCES.md](../REFERENCES.md).

The [protobuf schema](../proto/kaguya/v1/kaguya.proto) is authoritative for current
wire messages. [Gateway §1.1](spec-gateway-v0.1.0.md#11-configuration-driven-application-composition-ref-024)
records the accepted composition-root design. The Gateway spec now separates
current behavior from accepted targets and remaining gaps. Older Endpoint spec
sections still need the R7 reconciliation; their old process diagrams and
directory tree must not override REF-013/015/024 or be read as current progress.

## 1. Ownership and Extension Boundaries

| Concern | Owner / boundary |
| --- | --- |
| Session identity, authoritative history, policy, task records, workspace association, execution binding | Ordinary Gateway application modules; no universal plugin contract required |
| Conversation handling | Gateway pipeline; structured context in, semantic events/actions out |
| Provider selection and assembly | Service configuration → `config.rs` → `app.rs` → injected capability interfaces (REF-024) |
| Gateway async tasks, connection readiness, reconnect and shutdown | Gateway `lifecycle/`; these async tasks are distinct from durable application tasks |
| Managed processes, process-tree termination, sandbox resources and backend enforcement | Rust Supervisor (REF-013/015) |
| Voice capture processing, STT, LLM prompt formatting, sentence/tag processing and TTS | Python Listener/Talker service; Gateway does not inspect or decode audio |
| Reasoner backend adaptation | TypeScript Reasoner service; Gateway owns task coordination and authorized bindings |
| Presentation and controls | Console consumes authoritative APIs/events; it does not own session, policy, task or workspace truth |

Preserve these constraints throughout the work:

- Keep one continuous Kaguya conversation possible across reconnects and restarts. Session identity does not require introducing a multi-chat product.
- Keep session, turn, tool request, application task, backend session, workspace, and execution-handle identities distinct.
- Keep authoritative history separate from derived RAG memories.
- Keep logical workspace association separate from cwd, checkout materialization, access policy, runtime allocation, and file deletion.
- Keep provider identity, capability identity, and process identity separate. One process/provider may supply several capabilities.
- Start new core responsibilities as module files. Add directories or packages only when their concrete implementation needs them.
- Keep P0 control outside the Input Stream; preserve the responsiveness requirement and verify it under slow work.
- Keep raw audio outside protobuf and tokens inside the inference service. Current IPC uses TCP; browser audio is currently PCM, with optional Opus decoding in Listener.
- Keep Windows and macOS behavior in scope, including structured paths, process trees, shell invocation and backend capabilities.

## 2. Existing Baseline and Progress

These are implementation facts, not a declaration that the original milestones
passed all acceptance tests. Remaining work is assigned to the active stages.

| Historical area | Source status | Remaining work |
| --- | --- | --- |
| M0 — Scaffolding/proto | Rust/Python/TypeScript scaffolding, canonical schema, generators and CI exist | Contract updates and fresh checks as each new system lands |
| M1 — Gateway | Priority queues, pipeline, in-memory history, persona, RAG, clients, timers and reconnect exist | R0–R5; durable history and identity are absent |
| M2 — Listener | Raw audio ingress, PCM/Opus handling, STT and turn detection exist | R7/R8 voice-path and cross-platform validation |
| M3 — Talker | Converse, inline barge-in, prompt formatting, LLM client, sentence/tag processing and local TTS exist | R7 browser TTS egress; R8 behavioral validation |
| M4 — Toolkit | Rust Gateway registry/filesystem tools and Supervisor `sandbox_exec` exist | R2/R4/R5 policy and workspace binding; future tool inventory remains open |
| M5 — Reasoner | Package configuration and Gateway client exist; no `reasoner/src/` implementation | R6, after R1–R5 |
| M6 — Endpoint/Console | React/Vite Console, Gateway WebSocket endpoint, process controls and log streaming exist | R7 domain views, recovery, audio completion and UI validation |
| M7 — Integration | Focused unit/contract tests and CI jobs exist | R8 system acceptance; no new end-to-end results claimed here |
| Supervisor additions | Process orchestration, sandbox backends, HTTP/SSE logs and telemetry exist | R5 binding/recovery integration and R7 observability |

### 2.1 Confirmed Implementation

- [x] Canonical protobuf schema and buf lint/breaking workflow exist. Python stubs are committed and generated by `talker/scripts/gen_proto.py`; Rust stubs are generated into build output by each crate's `build.rs` using vendored protoc. Reasoner scaffolding declares runtime proto loading. Sources: [schema](../proto/kaguya/v1/kaguya.proto), [Makefile](../Makefile), [proto CI](../.github/workflows/proto-lint.yml).
- [x] Gateway has P1–P5 queues plus separate P0 control, pure pipeline handlers and an action executor. Sources: [input stream](../gateway/src/core/input_stream.rs), [pipeline](../gateway/src/core/pipeline/).
- [x] In-memory `History`, turn state, context assembly, persona loading/watching and silence timers exist. Only SOUL.md and IDENTITY.md are watched. Sources: [core](../gateway/src/core/), [startup](../gateway/src/app.rs).
- [x] SQLite/BM25 RAG, optional embeddings, memory export and `RagCapability` injection exist. Current extraction is gated by user-intent/non-interrupted response handling. Sources: [RAG](../gateway/src/rag/), [capability](../gateway/src/capabilities/rag.rs), [handlers](../gateway/src/core/pipeline/handlers.rs).
- [x] Gateway connects to Listener and Talker servers, and has a Reasoner client. Gateway serves RouterControlService and, with `dev-console`, the endpoint. Sources: [clients](../gateway/src/clients/), [services](../gateway/src/services/), [voice service startup](../talker/main.py).
- [x] Gateway task supervision, connection readiness, reconnect and shutdown primitives exist. Source: [lifecycle](../gateway/src/lifecycle/).
- [x] Supervisor owns configured process launch/restart/termination and exposes process status, logs, resource samples and telemetry. Sources: [Supervisor](../supervisor/src/), [runtime configuration](../config/kaguya.runtime.toml).
- [x] Supervisor sandbox acquire/execute/release and native, Docker, Bubblewrap and feature-gated Windows Job Object backends exist. Their isolation guarantees differ. Sources: [sandbox](../supervisor/src/sandbox/), [contract tests](../supervisor/tests/sandbox_backend_contract.rs).
- [x] Gateway registers `list_files`, `read_file`, `write_file`, and conditionally `sandbox_exec`. File tools use Gateway's configured root; sandbox execution uses Supervisor scratch/container storage, not yet a shared logical-workspace binding. Sources: [tools](../gateway/src/tools.rs), [sandbox client](../gateway/src/sandbox/mod.rs).
- [x] Listener implements STT/VAD integration, thread-safe turn detection and a raw length-prefixed TCP audio server; Talker implements Converse, PrefillCache, UpdatePersona, inline BargeInAck and local TTS. Sources: [voice](../talker/voice/), [inference](../talker/inference/), [server](../talker/server.py).
- [x] Gateway tracks active Reasoner requests and cancellation in its client. When unavailable it currently emits simulated fallback progress/completion; this is not a working Reasoner. Source: [Reasoner client](../gateway/src/clients/reasoner.rs).
- [x] Console implements text input, streamed turn views, connection state, audio capture/playback components, process/app controls, and log snapshot/SSE display. Browser state is ephemeral; browser TTS has no producer yet. Sources: [Console application](../console/src/App.tsx), [regions](../console/src/regions/), [store](../console/src/store.ts), [Supervisor proxy](../console/server/plugin.ts).
- [x] Focused pipeline, reconnect/lifecycle, sandbox, prompt, sentence, turn-threading and gRPC-handshake tests and cross-platform CI definitions exist. Sources: [CI](../.github/workflows/ci.yml), [Talker tests](../talker/tests/), [Supervisor tests](../supervisor/tests/).

### 2.2 Known Gaps Carried Forward

- Fresh conversation UUID on Gateway startup; no durable transcript reload or server-authoritative session bootstrap for Console → R1/R7.
- Approval handling is a placeholder; no shared user-policy resolution/enforcement path → R2.
- Active Reasoner entries are volatile client state, not durable task management → R3.
- A configured filesystem root and sandbox scratch directories are not a workspace registry or execution binding → R4/R5.
- Reasoner package has no service/backend implementation; its configured on-demand launch is not a complete dispatch path → R6.
- TTS plays on the voice-service host; the browser playback pipeline has no Talker audio feed → R7.
- History truncation counts messages rather than complete exchanges; summarization and an aggregate context-size guard are absent. The old 200-character retrieval-cap assumption is invalid; output caps are optional → R1/R8.
- Long awaits inside an event-loop branch can delay P0 processing despite biased selection → R0/R8.
- MCP, `web_fetch`, `search_tools`, and a standalone TypeScript Toolkit are not implemented → R5 inventory decision and deferred scope.

## 3. Active Implementation Sequence

| Stage | Focus | Main prerequisites | Status |
| --- | --- | --- | --- |
| R0 | Structural extraction and documentation alignment | Existing baseline | Complete for this extraction; Windows checks passed; R8 responsiveness/live/cross-host acceptance remains open |
| R1 | Session: identity and persistence | R0 | In-memory precursor only |
| R2 | Policy management | R1 identity | Pending |
| R3 | Task management and lifecycle | R1; R2 approval semantics | Volatile delegation precursor only |
| R4 | Workspace management and lifecycle | R1/R3 association contracts | Configured-root precursor only |
| R5 | Sandbox hookup and execution binding | R2–R4; existing Supervisor | Backend infrastructure present; binding pending |
| R6 | Build Reasoner | R3/R5; capability/protocol decisions | Scaffolding/client only |
| R7 | Complete and update Console | APIs from R1–R6 | Existing UI partial |
| R8 | Integration and release validation | Acceptance gates from R0–R7 | Pending |

This is the default build order. Define and test each stage's APIs before
dependent behavior. Small Console/API slices may accompany their backend stage;
R7 remains the final Console completion gate. R8 scenarios should be exercised
as their dependencies become available, not deferred wholesale to the end.

### R0 — Structural Extraction and Documentation Alignment

**Goal:** Make application assembly explicit without changing behavior or
introducing a plugin framework.

- [x] Extract construction/configuration/startup wiring to `gateway/src/app.rs`; keep `main.rs` as entry point (REF-024/026).
- [x] Extract the existing event loop into `gateway/src/core/pipeline/run.rs`, retaining P0's separate control branch and existing handler/action boundaries.
- [x] Keep existing concrete provider construction in assembly and RAG use behind its capability; no generalized provider registry or new core systems are introduced by this extraction.
- [x] Keep provider background work under the existing lifecycle owner; preserve Supervisor process/sandbox ownership.
- [x] Keep new responsibilities as files in the existing structure; update `lib.rs` exports and `gateway/structure.txt` to match the actual extraction.
- [x] Reconcile stale Gateway spec claims about process spawning, client/server direction, interruption protocol, session identity, transport, memory behavior, and actual versus planned audio egress. Extraction status updated on 2026-09-18.
- [x] Identify blocking/long awaits on the P0 path and record the required scheduling/cancellation correction separately from mechanical file moves; preserve the responsiveness requirement.

**Acceptance:** Existing behavior remains covered after extraction. Run Gateway
format/build/tests including `dev-console`, inspect dependency direction, and
record any remaining P0 responsiveness gap for R8. Do not mark the extraction
complete solely because the files moved.

**Recorded validation (2026-09-18, Windows):** Both default and `dev-console`
builds passed with `--locked --offline`. `cargo test --locked --offline
--all-targets` passed 99 tests; adding `--features dev-console` passed 100 tests.
`cargo fmt --all -- --check` and `git diff --check` passed. The extracted
assembly, event loop and entry point matched the approved source transformations
after ignoring formatting outside literals/comments. No additional substantive
runtime changes were required. No live voice-stack or macOS/Linux run is claimed.

**Unchanged R8 follow-up:** The loop still awaits retrieval and history access,
then sequential action execution including provider calls and post-turn work.
P0 itself awaits Reasoner cancellation, Talker barge-in or lifecycle shutdown.
Biased selection cannot interrupt a selected branch's await. R8 must establish
and verify the scheduling/cancellation design for responsive control under slow
work; this extraction deliberately leaves those statements and their order
unchanged.

### R1 — Session: Identity and Persistence

**Goal:** A continuous conversation has durable identity and authoritative history
independent of process, connection, and transient backend lifetimes.

**[OPEN]:** Define what `session_id` and the existing `conversation_id` each mean,
whether they are the same identity, how a session is selected at startup, and how
the persistence format/schema is versioned. Preserve continuity without assuming
a multi-conversation UI or a particular storage/event-sourcing design.

- [ ] Define stable session identity, metadata and its relationship to turn/request/task IDs.
- [ ] Replace unconditional fresh startup identity with explicit create/load/resume behavior.
- [ ] Choose and implement durable history storage with a clear write boundary, ordering, error reporting and schema/migration policy.
- [ ] Persist user inputs, assistant responses and tool results with correlation; define treatment of incomplete turns and confirmed-spoken partial responses on interruption.
- [ ] Make transcript retention independent of Talker readiness and the current prompt window; define failed/queued input behavior rather than silently losing input.
- [ ] Restore history after restart and prevent duplicates during reconnect/retry; keep derived RAG ingestion separate from transcript persistence.
- [ ] Define recent-context selection in messages versus exchanges, retention/deletion behavior, and a compaction interface. Record the status of future LLM summarization separately.
- [ ] Expose authoritative session identity and history snapshot/resume APIs for Console; define how snapshots and live updates join without gaps or duplication.

**Acceptance:** Tests cover restart continuity, storage failure, interrupted
responses, duplicate delivery, and reconnect to the same session. A bounded
prompt window must not erase durable history. Detailed retention and recovery
choices remain open until recorded.

### R2 — Policy Management

**Goal:** Resolve authorization consistently for Gateway tools and Reasoner work,
with enforcement in the component that performs the operation.

**[OPEN]:** Policy precedence and scope; supported decisions and approval choices;
grant lifetime/revocation; pending-approval timeout/disconnect behavior. A grant
must have an explicit scope, but this plan does not prescribe policy defaults.

- [ ] Define the policy model, effective-policy resolution and configuration/storage ownership.
- [ ] Identify which operations require policy evaluation, including filesystem access, process execution and applicable network/tool restrictions.
- [ ] Implement a decision API using explicit session/task/workspace/operation context.
- [ ] Implement correlated approval requests/responses, expiry/cancellation semantics and rejection of stale or mismatched responses.
- [ ] Persist policy settings and any durable grants according to their agreed scopes; distinguish them from transient approval state.
- [ ] Route direct Gateway filesystem tools through the same applicable policy decisions; sandbox-only checks must not leave a parallel bypass.
- [ ] Pass resolved constraints to execution/backend adapters and reject unsupported enforcement requirements rather than silently weakening them.
- [ ] Expose effective policy, pending approvals and decision reasons to Console without leaking credentials.

**Acceptance:** Test allow/deny/approval paths, scope isolation, revocation,
replayed responses and unavailable enforcement. Changing a backend must not
change core authorization semantics implicitly.

### R3 — Task: Management and Lifecycle

**Goal:** Own application work independently of its provider connection,
Supervisor process, or Tokio task.

**[OPEN]:** Initial task scope (delegated Reasoner work versus additional tool
jobs), task state transitions, concurrency, retry/resume semantics and durable
recovery. A failed connection is not automatically a completed or failed task.

- [ ] Define task identity and links to session, initiating turn/request, workspace association and eventual execution binding.
- [ ] Define the lifecycle/state machine, terminal outcomes and cancellation/approval transitions.
- [ ] Separate task records and coordination from `clients/reasoner.rs`; keep connection/protocol handling in the client.
- [ ] Persist required task metadata, progress/result references and terminal outcomes; define reconstruction after Gateway/Supervisor/backend loss.
- [ ] Correlate and order provider events, handle duplicates/late events, and prevent completed/cancelled tasks from being accidentally revived.
- [ ] Separate cancel-one-task, stop-active-work, and application shutdown behavior; connect them to provider cancellation and Supervisor resource cleanup.
- [ ] Expose task lists/status/results and narration inputs through explicit APIs/events.
- [ ] Define how approval waiting and workspace changes interact with running tasks; defer allocation mechanics to R5.

**Acceptance:** Exercise concurrent tasks, targeted cancellation, delayed events,
provider disconnect, restart reconciliation and result retrieval. Assert that
task identity survives transport replacement and that simulated progress cannot
be recorded as genuine backend success.

### R4 — Workspace: Management and Lifecycle

**Goal:** Associate sessions/tasks with a logical workspace independently of how
files are materialized or accessed in an execution environment.

**[OPEN]:** Existing workspace versus task checkout/snapshot, live-edit
consistency, concurrent access, persistence/retention, and cross-host support.
Define the supported initial scope explicitly rather than promising every mode.

- [ ] Define stable workspace identity, metadata and structured path handling.
- [ ] Implement registration/lookup and session/task association with a deliberate reassociation policy.
- [ ] Define validation for missing/moved paths, symlinks, platform path semantics and unavailable hosts.
- [ ] Define workspace lifecycle operations, distinguishing detach/unregister/archive from file deletion, and access revocation from storage cleanup.
- [ ] Preserve a shared logical workspace for Talker tools and Reasoner tasks while allowing different runtime paths and permissions.
- [ ] Define how a task selects its workspace version/checkout and what prevents conflicting edits.
- [ ] Define materialization, result publication/export and cleanup requirements for R5, including failure handling.
- [ ] Expose workspace associations and lifecycle status to Console.

**Acceptance:** Test registration, association, moved/unavailable roots, task
references and lifecycle operations. Demonstrate that an association alone
neither grants access nor starts a sandbox or deletes files.

### R5 — Sandbox Hookup and Execution Binding

**Goal:** Connect session/task identity, workspace selection, effective policy,
runtime capabilities and environment-specific paths into an executable binding.

**Existing substrate:** Supervisor backends and opaque handles already exist;
extend that boundary rather than adding another process owner.

**[OPEN]:** Binding lifetime and granularity, sharing/reuse, path mapping,
local/remote storage consistency, publication/export semantics and recovery after
either process restarts. Do not equate binding release with workspace deletion.

- [ ] Define the binding request/result and validate prerequisites before acquiring runtime resources.
- [ ] Map logical workspaces to the selected backend's actual working directory/storage; current scratch/container storage does not provide this mapping.
- [ ] Extend Supervisor acquisition/execution/release contracts with the information needed for workspace and policy enforcement.
- [ ] Add backend capability checks and explicit unsupported-combination errors; distinguish native execution from OS-enforced isolation.
- [ ] Bind both Gateway file-tool access and Reasoner execution to the intended logical workspace and applicable policy.
- [ ] Implement cancellation, release, timeout/failure cleanup, restart reconciliation and orphan handling without silently deleting persistent workspace data.
- [ ] Keep backend/process/container details out of core pipeline logic; expose opaque identities and useful status to task management and Console.
- [ ] Define the canonical initial tool inventory. Resolve the old standalone TypeScript Toolkit proposal separately; do not silently reintroduce direct shell execution or present absent tools as available.
- [ ] Decide whether basic MCP registration/tool exposure belongs in this delivery; if included, implement it through the same policy/binding path. Large-registry tool search remains deferred.

**Acceptance:** For each supported backend/mode, verify intended file visibility,
write/network restrictions where promised, tool/Reasoner consistency, cleanup
and unsupported-policy rejection. Reuse and extend the existing sandbox contract
suite; document platform-specific guarantees and skipped deployment coverage.

### R6 — Build Reasoner

**Goal:** Deliver the actual TypeScript Reasoner service behind the existing
Gateway delegation boundary and the new task/execution foundations.

**[OPEN]:** Initial backend adapters, backend session/turn lifetime, required
context updates, approval event contract, result retention, and provider process
reuse. Earlier OpenClaw/Claude examples and illustrative Qwen selections are not
a finalized supported-provider set or a process-per-task requirement.

- [ ] Implement `reasoner/src/` service startup/configuration and runtime proto loading.
- [ ] Implement the Reasoner gRPC server; Gateway remains the client. Evolve Delegate/Interrupt/Telemetry contracts only as required by the agreed task, policy and binding models.
- [ ] Define a capability/adapter boundary for supported backends; keep backend selection outside the Gateway pipeline.
- [ ] Implement the first approved adapter using the authorized execution binding, with workspace mapping and applicable permission enforcement.
- [ ] Map backend progress, output, errors and completion to correlated task events; support context updates where required.
- [ ] Implement cancel/stop/shutdown and reconcile interrupted/disconnected backend runs with R3 lifecycle rules.
- [ ] Complete Supervisor-controlled startup/readiness for on-demand Reasoner use; surface failures before treating work as running.
- [ ] Make simulated fallback an explicit development behavior; normal unavailability must not produce a fake successful task.
- [ ] Expose backend readiness and useful task/result information to Console, including retrieval of full results when summaries are insufficient.
- [ ] Run proto generation/lint and adapter/contract tests after wire changes.

**Acceptance:** A real supported backend completes a delegated task in the
selected workspace under the applicable policy; progress, approval, cancellation,
failure and restart behavior are observable and correctly recorded. Test
provider substitution without adding provider-name branches to the pipeline.

### R7 — Complete and Update Console

**Goal:** Extend the existing Console into a usable interface for the new systems,
with accurate backend state and a complete voice path.

**Existing substrate:** React/Vite regions, event-derived turn views, WebSocket
reconnect, process controls and log streaming. Retain these where useful.

- [ ] Render authoritative session identity and durable history; resume after browser reload/reconnect without fabricated IDs, duplicated turns or dependence on an ephemeral event ring.
- [ ] Add task status/progress/results and task-specific cancellation, clearly distinguished from process controls and application shutdown.
- [ ] Add workspace association/status controls and lifecycle operations supported by R4.
- [ ] Add effective-policy visibility and approval interaction, including pending/expired/cancelled requests and backend-confirmed outcomes.
- [ ] Expose provider readiness, execution-binding state and relevant errors; keep credentials and backend implementation details out of ordinary user flows.
- [ ] Complete Talker TTS audio transport to Gateway and browser playback. Preserve raw-byte transport and Gateway's no-decoding invariant; align PCM/Opus contracts explicitly.
- [ ] Validate microphone input, playback, interruption/muting and codec/sample-rate configuration across the complete path. Resolve open-mic/PTT behavior before adding a PTT control.
- [ ] Connect useful Supervisor telemetry/metrics and correlation to the existing inspector/log views; retain observable task/turn identity across reconnects.
- [ ] Handle rejected/failed control requests and stale/disconnected process data visibly; derive source filters from actual runtime sources where appropriate.
- [ ] Update `console/README.md`, endpoint spec and message types to match the actual React regions, Rust Supervisor/proxy ownership and new APIs.
- [ ] Complete layout, keyboard/accessibility, loading/empty/error states and browser-level interaction verification; add meaningful UI/protocol tests and Console CI coverage.

**Acceptance:** Through Console, resume a session, associate a workspace, inspect
policy, approve/deny a request, observe/cancel a real task, inspect results and
recover from disconnection. Demonstrate a full browser voice turn and barge-in
with live dependencies; component presence alone is insufficient.

### R8 — Integration, Remaining Contracts and Release Validation

**Goal:** Verify the new systems together and preserve unfinished baseline
requirements instead of declaring them complete from unit tests alone.

- [ ] Resolve long-running awaits/control scheduling so P0 stop remains responsive during slow retrieval, provider calls and task work; measure and document the required budget before asserting a guarantee.
- [ ] Implement aggregate context-size accounting and a clear oversize response policy. Validate history, memory export, retrieval, tool results and metadata together; make the limit configurable.
- [ ] Review the inherited 3.5 MiB guard proposal against actual transport/model constraints and record rationale before adopting a default. Do not assume retrieval entries are capped at 200 characters.
- [ ] Verify full text/voice turns, inline interruption accounting, idle barge-in, tool-result continuation and the configured silence cascade.
- [ ] Verify RAG ingestion eligibility, retrieval, memory export, persona refresh and prefix prefill; measure prefill benefit rather than assuming it.
- [ ] Verify session/task recovery across browser reconnect, Gateway restart, Supervisor restart, unavailable provider and failed persistence.
- [ ] Verify authorization and workspace isolation/consistency across direct file tools and real Reasoner execution.
- [ ] Verify full-result retrieval, targeted cancellation and cleanup without duplicate terminal outcomes or lost history.
- [ ] Run relevant Rust/Python/TypeScript builds/tests, buf checks, browser tests and supported-host sandbox deployment checks; record results, versions and any skips.
- [ ] Reconcile the plan, specs, configuration examples and source tree after implementation. Close each stage only when its acceptance evidence is recorded.

**Acceptance:** Record results for the supported deployment configuration and
each stage's acceptance scenarios. Explicitly identify skipped coverage and
unresolved gaps; a green unit-test run or simulated provider output alone does
not establish system completion.

## 4. Validation Workflow

Run checks appropriate to the changed component; a documentation-only progress
update does not imply these commands have passed.

| Change | Checks / evidence |
| --- | --- |
| Gateway | `cargo fmt --check`, `cargo test --all-targets`, and `cargo test --all-targets --features dev-console` from `gateway/` |
| Supervisor/binding | Rust formatting/tests, existing sandbox contract suite, backend deployment checks on supported hosts |
| Talker | Python lint/type checks and relevant pytest suites; live STT/TTS validation separately |
| Reasoner | TypeScript build plus real service/adapter tests introduced in R6; `npm test --if-present` is not evidence of tests when no test script exists |
| Console | `npm run build`, UI/protocol tests introduced in R7, and actual browser interaction/audio checks |
| Proto | `make proto` regenerates Python and builds Gateway; also build Supervisor for its generated client; run `buf lint proto/` and applicable breaking-change checks |
| End-to-end | Scenario evidence from R8, with model/provider/backend/platform configuration and explicit limitations |

Existing CI definitions are in [.github/workflows/ci.yml](../.github/workflows/ci.yml)
and [proto-lint.yml](../.github/workflows/proto-lint.yml). Backend environment
requirements are documented in [sandbox deployment testing](sandbox-backend-deployment-testing.md);
telemetry scenarios are in [telemetry testing](telemetry-e2e-testing.md).

## 5. Open Decisions and Deferred Scope

The active-stage **[OPEN]** paragraphs are design work, not optional polish.
Record decisions in REFERENCES.md and update the relevant contract/spec before
building dependent behavior.

Retained empirical questions from the original plan:

- GPU contention and voice latency with STT, LLM inference and TTS running together.
- Voice selection and audio quality; the configured voice is not a completed listening evaluation.
- Action-tag reliability across supported models and whether additional training is warranted.
- Custom versus library sentence detection and its latency/quality tradeoffs.
- Barge-in spoken/unspoken precision: current code uses conservative sentence-level accounting; validate it and evaluate word-level tracking separately.
- RAG growth/scaling and memory-extraction quality. Keyword extraction exists; LLM-based extraction remains future work.
- History compaction strategy and speculative-prefill invalidation remain distinct from durable storage.

Deferred unless explicitly brought into scope:

- OpenPod integration and ambient host telemetry.
- Incremental partial-transcript prefill, speculative decoding and two-stage barge-in.
- A chosen dedicated vector database, model fine-tuning, custom TTS voices, learned turn detection and speaker diarization.
- Large-registry tool search and programmatic multi-tool scripting.
- General dynamic plugin discovery, hot-swapping and an everything-is-a-plugin runtime.
- Multi-user hosting and additional remote-workspace materialization modes beyond the initial R4/R5 scope.

Basic tool/MCP inventory must be resolved in R5; deferring advanced tool search
does not silently cancel the earlier basic MCP requirement.
