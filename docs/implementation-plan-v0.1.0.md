# Project Kaguya — Implementation Plan

**Version:** 0.1.0

**Updated:** 2026-10-30

**Scope:** Existing voice-stack baseline, Gateway refactor, and the next application systems

**Progress evidence:** R1a instance/deployment closure and the fixed-scope protocol cleanup are implemented on `sessions` with focused Windows validation. The R1a review follow-up (single in-process scope, process-boundary checks only, per-user instance locks, `init-instance --dry-run`, `fork-instance`) has Linux validation (§2.3); supported-host/deployment acceptance remains open. The selected R1b generation/session/utterance renames are implemented; the complete naming inventory and acceptance remain open. R1c and the accepted R1d follow-up remain unimplemented.

## 0. How to Use This Plan

This document owns implementation order and the remaining-work checklist.
The original M0–M7 identifiers are retained below as historical baseline labels;
the active sequence is now R0–R8. This replaces the original linear
build order, including the instruction to build Reasoner before the new session,
policy, task, workspace, and execution foundations.

- `[x]` means the specifically described implementation or test code exists and was inspected. It does **not** certify a passing test run or a complete milestone.
- `[ ]` means implementation, a design decision, or acceptance validation remains.
- **[OPEN]** marks an unsettled choice. Resolve it before implementing dependent behavior; do not treat illustrative APIs, provider names, states, or thresholds as defaults.
- Each active stage includes an acceptance gate. Close it only with recorded validation evidence.
- Cite external research or industry practice for significant design questions in [REFERENCES.md](../REFERENCES.md). Keep project decisions, numerical defaults, and validation evidence in their relevant specs, configuration documentation, or this plan.

The [protobuf schema](../proto/kaguya/v1/kaguya.proto) is authoritative for current
wire messages. [Gateway §1.1](spec-gateway-v0.1.0.md#11-configuration-driven-application-composition)
records the accepted composition-root design. The Gateway spec now separates
current behavior from accepted targets and remaining gaps. The [Endpoint spec](spec-endpoint-v0.1.0.md) describes the current Console
contract and identifies the deferred R7 surfaces.

## 1. Ownership and Extension Boundaries

| Concern | Owner / boundary |
| --- | --- |
| Session identity, authoritative history, policy, task records, workspace association, execution binding | Ordinary Gateway application modules; no universal plugin contract required |
| Conversation handling | Gateway pipeline; structured context in, semantic events/actions out |
| Provider selection and assembly | Service configuration → `config.rs` → `app.rs` → injected capability interfaces |
| Gateway async tasks, connection readiness, reconnect and shutdown | Gateway `lifecycle/`; these async tasks are distinct from durable application tasks |
| Managed processes, process-tree termination, sandbox resources and backend enforcement | Rust Supervisor |
| Voice capture processing, STT, LLM prompt formatting, sentence/tag processing and TTS | Python Listener/Talker service; Gateway does not inspect or decode audio |
| Reasoner backend adaptation | TypeScript Reasoner service; Gateway owns task coordination and authorized bindings |
| Presentation and controls | Console consumes authoritative APIs/events; it does not own session, policy, task or workspace truth |

Preserve these constraints throughout the work:

- Bind a durable `InstanceId` to service configuration and select one default `SessionId` for its continuous conversation. Keep the identities distinct; ordinary configuration edits and process restarts preserve both. Additional sessions remain future work.
- Keep instance, session, generation, conversational turn, tool request, application task, Reasoner run, backend session, workspace, and sandbox-allocation identities distinct. Cancellation tokens are control objects, not durable identities.
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
| M0 — Scaffolding/proto | Rust/Python components, Console, canonical schema, generators and CI exist | Contract updates and fresh checks as each new system lands |
| M1 — Gateway | Pipeline, in-memory history, persona/RAG, clients, and R1a durable instance/session identity exist | Durable transcript/recovery remains R1c; policy/task/workspace/binding remains R2–R5 |
| M2 — Listener | Raw audio ingress, PCM/Opus handling, STT and turn detection exist | R7/R8 voice-path and cross-platform validation |
| M3 — Talker | Converse, inline barge-in, prompt formatting, LLM client, sentence/tag processing and local TTS exist | R7 browser TTS egress; R8 behavioral validation |
| M4 — Toolkit | Rust Gateway registry/filesystem tools and Supervisor `sandbox_exec` exist | R2/R4/R5 policy and workspace binding; future tool inventory remains open |
| M5 — Reasoner | Gateway client and protocol contract exist; Reasoner service is unimplemented | R6, after R1–R5 |
| M6 — Endpoint/Console | React/Vite Console, Gateway WebSocket endpoint, process controls and log streaming exist | R7 domain views, recovery, audio completion and UI validation |
| M7 — Integration | Focused unit/contract tests and CI jobs exist | R8 system acceptance; no new end-to-end results claimed here |
| Supervisor additions | Process orchestration, sandbox backends, HTTP/SSE logs and telemetry exist | R5 binding/recovery integration and R7 observability |

### 2.1 Confirmed Implementation

- [x] Canonical protobuf schema and buf lint/format workflow exist. Python stubs are committed and generated by `talker/scripts/gen_proto.py`; Rust stubs are generated into build output by each crate's `build.rs` using vendored protoc. Sources: [schema](../proto/kaguya/v1/kaguya.proto), [Makefile](../Makefile), [proto CI](../.github/workflows/proto-lint.yml).
- [x] Gateway has P1–P5 queues plus separate P0 control, pure pipeline handlers and an action executor. Sources: [input stream](../gateway/src/core/input_stream.rs), [pipeline](../gateway/src/core/pipeline/).
- [x] In-memory `History`, session state, context assembly, persona loading/watching and silence timers exist. Only SOUL.md and IDENTITY.md are watched. Sources: [core](../gateway/src/core/), [startup](../gateway/src/app.rs).
- [x] SQLite/BM25 RAG, optional embeddings, memory export and `RagCapability` injection exist. Current extraction is gated by user-intent/non-interrupted response handling. Sources: [RAG](../gateway/src/rag/), [capability](../gateway/src/capabilities/rag.rs), [handlers](../gateway/src/core/pipeline/handlers.rs).
- [x] Gateway connects to Listener and Talker servers, and has a Reasoner client. Gateway serves RouterControlService and, with `dev-console`, the endpoint. Sources: [clients](../gateway/src/clients/), [services](../gateway/src/services/), [voice service startup](../talker/main.py).
- [x] Gateway task supervision, connection readiness, reconnect and shutdown primitives exist. Source: [lifecycle](../gateway/src/lifecycle/).
- [x] Supervisor owns configured process launch/restart/termination and exposes process status, logs, resource samples and telemetry. Sources: [Supervisor](../supervisor/src/), [runtime configuration](../config/kaguya.runtime.toml).
- [x] Supervisor sandbox acquire/execute/release and native, Docker, Bubblewrap and feature-gated Windows Job Object backends exist. Their isolation guarantees differ. Sources: [sandbox](../supervisor/src/sandbox/), [contract tests](../supervisor/tests/sandbox_backend_contract.rs).
- [x] Gateway registers `list_files`, `read_file`, `write_file`, and conditionally `sandbox_exec`. File tools use Gateway's configured root; sandbox execution uses Supervisor scratch/container storage, not yet a shared logical-workspace binding. Sources: [tools](../gateway/src/tools.rs), [sandbox client](../gateway/src/sandbox/mod.rs).
- [x] Listener implements STT/VAD integration, thread-safe turn detection and a raw length-prefixed TCP audio server; Talker implements Converse, PrefillCache, UpdatePersona, inline BargeInAck and local TTS. Sources: [voice](../talker/voice/), [inference](../talker/inference/), [server](../talker/server.py).
- [x] Gateway tracks active Reasoner requests and cancellation in its client. Unavailability emits a scoped task error; it does not simulate progress or completion. Source: [Reasoner client](../gateway/src/clients/reasoner.rs).
- [x] Console implements text input, streamed turn views, connection state, microphone capture components, process/app controls, and log snapshot/SSE display. Browser state is ephemeral; browser TTS transport remains deferred. Sources: [Console application](../console/src/App.tsx), [regions](../console/src/regions/), [store](../console/src/store.ts), [Supervisor proxy](../console/server/plugin.ts).
- [x] Focused pipeline, reconnect/lifecycle, sandbox, prompt, sentence, turn-threading and gRPC-handshake tests and cross-platform CI definitions exist. Sources: [CI](../.github/workflows/ci.yml), [Talker tests](../talker/tests/), [Supervisor tests](../supervisor/tests/).

### 2.2 Known Gaps Carried Forward

- R1a provides durable instance/session identity and Console identity bootstrap; durable transcript reload and replay remain R1c/R7.
- Gateway accepts Talker output by generation for its stream but does not yet drop output from a superseded generation, and Talker does not limit concurrent `Converse` calls → generation-correctness step before R1c.
- `ApprovalSignal` remains declared but returns `UNIMPLEMENTED` without P0 enqueue or granting authority; shared user-policy resolution/enforcement remains R2.
- Listener retains a bidirectional control extension point; declared reset requests return `UNIMPLEMENTED`, and future pause/reconfiguration semantics remain undefined.
- Active Reasoner entries are volatile client state, not durable task management → R3.
- A configured filesystem root and sandbox scratch directories are not a workspace registry or execution binding → R4/R5.
- Reasoner has no service/backend implementation or launchable package → R6.
- TTS plays on the voice-service host; the browser has no speech playback transport → R7.
- History truncation counts messages rather than complete exchanges; summarization and an aggregate context-size guard are absent. Retrieval output caps are optional → R1/R8.
- Long awaits inside an event-loop branch can delay P0 processing despite biased selection → R0/R8.
- MCP, `web_fetch`, `search_tools`, and a standalone TypeScript Toolkit are not implemented → R5 inventory decision and deferred scope.

### 2.3 Current Validation (2026-10-28, Windows)

The repository cleanup and Listener/approval contract follow-up passed these
local checks. The unchanged Console retains its initial audit results:

| Component | Result |
| --- | --- |
| Gateway | 120 library + 6 CLI tests; `dev-console`: 122 + 6 CLI + 1 real Gateway/Supervisor status-contract test |
| Supervisor | 52 tests; `sandbox-jobobject`: 55 |
| Talker | 246 tests; mypy on 14 runtime files; Ruff 0.16.6 lint/format on 28 files |
| Console | 21 tests, browser/server TypeScript checks, Vite production build; npm audit: zero reported vulnerabilities |
| Contracts/tooling | `make proto`, buf lint/format, Rust formatting, Gateway rustdoc with broken links denied, local documentation links and diff whitespace |

Follow-up regressions verify approval returns `UNIMPLEMENTED` without entering
P0, Listener initialization/control ordering and request half-close behavior,
and the Supervisor's status request against the actual Gateway router.

Docker's conditional test did not exercise an unavailable daemon; Bubblewrap
was unavailable on this Windows host.

R1a review follow-up (2026-10-30, Linux container):

| Component | Result |
| --- | --- |
| Gateway | 121 library + 6 CLI tests; `dev-console`: 123 + 6 CLI + 1 real Gateway/Supervisor status-contract test; `cargo fmt --check` |
| Supervisor | All `cargo test` targets pass (37 library, 3 + 3 + 11 integration); `cargo fmt --check` |
| Talker | 245 tests (CI dependency set, CPython 3.13); mypy on 14 runtime files; Ruff 0.16.6 lint/format on 28 files |
| Console | 21 tests and the browser/server production build |
| Contracts/tooling | `make proto-py` with the locked grpcio-tools 1.78.0 and mypy-protobuf 5.0.0; `buf lint` and `buf format --diff --exit-code` (buf 1.47.2) |
| Manual | Two Gateways on a restored copy of one instance with different `TMPDIR` values: the second is rejected. Config typo reports the key and line/column. `init-instance --dry-run` creates nothing and refuses a selected deployment. |

Windows/macOS hosts, the Job Object and Bubblewrap backends, and live voice were
not exercised in this run. These checks do not establish live
voice/GPU/browser acceptance, cross-host Windows/macOS/Linux operation, or
completion of the deferred R1-R8 systems. The audit removed compatibility
scaffolding and false-success paths; remaining architecture work stays in the
stages below.

## 3. Active Implementation Sequence

| Stage | Focus | Main prerequisites | Status |
| --- | --- | --- | --- |
| R0 | Structural extraction and documentation alignment | Existing baseline | Complete for this extraction; Windows checks passed; R8 responsiveness/live/cross-host acceptance remains open |
| R1 | Instance/session identity; generation semantics; persistence; configuration foundation | R0 | R1a implementation/Windows checks present; R1a host/deployment acceptance and R1b → R1c → R1d remain |
| R2 | Policy management | R1 identity and R1d deployment policy foundation | Pending |
| R3 | Task management and lifecycle | R1; R2 approval semantics | Volatile delegation precursor only |
| R4 | Workspace management and lifecycle | R1/R3 association contracts | Configured-root precursor only |
| R5 | Sandbox hookup and execution binding | R2–R4; existing Supervisor | Backend infrastructure present; binding pending |
| R6 | Build Reasoner | R3/R5; capability/protocol decisions | Client/protocol only |
| R7 | Complete and update Console | APIs from R1–R6 | Existing UI partial |
| R8 | Integration and release validation | Acceptance gates from R0–R7 | Pending |

This is the default build order. Define and test each stage's APIs before
dependent behavior. Small Console/API slices may accompany their backend stage;
R7 remains the final Console completion gate. R8 scenarios should be exercised
as their dependencies become available, not deferred wholesale to the end.

### R0 — Structural Extraction and Documentation Alignment

**Goal:** Make application assembly explicit without changing behavior or
introducing a plugin framework.

- [x] Extract construction/configuration/startup wiring to `gateway/src/app.rs`; keep `main.rs` as entry point.
- [x] Extract the existing event loop into `gateway/src/core/pipeline/run.rs`, retaining P0's separate control branch and existing handler/action boundaries.
- [x] Keep existing concrete provider construction in assembly and RAG use behind its capability; no generalized provider registry or new core systems are introduced by this extraction.
- [x] Keep provider background work under the existing lifecycle owner; preserve Supervisor process/sandbox ownership.
- [x] Keep new responsibilities as files in the existing structure; update module declarations and imports to match the actual extraction.
- [x] Reconcile stale Gateway spec claims about process spawning, client/server direction, interruption protocol, session identity, transport, memory behavior, and actual versus planned audio egress.
- [x] Identify blocking/long awaits on the P0 path and record the required scheduling/cancellation correction separately from mechanical file moves; preserve the responsiveness requirement.

**Acceptance:** Existing behavior remains covered after extraction. Run Gateway
format/build/tests including `dev-console`, inspect dependency direction, and
record any remaining P0 responsiveness gap for R8. Do not mark the extraction
complete solely because the files moved.

**R8 follow-up:** The loop still awaits retrieval and history access,
then sequential action execution including provider calls and post-turn work.
P0 itself awaits Reasoner cancellation, Talker barge-in or lifecycle shutdown.
Biased selection cannot interrupt a selected branch's await. R8 must establish
and verify the scheduling/cancellation design for responsive control under slow
work.

### R1 — Session: Identity and Persistence

**Guiding checklist:** Each checkbox groups changes that must land together.
The accepted order is R1a → R1b → generation correctness → R1c → R1d → R2.
Complete the dedicated R1b refactor before R1c commits history-schema semantics. R1d handles broader
deployment consolidation without expanding R1b's semantic refactor.

#### R1a — Instance Lifecycle, Deployment Selection, and Binding

- [x] **Identity + configuration + startup:** Introduce validated `InstanceId` and default `SessionId`, initialize/save them once, and resolve the same configuration under managed and standalone startup. Surface configuration failures and stop startup when identity cannot be resolved reliably; no silent default identity. Define restore versus independent-copy behavior. Use canonical `session_id` / `SessionRef` throughout the runtime contract.
- [x] **Associations + enforcement + exposure:** Each process holds its resolved instance/session as one scope; clients stamp it on the messages they send, and each process checks an incoming scope once at its boundary (gRPC, HTTP, raw audio). No scope fields or checks on internal events, callbacks or calls. Expose identity through runtime/endpoint bootstrap. Establish instance-owned data locations and reject incompatible service bindings (one `FAILED_PRECONDITION` signal, fatal to Gateway) or duplicate local instance use (per-user lock directory, independent of `TMPDIR`). Retain one active session; history schemas and service multiplexing are later work.
- [x] **Allocation lifetime + transport + telemetry:** Separate sandbox allocation keys/handles from conversation identity and tool request IDs across Gateway/Supervisor contracts, cleanup, and event correlation. Use `supervisor_run_id` for ephemeral Docker runtime ownership; preserve its ephemeral lifetime and cleanup behavior under the `kaguya.sandbox.supervisor_run` label. Full workspace/task execution binding remains R5.
- [x] **Authoritative selection + portable paths:** Runtime TOML selects Gateway settings and explicit portable storage; its instance reference is portable-root-relative. One shared Rust selection/path resolver applies CLI → environment → documented discovery/profile precedence. Managed and direct Console/CLI paths propagate the absolute root/profile; no independent Gateway identity selector remains.
- [x] **Instance-local manifest + explicit handoff:** The selected `instances/<InstanceId>/instance.toml` is the sole identity authority and its parent the exact data root. Enforce directory/manifest identity agreement, validate manifest IDs and retain both local leases. `init-instance` / `fork-instance` create verified new directories and print the new selection without rewriting deployment files; `init-instance --dry-run` checks and prints without creating anything.
- [x] **Terminology + separation:** Keep `instance.rs` / `InstanceIdentity` / `InstanceLease` as the compact lifecycle module. Gateway and Supervisor hold one validated `SessionScope` (`shared/session_scope.rs`) and convert to the generated `SessionRef` only on the wire. Supervisor receives its scope at construction (no bind endpoint, no per-allocation owner); its HTTP adapters keep scalar ID fields and check them per request, including telemetry ingest. Python keeps its immutable `InstanceSessionBinding`. Telemetry carries the configured pair. One instance/session per process is the decided model; the wire scope is the extension point for multiple sessions (owner decision, 2026-10-29/30). Preserve persona identity names, manifest schema/fields, `shared/deployment.rs`, and the Cargo package layout; broader deployment consolidation remains R1d.
- [x] **Fixed-scope protocol + bootstrap:** One instance/session pair is required at runtime construction. Supervisor and Console's Gateway diagnostics preflight validate the selected manifest and supply authoritative child environment. Voice validates it before accepting traffic; no lazy first-caller binding remains. Named protobuf ID objects inside `SessionRef` carry the configured scope. Unary requests and initial stream messages assert the fixed pair; later messages on a stream (`BargeInSignal`, `TaskContext`) carry no scope. Scope is validated before request effects. Talker acknowledges only the first barge-in of a `Converse`. Keep `UpdatePersona` as a change-driven scoped RPC. No instance/session routing or multiplexing is introduced.
- [x] **Typed generation context + provenance:** Use `GenerationStart`, shared `PromptContext`, typed active tasks, and a trigger oneof with consistent tool `request_id` / Reasoner `task_id`. Every `TalkerOutput` carries its generation in the outer protobuf envelope; validate it against the dispatch before queuing the generated message directly and projecting it into Console events. Lifecycle payloads do not duplicate the ID. Preserve current cancellation, scheduling and stale-output acceptance policy; stronger generation lifecycle handling is separate work.
- [x] **Nonmutating diagnostics + trust:** `validate-config` / `show-config` resolve the implemented selection/settings/identity subset without launch, directories, leases, ports, or database writes, and require an initialized instance; `init-instance --dry-run` covers the uninitialized case. Allowlisted output; errors give line/column and key names but never configured values or endpoint addresses; provenance lists only settings with a value. Document shell launch configuration as trusted executable input.
- [ ] **Acceptance:** Verify stable identity across restarts/configuration edits, independent instance initialization, invalid/missing configuration failures (line/column errors), mismatched-owner rejection through Talker, Listener and a Supervisor serving another pair, duplicate restored instances under different `TMPDIR` values, `init-instance --dry-run` creating nothing and refusing exactly what init refuses, `fork-instance` preserving its source, and allocation release without session replacement. Record what happens when the selection is edited while Supervisor runs and Gateway restarts (re-resolution is R1d). Exercise affected Gateway/Supervisor contracts and Windows/macOS path behavior; distinguish tests actually run from unsupported-host checks.

R1a's selected schema, CLI handoff, storage/binding semantics, diagnostic limits,
and follow-ups are recorded in [instance lifecycle and deployment selection](r1a-identity.md).

#### R1b — Semantic Refactor of `turn*` (dedicated session)

- [ ] **Complete inventory + naming map:** Classify every remaining `turn*` field, type, function, history/configuration counter, event, and test by actual meaning. The October 23 cleanup uses `generation_id` for one Talker dispatch, `last_user_generation_id` for the latest user-intent generation, `SessionState` for session-wide state, and `utterance_epoch` for Listener's local reset counter. Message-count history limits (including `max_recent_turns`) and the broader UI/config vocabulary still need deliberate reconciliation. Reserve conversational `TurnId` for a separately defined interaction lifecycle; no blanket rename.
- [ ] **Coordinated refactor:** Apply the naming map through Gateway, Talker/Listener, protobuf/stubs, Console, telemetry, RAG source references, tests, and docs. Preserve existing IDs, dispatch boundaries, scheduling, interruption, and history behavior; update all wire and stored-field consumers together without compatibility shims. Keep behavioral bug fixes separate from this refactor.
- [ ] **Acceptance:** Review the semantic mapping and call-site changes, run affected behavior tests and frontend checks, regenerate stubs with `make proto`, and run `buf lint proto/` and relevant cross-component checks. Prove representative user, tool-continuation, Reasoner, silence, and barge-in paths retain their behavior.

#### Generation Correctness (before R1c)

A standalone behavior change after R1b's rename and before R1c defines how
generations are stored. R1b keeps behavior unchanged, and R1c needs a reliable
current generation. Approved by the owner on 2026-10-30.

- [ ] **Stale output:** Track the active generation in `SessionState`; drop any `TalkerOutput` whose `generation_id` is not the active one, so a late `ResponseComplete` or `Sentence` from a superseded generation cannot cancel or pollute the current one.
- [ ] **One active Converse:** Define and enforce in Talker that a new `Converse` preempts the active one, so an older stream cannot keep speaking after the shared speaker resets.
- [ ] **Generation ID contract:** Document `generation_id` as an opaque, non-empty string; do not add format validation.
- [ ] **Acceptance:** Test a stale `ResponseComplete` after a new dispatch, overlapping `Converse` calls, and the user, tool-continuation, Reasoner, silence and barge-in paths.

#### R1c — History Persistence, Context and Recovery (following session)

- [ ] **Schema + write boundary + ownership:** Add versioned authoritative history to `kaguya.db` using resolved instance/session ownership and R1b's correlation semantics. Define ordering, storage failures, incomplete generations, and confirmed-spoken partials; persist input independently of Talker readiness. Keep derived RAG memory distinct.
- [ ] **Context + retention:** Separate bounded prompt selection from retained source history; share authoritative history/memory across Talker and Reasoner with role-specific prompts/projections. Define compaction and deletion propagation, preserving source links. A fresh task working context must not replace instance/session/task identity; resumable context branches and LLM summarization remain explicit design choices.
- [ ] **Recovery + Console + acceptance:** Implement snapshot/cursor replay and deduplication together. Test restart, reconnect, duplicate delivery, unavailable Talker, storage failure, interrupted output, ownership separation, and prompt trimming without transcript loss. Replay must not blindly repeat external effects.
- [ ] **Identity-write durability prerequisite:** Before accepting valuable persistent history, implement safe identity-file publication and deterministic recovery from interrupted init/fork, with focused failure tests. Preserve local manifest/data consistency, fail closed, and never mint replacement IDs as error recovery. Whole-instance backup/export is tracked in R8.

#### R1d — Service Configuration Resolution in the Launcher

This accepted follow-up runs after R1c and before R2.

**Accepted direction:** The launcher owns resolution of Service Configuration
(SC) into explicit per-service startup inputs: deployment/profile selection,
source precedence, deployment-relative paths, identity selection, and
coordinated endpoints. Services validate their own inputs and construct their
runtimes. Forwarding the original runtime-file/profile selectors for children
to repeat deployment resolution does not satisfy this target. R1a describes
the current selection/reload contract; R1d replaces it deliberately.

- [ ] **Resolution ownership + readers:** Move deployment resolution into launcher-owned code; migrate consumers of `shared/deployment.rs` and retire redundant runtime readers/source sharing. Preserve established selector precedence, explicit-selection failure behavior, and profile replacement semantics; specify complete supported TOML behavior.
- [ ] **Service boundary:** Define explicit startup inputs for each service. Keep component schemas, semantic validation, provider/client construction, and runtime resource ownership local. Define which defaults and credential operations remain service-local; the launcher need not interpret every provider-specific option. Gateway retains instance creation/forking, active data leases, and authoritative application state.
- [ ] **Launch paths + bootstrap:** Make Supervisor, Console development controls, and standalone startup use the same resolution authority, or accept explicitly resolved inputs at direct service boundaries. Define initialization/fork inputs and Console/Supervisor bootstrap/proxy discovery without requiring the whole stack for standalone operation.
- [ ] **Endpoint consistency:** Align actual service binds, client destinations, advertised endpoints, and Console/Supervisor connection information from the resolved deployment; remove fixed routing assumptions.
- [ ] **Precedence + settings:** Define environment, `.env`, implementation-default, generated bind-value, and credential-reference precedence and prevent child inputs from unintentionally overriding resolved deployment decisions. Reconcile duplicated LLM inference/health settings and distinguish initialization-source settings from ongoing instance data settings.
- [ ] **Launch contract:** Introduce structured executable/argument/cwd/environment launch descriptors with explicit shell use and deliberate Windows/macOS behavior. Define startup-input delivery, configuration consistency across child startup/restarts, and replace existing command strings; launch configuration remains trusted executable input.
- [ ] **Diagnostics:** Make `validate-config` / `show-config` use the same resolution path as launch, with deployment-wide effective configuration, provenance, validation, and allowlisted/redacted output. Keep diagnostics free of process launch and instance-data writes.
- [ ] **Platform paths:** Implement platform application-data paths behind the existing path boundary only with explicit selection, supported-host validation, and explicit data placement. Never silently fall back to portable mode.
- [ ] **Documentation + acceptance:** Reconcile R1a's child-reload description and Gateway's configuration/composition specification with the implemented launcher boundary. Verify resolved inputs against actual startup, bind/connect agreement, managed/standalone equivalence, changed cwd/paths with spaces, conflicting inherited environment, invalid configuration, and nonmutating/redacted diagnostics across supported platforms; distinguish executed checks from remaining acceptance.

**[OPEN]:** The startup-input carrier/schema, exact standalone and
connection/bootstrap interfaces, complete precedence and credential contracts,
configuration pinning versus re-resolution on restart, and launch-descriptor
replacement. Resolver ownership is settled. Live reconfiguration, profile
inheritance, provider frameworks, and moving every tuning value into the root
are not implicitly included.

**[OPEN]:** The complete semantic naming map (R1b), and history schema,
retention and compaction choices (R1c). Resolve each before dependent work.

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
- [ ] **Task + Reasoner runs (one coordinated checkpoint):** Allow multiple concurrent or sequential Reasoner runs under one `TaskId`. Give each delegation attempt a distinct `ReasonerRunId`; retain both IDs through dispatch, progress, results, cancellation and persistence. Replace the current task-keyed active-delegation map so one run cannot overwrite or remove a sibling. A retry creates a new run under the same task; provider/process identity remains separate.
- [ ] Define the lifecycle/state machine, terminal outcomes and cancellation/approval transitions.
- [ ] Define task-result aggregation and completion rules across runs, including partial failure and run-only versus whole-task cancellation. Validate sibling-run independence with fake backends in R3 and real adapters in R6; one run completing must not implicitly complete the objective.
- [ ] Separate task records and coordination from `clients/reasoner.rs`; keep connection/protocol handling in the client.
- [ ] Persist required task metadata, progress/result references and terminal outcomes; define reconstruction after Gateway/Supervisor/backend loss.
- [ ] Correlate and order provider events, handle duplicates/late events, and prevent completed/cancelled tasks from being accidentally revived.
- [ ] Separate cancel-one-task, stop-active-work, and application shutdown behavior; connect them to provider cancellation and Supervisor resource cleanup.
- [ ] Expose task lists/status/results and narration inputs through explicit APIs/events.
- [ ] Define how approval waiting and workspace changes interact with running tasks; defer allocation mechanics to R5.

**Acceptance:** Exercise concurrent tasks and multiple runs of one task, targeted
cancellation, delayed events, provider disconnect, restart reconciliation and result retrieval. Assert that
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
- [ ] Use common correlation conventions for instance/session/task/run/request/allocation references while preserving their different lifetimes. Reuse R1a's allocation boundary; a reusable handle is not an invocation or Reasoner attempt, and a cancellation token is not an identity.
- [ ] Add backend capability checks and explicit unsupported-combination errors; distinguish native execution from OS-enforced isolation.
- [ ] Bind both Gateway file-tool access and Reasoner execution to the intended logical workspace and applicable policy.
- [ ] Implement cancellation, release, timeout/failure cleanup, restart reconciliation and orphan handling without silently deleting persistent workspace data.
- [ ] Keep backend/process/container details out of core pipeline logic; expose opaque identities and useful status to task management and Console.
- [ ] Define the canonical initial tool inventory. Resolve the standalone TypeScript Toolkit proposal separately; do not silently reintroduce direct shell execution or present absent tools as available.
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
- [ ] Map backend progress, output, errors and completion to both task and Reasoner-run identities; preserve sibling-run independence and support context updates where required.
- [ ] Implement cancel/stop/shutdown and reconcile interrupted/disconnected backend runs with R3 lifecycle rules.
- [ ] Complete Supervisor-controlled startup/readiness for on-demand Reasoner use; surface failures before treating work as running.
- [x] Remove simulated fallback; normal unavailability produces a task error. Real adapter execution remains R6.
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
- [ ] Build backup/export/restore and consistent whole-instance snapshot tooling, with broader disk/power/process-failure scenarios and operational recovery procedures. Preserve R1c's earlier focused identity-publication/recovery prerequisite; optimize performance only against observed requirements or measurements.
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
| Console | `npm test`, `npm run build` (browser and server types), and actual browser/microphone interaction checks |
| Proto | `make proto` regenerates Python and builds both Gateway and Supervisor; run `buf lint proto/` and `buf format --diff --exit-code proto/` |
| End-to-end | Scenario evidence from R8, with model/provider/backend/platform configuration and explicit limitations |

Existing CI definitions are in [.github/workflows/ci.yml](../.github/workflows/ci.yml)
and [proto-lint.yml](../.github/workflows/proto-lint.yml). Backend environment
requirements are documented in [sandbox deployment testing](sandbox-backend-deployment-testing.md);
telemetry scenarios are in [telemetry testing](telemetry-e2e-testing.md).

## 5. Open Decisions and Deferred Scope

The active-stage **[OPEN]** paragraphs are design work, not optional polish.
Record decisions in the relevant contract/spec before building dependent
behavior. Add or refine bibliography entries when external evidence informs a
significant design question.

Retained empirical questions from the original plan:

- GPU contention and voice latency with STT, LLM inference and TTS running together.
- Voice selection and audio quality; the configured voice is not a completed listening evaluation.
- Action-tag reliability across supported models and whether additional training is warranted.
- Custom versus library sentence detection and its latency/quality tradeoffs.
- Barge-in spoken/unspoken precision: current code acknowledges naturally completed, error-free playback batches; validate live playback and evaluate word-level tracking separately.
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
