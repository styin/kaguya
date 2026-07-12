# spec-reasoner-v0.1.0.md

# Project Kaguya — Reasoner Specification

**Component:** Reasoner
**Version:** 0.1.0
**Date:** June 2026
**Audience:** Developers and coding agents building the Reasoner and its Gateway integration
**Status:** DRAFT — sections marked **[SIGN-OFF]** require human approval before implementation.

---

## 1. Role & Execution Model

The Reasoner is the **slow, deliberate path** of Kaguya's dual-path inference harness.
Where the Talker must be fast, voice-first, and responsive, the Reasoner does the heavy
lifting: long-horizon task execution, tool use, multi-step reasoning. It is spawned on
demand when the Talker emits a `[DELEGATE:...]` tag.

The Reasoner does **not** own:

- User contact — it never talks to the user or produces voice. All output flows through
  the Gateway, surfaced (or not) at the Talker's discretion.
- Persona, voice, or conversation history.
- Turn timing or the Input Stream.

> **Current design decision.** Every Reasoner backend runs inside a Gateway-authorized task
> workspace lease. Gateway retains sole discretion over durable Kaguya-agent workspace and host
> capabilities; Supervisor supplies the volatile task environment and cleanup. The lease is the
> boundary for native file, shell, and network tools. It is an
> opaque, task-scoped capability, never a host path passed through gRPC. CLI-backed adapters
> (Codex app-server and ACP-compatible agents) are first-class. A backend retains its own native
> tools inside the lease; Kaguya augments those tools
> with namespaced capabilities, normally over MCP, rather than replacing the CLI registry.
> Gateway authorization and Supervisor isolation together enforce the boundary (REF-011).

**The single swap point is the adapter interface (§4).** Backend-pluggability lives there
and nowhere else. Phase 1 supports Codex app-server plus Grok and Kimi ACP adapters (§9B).
Future adapters require no change to the Gateway, wire contract, or Talker.

---

## 2. Architecture

Only the settled, undisputed choices are stated here. Anything still open is in §14.

```
Supervisor ──launches on_demand──► Reasoner service (one process)
                                     │  N concurrent sessions, keyed by task_id
Gateway ◄──── gRPC ReasonerService ──┘
   │  Delegate(stream) per task
   ├─ TaskRegistry: folds the event stream → per-task TaskState
   ├─ TaskState injected into every TalkerContext (context, not events)
   └─ Input Stream events emitted ONLY for: approval_request, completed, error  [P3]
```

- **Supervisor owns volatile execution lifecycle.** It launches and cleans up the Reasoner
  service and task scratch environment on demand. Gateway authorizes the task workspace and
  capabilities; it does not spawn OS processes.
- **One service process, N sessions.** A task is a *session* inside the Reasoner process,
  keyed by `task_id` — not an OS process per task. This makes concurrency a config knob
  (§10), not a process-management problem.
- **Gateway is the gRPC client.** The Reasoner is the server. The adapter normalizes
  whatever the backend produces into the wire vocabulary of §3.

### Where the code changes land

| Area | Change |
| ---- | ------ |
| `reasoner/` | Rewritten from the scaffold into the Reasoner service and Codex app-server / ACP adapters. |
| `proto/kaguya/v1/kaguya.proto` | Revise `ReasonerService`; remove `Telemetry`; use `repeated TaskState` in `TalkerContext`. |
| `gateway/src/reasoner.rs` | `ReasonerManager` → `TaskRegistry` + stream-fold pump. |
| `gateway/src/narration.rs` | Removed; narration selection belongs to the Talker (see §5). |
| `gateway/src/context.rs` | Injects `repeated TaskState`; rate-limits re-prefill on digest change. |
| `talker/inference/prompt_formatter.py` | Renders the TaskState block; small system-prompt addition. No structural change. |

---

## 3. Wire Contract & Event Vocabulary **[SIGN-OFF]**

This is the authoritative gRPC contract. It is ground truth and must be approved before
implementation. The event vocabulary is deliberately shaped after an established
agent-client event model (see REF-014) so that a future protocol-level adapter is a
transport change, not a redesign.

```protobuf
service ReasonerService {
  // Bidi, one stream per task. Gateway sends task start, mid-task context updates,
  // and approval decisions; Reasoner streams lifecycle / activity / plan / approval /
  // result events.
  rpc Delegate(stream DelegateInput) returns (stream DelegateOutput);

  // Unary interrupt — always available, even with no delegation in flight.
  rpc Interrupt(InterruptRequest) returns (InterruptAck);
}

// ── Inbound: Gateway → Reasoner ──────────────────────────────────────────────
message DelegateInput {
  oneof payload {
    TaskRequest      start_task        = 1;
    TaskContext      context_update    = 2;  // inject context into a live task
    ApprovalDecision approval_decision = 3;  // resolve a parked approval
  }
}

message TaskRequest {
  string task_id              = 1;  // generated by the Talker; stable for the lifecycle
  string description          = 2;
  PermissionPolicy policy     = 3;  // what the task may do without asking (§7)
  // Opaque task-scoped capability minted by Supervisor. Never a host path.
  string workspace_lease_id = 4;
  map<string, string> metadata = 5;
}

message TaskContext {
  string task_id = 1;
  string content = 2;  // additional natural-language context, mid-task
}

message ApprovalDecision {
  string   task_id    = 1;
  string   request_id = 2;  // correlates to ApprovalRequest.request_id
  Decision decision   = 3;
  string   reason     = 4;  // optional; surfaced to the backend on deny
}

enum Decision {
  DECISION_UNSPECIFIED  = 0;
  DECISION_ALLOW        = 1;
  DECISION_DENY         = 2;
  DECISION_ALLOW_ALWAYS = 3;  // OPEN (§14): allow + remember for this session
}

// ── Outbound: Reasoner → Gateway ─────────────────────────────────────────────
message DelegateOutput {
  string task_id      = 1;
  uint32 seq          = 2;  // monotonic per task; ordering parity with TalkerOutput
  int64  timestamp_ms = 3;
  oneof event {
    TaskStarted     started          = 4;
    TaskActivity    activity         = 5;
    PlanUpdate      plan             = 6;
    ApprovalRequest approval_request = 7;
    TaskCompleted   completed        = 8;
    TaskError       error            = 9;
  }
}

message TaskStarted {
  string backend         = 1;  // "codex-app-server", "grok-acp", "kimi-acp", ...
  string model           = 2;
  string permission_mode = 3;  // backend's effective mode after policy mapping
  repeated string available_tools = 4;  // full inventory, for UI introspection (§8)
}

message TaskActivity {
  string         activity_id = 1;  // stable; later updates reuse the same id
  ActivityKind   kind        = 2;
  string         title       = 3;  // human-readable, narration-grade ("Reading config.yml")
  ActivityStatus status      = 4;
  repeated string locations  = 5;  // absolute paths touched, optional
  string         raw_json    = 6;  // full structured payload; opaque to Gateway, for UI/relay
}

enum ActivityKind {
  ACTIVITY_KIND_UNSPECIFIED = 0;
  ACTIVITY_KIND_THINK    = 1;  // reasoning / thought content
  ACTIVITY_KIND_READ     = 2;
  ACTIVITY_KIND_EDIT     = 3;
  ACTIVITY_KIND_EXECUTE  = 4;
  ACTIVITY_KIND_SEARCH   = 5;
  ACTIVITY_KIND_FETCH    = 6;
  ACTIVITY_KIND_TEXT     = 7;  // assistant message text
  ACTIVITY_KIND_OTHER    = 8;
}

enum ActivityStatus {
  ACTIVITY_STATUS_UNSPECIFIED = 0;
  ACTIVITY_STATUS_PENDING     = 1;
  ACTIVITY_STATUS_IN_PROGRESS = 2;
  ACTIVITY_STATUS_COMPLETED   = 3;
  ACTIVITY_STATUS_FAILED      = 4;
}

message PlanUpdate {
  repeated PlanEntry entries = 1;  // COMPLETE replacement each time — no incremental merge
}

message PlanEntry {
  string          content = 1;
  PlanEntryStatus status  = 2;
  // priority: OPEN (§14) — include only if a consumer needs it
}

enum PlanEntryStatus {
  PLAN_ENTRY_STATUS_UNSPECIFIED = 0;
  PLAN_ENTRY_STATUS_PENDING     = 1;
  PLAN_ENTRY_STATUS_IN_PROGRESS = 2;
  PLAN_ENTRY_STATUS_COMPLETED   = 3;
}

message ApprovalRequest {
  string request_id = 1;
  string title      = 2;  // "wants to run `git push`"
  string tool_name  = 3;
  string raw_json   = 4;  // full tool input — UI detail + relay
  // options: OPEN (§14) — fixed allow/deny vs. backend-supplied option set
}

message TaskCompleted {
  string result = 1;  // FULL result text, unbounded; Gateway retains it and caps only the digest
}

message TaskError {
  string message = 1;
  int32  code    = 2;
}

message InterruptRequest {
  oneof signal {
    TaskCancel cancel   = 1;  // cancel one task
    GlobalStop stop     = 2;  // cancel all in-flight tasks
    Shutdown   shutdown = 3;  // drain and exit
  }
}
message TaskCancel { string task_id = 1; }
message GlobalStop {}
message Shutdown   {}
message InterruptAck {}

// ── Permission policy (§7) ───────────────────────────────────────────────────
// Shape is SIGN-OFF + has OPEN questions (§14).
message PermissionPolicy {
  repeated string allow_tools      = 1;  // auto-allowed, never prompts
  repeated string deny_tools       = 2;  // auto-denied, never even asks
  // Workspace scope is represented by TaskRequest.workspace_lease_id, never paths.
  repeated string allow_kaguya_capabilities = 3;
  repeated string deny_kaguya_capabilities  = 4;
  // anything not matched by allow/deny → ApprovalRequest
}
```

The Talker-facing projection (`TaskState`) lives in §6 because its shape is driven by how
the digest is consumed.

---

## 4. Adapter Interface

The adapter is the seam between Kaguya's stable wire contract and a specific backend. It is
the only place a backend's vocabulary appears. The interface is presented language-agnostic;
the implementation language is **[OPEN]** (§14).

```
interface ReasonerAdapter {
  // Begin a task. Yields normalized DelegateOutput events until the task ends.
  start(task: TaskRequest) -> AsyncStream<Event>

  // Inject additional natural-language context into a running task.
  send_context(task_id: string, content: string)

  // Resolve a pending approval; unblocks the parked tool call inside the backend.
  decide(task_id: string, request_id: string, decision: Decision)

  // Cancel a running task (maps to the backend's interrupt/abort).
  cancel(task_id: string)
}
```

The Reasoner service owns the session table (`task_id → adapter session`), the concurrency
cap (§10), and the gRPC plumbing. The adapter owns only the translation.

## 5. Design Principles & Implications

These shape the rest of the spec; each carries a concrete consequence.

- **Progress is context, not events.** The Gateway folds the activity/plan stream into
  `TaskState` silently. Only **approval requests**, **completion**, and **errors** become
  Input Stream events (P3). *Consequence:* Gateway does not decide which step to narrate.
- **Narration is a Talker phenomenon, not a trigger.** The Talker mentions in-flight work
  at its own discretion when it has the context. *Consequence:* the Talker speaks at
  **openings, not triggers** — a user turn, a silence-timer P4 event, or a P3
  completion/approval event. No new mechanism is needed; the digest must simply be *current*
  whenever a turn happens.
- **The digest is prose-shaped on purpose.** An 8B model handed JSON parrots JSON. Handed
  human-readable activity titles and plan entries, it can paraphrase naturally.
  *Consequence:* §6's digest renders titles/plan text, never raw structured payloads.
- **Results are unbounded gateway-side; the digest is capped.** The full `TaskCompleted.result`
  is retained by the Gateway; only the copy placed into `TaskState` is length-capped.
  *Consequence:* context budget stays bounded under N concurrent tasks without losing the
  full result (the Talker can surface detail on demand).
- **Digest changes are rate-limited against prefill thrash.** `TaskState` lives in the
  Talker prompt's stable prefix, so every change invalidates the KV cache the Talker relies
  on for sub-second latency. *Consequence:* the Gateway updates `TaskState` in memory
  continuously but **re-renders/re-prefills only on status or plan transitions**, not on
  every activity. Exact cadence is **[OPEN]** (§14).

---

## 6. Task State & the Digest **[SIGN-OFF]**

`TaskState` is the Gateway's materialized view of a task, injected into every `TalkerContext`.
It is ground truth for what the Talker can say about
in-flight work.

```protobuf
message TaskState {
  string          task_id          = 1;
  string          description      = 2;
  TaskStatus      status           = 3;
  repeated PlanEntry plan          = 4;  // latest complete plan
  string          current_activity = 5;  // latest non-THINK activity title
  ApprovalSummary pending_approval = 6;  // present iff WAITING_APPROVAL
  string          result_digest    = 7;  // capped; present once COMPLETED
}

enum TaskStatus {
  TASK_STATUS_UNSPECIFIED       = 0;
  TASK_STATUS_RUNNING           = 1;
  TASK_STATUS_WAITING_APPROVAL  = 2;
  TASK_STATUS_COMPLETED         = 3;
  TASK_STATUS_FAILED            = 4;
}

message ApprovalSummary {
  string request_id = 1;
  string title      = 2;  // "wants to run `git push`"
}
```

**Digest rendering rules (Phase 1, mechanical — no LLM):**

- `plan` is the primary narration material: phase-level, already statused, already prose.
- `current_activity` is the latest non-think activity `title`.
- `pending_approval` is set whenever status is `WAITING_APPROVAL`.
- `result_digest` is `TaskCompleted.result` truncated to a configured char cap; the full
  result is retained by the Gateway and not placed in context.
- Target budget: small and bounded per task (a handful of plan entries + one activity line),
  so N concurrent tasks stay within the `TalkerContext` budget. Exact cap is config.

**Talker system-prompt addition (small):** *"In-flight tasks appear under Active Tasks.
Mention them at your discretion when relevant; relay a pending approval when convenient.
Paraphrase — never read file paths or command strings aloud."*

> The LLM-summarized digest (a one-sentence phase summary replacing the mechanical render)
> is a deferred upgrade (§13). It changes only the digest *producer* — `TaskState`, the
> wire contract, and the Talker are unaffected — which is why the mechanical version ships
> first.

---

## 7. Permission Model

Kaguya defines *what a task may do without asking*; the adapter maps that onto the backend's
native permission system; anything not pre-decided becomes an approval (§4, §6). Kaguya does
not rebuild a permission engine — it configures one and supplies the decision for the gated
cases.

**Worked example.** A task delegated with:

```
PermissionPolicy {
  allow_tools:      ["Read", "Edit", "Grep", "Glob"]
  deny_tools:       ["WebFetch"]
  allow_kaguya_capabilities: []
  deny_kaguya_capabilities:  []
}
```

The accompanying `TaskRequest.workspace_lease_id` identifies the Supervisor-managed workspace
in which the backend is started. It grants no direct host-path authority to the Gateway or
Reasoner service.

- The agent reads and edits freely **within** `/home/user/project` — no prompts.
- It attempts `Bash("git push")` — not in `allow_tools`, not in `deny_tools` → the adapter's
  `canUseTool` fires → `ApprovalRequest{title: "wants to run \`git push\`"}` → flows to the
  Talker as a non-blocking todo (Scenario S2) → user decides by voice → `ApprovalDecision`
  resolves the parked call.
- It attempts `WebFetch(...)` — in `deny_tools` → auto-denied, the user is never asked.

**Mid-task policy change vs. topology change.** A change to *policy* (allow/deny, mode) takes
effect on the **next gated call**, because evaluation is per-call in our code. A change to
*topology* (which tools/MCP servers are mounted) is fixed at session start and requires a
session **resume** — a restart-shaped operation. For UI-driven config, policy changes apply
instantly; topology changes apply at next task or via resume.

---

## 8. Introspection Model

Three layers, one stream, two renderings.

- **Contract.** `TaskStarted` carries the full tool inventory, model, and permission mode.
  Every `TaskActivity` carries a structured `raw_json` payload. Nothing the backend does is
  hidden from the Gateway.
- **Fold.** The Gateway's `TaskRegistry` materializes `TaskState` from the event stream —
  this is the single source of derived task state.
- **Rendering — two consumers, two fidelities:**
  - **UI / console:** the **raw** activity stream and tool inventory, verbatim. This is the
    introspection channel — the user can read the reasoner's reasoning and tool calls
    directly, ready for relay.
  - **Talker:** the **digest** (§6) — small, prose-shaped, budget-bounded.

The UI's verbatim feed and the Talker's digest are the same fold rendered at different
fidelities; they never diverge in source of truth.

---

## 9. Tool Registry Boundary

The Talker Toolkit and the Reasoner's toolset are **separate registries by design**:

- **Talker Toolkit** — fast, Talker-only tools (`[TOOL:...]` flow), dispatched by the
  Gateway. Sub-second, conversational.
- **Reasoner tools** — the backend's **native** toolset (file ops, bash, web, etc.), plus
  any Kaguya MCP servers mounted *into* the session. The Reasoner executes these itself.

Kaguya governs the Reasoner's tools through **per-call policy (§7), not ownership**. Reasoner
tool calls do **not** route through the Talker Toolkit — doing so would recreate the wrapping
problem the SDK backend exists to avoid. Kaguya may *extend* the Reasoner's tools by mounting
its own MCP servers, but it does not intermediate every call.

---

## 9A. Sandbox-Leased Backend Execution **[SIGN-OFF]**

### Authority and execution boundary

- **Gateway is the authority for host capabilities and the durable Kaguya-agent workspace.**
  Gateway grants the task's access scope and authorizes the resulting workspace lease.
- **Supervisor supplies the volatile execution environment.** It creates and cleans up task
  scratch space and applies the process/network policy that Gateway authorized.
- **The Reasoner executes only inside that lease.** A native CLI may read, edit, and execute
  commands through its own tool registry, but those actions are confined to the workspace and
  network policy the Supervisor actually granted.
- **A lease is opaque on the Reasoner gRPC boundary.** `TaskRequest.workspace_lease_id` is a
  task-scoped reference, not an absolute path, container ID, or host credential. Supervisor is
  the only component that resolves it to a backend-specific environment.
- **Scratch lifetime is task-scoped.** Supervisor cleans it up after completion, failure, or
  cancellation. Any export into the durable Kaguya-agent workspace requires Gateway approval;
  it is never an accidental consequence of a CLI writing directly to the host.

### Tool planes

| Plane | Owner | Rule |
| ----- | ----- | ---- |
| Talker Toolkit | Gateway | Fast conversational tools. It is not exposed as a replacement filesystem/shell registry to the Reasoner. |
| Native backend tools | CLI or SDK adapter | The backend keeps its own file, edit, shell, and test loop and runs it inside the lease. |
| Kaguya extensions | Gateway capability plugins, normally MCP | Exposed with a `kaguya.*` namespace. They may require a Kaguya approval or capability grant and do not collide with native tool names. |

An adapter may map `PermissionPolicy.allow_tools` and `deny_tools` to its native permission
mechanism, but that is advisory defense in depth. A backend which cannot honor a requested
permission or isolation feature must report the limitation; it must not claim equivalent
enforcement.

### Adapter capability profile

Every adapter declares, at task start, whether it supports structured activity, native
approval interception, Kaguya-extension injection, cancellation, and session resume. The
Gateway renders only activity the adapter actually observed. This permits a lower-fidelity CLI
adapter without pretending that a terminal scrape offers SDK-level introspection.

### Workspace materialization

The lease describes the *security boundary*, not yet a single checkout strategy. Before
implementation, decide whether a task receives a copied checkout, a controlled bind-backed
workspace, or another Supervisor-managed materialization. That decision must preserve
task isolation and define how an approved result is exported to the host.

---

## 9B. Phase 1 Adapter Shortlist and Contract Mapping **[SIGN-OFF]**

Phase 1 supports only the following adapters:

| Backend | External contract | Phase 1 adapter shape |
| ------- | ----------------- | --------------------- |
| Codex | Codex `app-server` JSON-RPC | A long-lived app-server client, one Codex thread per Kaguya task. |
| Grok | ACP-style JSON-RPC over stdio | Supervisor starts the ACP agent inside the lease; the adapter is an ACP client. |
| Kimi | `kimi acp`, ACP JSON-RPC over stdio | Supervisor starts `kimi acp` inside the lease; the adapter is an ACP client. |

Claude and Qwen adapters are **Phase 2** work. No other CLI is in scope for Phase 1.

### Kaguya internal adapter contract

The service exposes one backend-neutral contract. Adapters may report a capability as absent,
but must not emulate it by scraping unstructured terminal prose.

```text
create_task(task, workspace_lease) -> AdapterSession
submit_turn(session, description | context_update)
events(session) -> AsyncStream<NormalizedEvent>
decide(session, approval_id, decision)
interrupt(session)
resume(backend_session_id) -> AdapterSession   // only when declared supported
close(session)
```

`NormalizedEvent` is the §3 vocabulary: lifecycle metadata, observable activity, complete plan
replacement, approval request, completion, or error. A missing structured plan is represented
by no `PlanUpdate`, not by an invented plan.

### Contract mapping

| Kaguya contract | Codex app-server mapper | Grok/Kimi ACP mapper | Kaguya rule |
| ---------------- | ----------------------- | -------------------- | ----------- |
| `create_task` | Start an app-server thread and retain its thread ID. | `initialize`, then create an ACP session; retain its session ID. | Supervisor launches the server or ACP subprocess inside the workspace lease before the adapter connects. |
| `submit_turn` | Start a turn on the task thread; a context update is a later turn on that same thread. | Send `session/prompt` to the existing ACP session. | The original delegation and later user context share one backend session. |
| `events` | Fold thread/turn/item notifications into activity and terminal events. Item type determines the normalized activity kind. | Fold `session/update` notifications and prompt completion into activity and terminal events. | Preserve raw structured payloads for the UI; emit only observed fields. |
| `PlanUpdate` | Map a structured Codex plan/todo item when present. | Map an ACP structured plan/todo update when the agent declares and emits one. | Never infer a plan from assistant prose. |
| `ApprovalRequest` | Map app-server tool/command approval requests and reply with the selected decision. | Map ACP permission/tool-call requests and send the ACP response. | If a backend lacks an interceptable approval hook, its native actions are governed solely by the lease and the adapter declares that limitation. |
| `interrupt` | Send the app-server turn interrupt/cancel request and wait for terminal confirmation. | Send ACP session cancellation; terminate the leased subprocess only as the Supervisor fallback. | A P0 stop does not depend on a queued conversational turn. |
| `resume` | Reattach using the retained Codex thread/session identity when supported by the installed app-server version. | Use ACP session load only when `initialize` advertises it. | Resume is capability-gated; otherwise a backend restart fails the task cleanly. |
| Kaguya extensions | Configure/inject Kaguya MCP capability plugins through the app-server-supported MCP/config surface. | Supply Kaguya MCP servers through ACP only when the agent advertises the needed MCP transport. | Extensions are `kaguya.*`; they never replace native file or shell tools. |

### Phase 1 conformance gates

Codex, Grok, and Kimi must each pass the same adapter conformance suite: task start, activity
ordering, context update, completion/error, cancellation, and declared-capability reporting.
Approval, plan, resume, and MCP tests run only when the backend advertises their supporting
contract feature. Grok's exact command and capability profile remain configuration and probe
data until its ACP implementation is verified against that suite.

---

## 10. Concurrency

- A task is a session keyed by `task_id`; the Reasoner process multiplexes N sessions.
- **Day-one cap: 1** concurrent task (config). The interface, wire contract, and `TaskState`
  are all N-ready — concurrency is a config change, not a redesign.
- Mental model: like a coding agent dispatching background sub-agents — concurrency is built
  into the interface even when the cap is 1.

---

## 11. Future Adapters (high-level)

Sketches only — not specced here, not in Phase 1 scope.

- **Additional CLI adapter** — maps observable exec/patch events to `TaskActivity`, approval
  policy to `ApprovalRequest`, and declared sandbox modes to `PermissionPolicy`.
- **Raw-model-API loop** — Kaguya owns the agent loop and swaps raw providers (incl. local
  endpoints). The documented escape hatch if the SDK constrains us; cost is owning tool
  execution, sandboxing, and compaction.
- **Generic protocol adapter** — speak a standard agent-client protocol to any conforming
  agent. Cheap precisely because the §3 vocabulary is already shaped for it (REF-014).

Backend selection is configuration; auth remains each backend's own concern (the adapter
never handles credentials).

---

## 12. Task Lifecycle — Scenarios **[SIGN-OFF]**

These are the **ground-truth desired outcomes** of this spec. The implementing agent MUST,
on completion, verify the implementation supports every flow below.

### Primary flows

**S1 — Happy path.**
1. Talker emits `[DELEGATE: investigate failing pipeline]`.
2. Gateway opens `Delegate`, sends `start_task` (with `PermissionPolicy`).
3. Reasoner emits `started` (inventory/model/mode) → Gateway records `RUNNING`; UI shows it.
4. `activity` / `plan` events stream → Gateway folds into `TaskState` silently (no Talker turn).
5. Reasoner emits `completed{result}` → Gateway retains full result, caps the digest, emits
   a **P3** event.
6. At the next opening, the Talker summarizes from `result_digest`.

**S2 — Approval gate (non-blocking).**
1. During S1, the agent attempts a gated tool (e.g. `git push`).
2. Adapter `canUseTool` fires → `approval_request` emitted; the backend call parks.
3. Gateway sets `TaskState.status = WAITING_APPROVAL`, populates `pending_approval`, emits a
   **P3** event. Kaguya does not block; only that one tool call waits.
4. The Talker relays the ask at its next opening; the user answers by voice.
5. Gateway sends `approval_decision`; the adapter resolves the parked future; the task resumes.

**S3 — Mid-task context update.**
1. The user adds information relevant to a running task ("actually, check the staging branch").
2. Gateway sends `context_update{content}` on the live `Delegate` stream.
3. The adapter injects it as a follow-up message; the task adapts. No restart.

**S4 — Cancellation / interrupt.**
1. The user or a P0 `STOP` cancels work.
2. Gateway sends `Interrupt{cancel(task_id)}` (or `stop` / `shutdown`).
3. The adapter aborts the session; the Reasoner stops emitting; the Gateway marks the task
   ended and drops it from `TaskState`.

### Edge-case flows

**E1 — Completion arrives while the user is mid-utterance.** The `completed` P3 event sits
behind P1/P2 in the Input Stream and is handled only once the user's turn settles — a task
result never preempts the user.

**E2 — Reasoner error or backend unavailable.** On `error` (or connect failure), the Gateway
marks the task `FAILED`, surfaces a concise failure via P3, and the Talker conveys it without
exposing raw logs. (Replaces the current silent stub-fallback behavior — **[OPEN]**: is any
stub retained for dev? §14.)

**E3 — Approval never answered.** A `WAITING_APPROVAL` task with no decision within a
configured window → **default-deny** (the safe direction), task continues or fails per the
backend. Timeout value and default are **[OPEN]** (§14).

**E4 — Multiple concurrent tasks (when cap > 1).** Each task has independent `TaskState`;
the combined digest must stay within the `TalkerContext` budget — per-task caps shrink as N
grows, or the Talker is told only the K most relevant tasks. Exact policy is **[OPEN]** (§14).

**E5 — Task completes before the Talker ever mentioned it.** Valid and common. The `completed`
P3 event is the first time the Talker speaks about the task — it summarizes the result with no
prior "I'm working on it." The flow must not assume an acknowledgment was ever spoken.

---

## 13. Phased Delivery

**Phase 1 — where the code changes land** (detail in §2): rewrite `reasoner/` into the
service plus Codex app-server and ACP adapters; revise `ReasonerService` and `TalkerContext`
in the proto; convert `gateway/src/reasoner.rs` into a `TaskRegistry` + fold; remove Gateway
narration selection; extend `gateway/src/context.rs` with `TaskState` injection + re-prefill
rate-limiting; add the `TaskState` render + system-prompt line in
`talker/inference/prompt_formatter.py`.

**Deferred (high-level list only):**

- LLM-summarized digest producer.
- Additional backend adapters beyond Codex app-server, Grok ACP, and Kimi ACP.
- Per-task backend selection.
- Concurrency cap > 1.
- Raw-model-API loop adapter (Gateway-owned compaction).

---

## 14. Open Questions — Requires Human Sign-Off

None of these are assumed in the spec; each needs a decision before or during implementation.

- **Adapter / service language.** Choose the implementation language based on the supported
  adapter contracts and operational fit; it does not change the Kaguya wire contract.
- **Digest re-prefill cadence.** The rate-limit that prevents prefill thrash (§5): coalesce
  interval, or only on status/plan transitions? Needs a tuned value.
- **Approval timeout semantics (E3).** Timeout window and default (assumed default-deny).
- **Multi-task context budget (E4).** How the combined digest is bounded when cap > 1.
- **`Decision.ALLOW_ALWAYS` (§3).** Support session-scoped "allow and remember", or
  allow/deny only in Phase 1?
- **`ApprovalRequest.options` (§3).** Fixed allow/deny, or a backend-supplied option set?
- **`PlanEntry.priority` (§3).** Include priority, or status + content only?
- **Dev stub (E2).** Retain any stub/echo fallback for development when no backend is
  configured, or fail hard?

---

## Companion reference to add

- **REF-014 — Reasoner event vocabulary modeled on an agent-client protocol.** Rationale for
  shaping `DelegateOutput` (activity kinds/status, complete-replacement plans, permission
  request/response as first-class events) after an established agent-client event model, so a
  future protocol-level adapter is a transport change rather than a redesign. *(To be drafted
  and appended to `REFERENCES.md` on sign-off.)*
