# Instance lifecycle and deployment selection (R1a)

The runtime TOML is the deployment entry point. It selects component settings,
an explicit portable data root, and one instance-local manifest. Gateway owns
instance creation, rekeying, persona/RAG adoption, and active data leases.
Supervisor manages processes and sandbox allocations. R1a supports one active
session per instance; history remains in memory until R1c.

## Deployment selection

All Rust entry points and Console's development launcher use this precedence:

1. `--runtime-config PATH` / `--profile NAME`.
2. `KAGUYA_RUNTIME_CONFIG` / `KAGUYA_RUNTIME_PROFILE`.
3. Runtime-file discovery / the selected runtime file's `profile`.

Discovery checks `config/kaguya.runtime.toml`, then
`../config/kaguya.runtime.toml`, then `kaguya.runtime.toml`, relative to the
launcher's cwd. An explicit missing/invalid root or empty selector fails; it
never falls back to another deployment. A selected named profile must exist.
Named profiles replace the process map; they do not deep-merge it. A root with
top-level `processes` can omit profile selection.

The root schema includes:

```toml
profile = "app"
gateway_settings = "../gateway/gateway.toml"
supervisor_addr = "127.0.0.1:3001"

[paths]
mode = "portable"
portable_root = "../gateway/data"

[instance]
# Set after explicit initialization:
# manifest = "instances/<InstanceId>/instance.toml"
```

`gateway_settings` and `portable_root` resolve relative to the runtime file;
absolute values are also supported. `instance.manifest` must be relative to
the portable root, select `instances/<InstanceId>/instance.toml`, and contain
no parent traversal. The directory component must be a canonical non-nil UUID,
and loading the manifest requires it to equal the declared `instance_id`.
Gateway persona initialization sources, RAG source, and file-tool workspace
paths resolve relative to the selected Gateway settings file.

`shared/deployment.rs` is the narrow shared Rust selection/path resolver;
component loaders retain their typed settings and topology responsibilities.
Application assembly receives the resolved selection and settings instead of
discovering them. Managed children receive the absolute root/profile after
per-process environment entries are applied and reload that selection.
Console uses the same selected root/profile for managed and direct launch
controls, including the standalone voice child. Before launching children it
asks Gateway's read-only `show-config` command to resolve and validate the
selected identity. Supervisor resolves the same manifest before launching its
children. Both launch paths apply `KAGUYA_INSTANCE_ID` and `KAGUYA_SESSION_ID`
after per-process environment entries, so a child cannot override the selected
pair. Direct voice debugging requires those same two variables explicitly.
A deployment file is assumed
stable during startup; this is not an immutable configuration snapshot.

Only explicit `portable` mode is implemented. Platform application-data mode
and automatic relocation are deferred; requesting an unsupported mode fails.
Windows drive-relative/root-relative paths fail instead of depending on a
process's current drive. An existing portable root must be a directory; a
missing root is valid for initialization.

## One instance-local authority

```text
gateway/data/
`-- instances/
    `-- <InstanceId>/
        |-- instance.toml
        |-- SOUL.md
        |-- IDENTITY.md
        `-- kaguya.db
```

The selected manifest's parent is the exact instance root: no second
`instances/<id>` suffix is appended. `instance.toml` contains only
`schema_version = 1`, `instance_id`, and `default_session_id`; both IDs are
different, non-nil, canonical UUID strings. Generation uses UUIDv4. There is
no duplicate `owner.toml` record and no legacy selection/migration interface.
The default-session association is therefore not duplicated in directory
ownership metadata. Changing it does not rename the instance directory; this
does not add live session switching, and running services still enforce their
existing pair binding.

`gateway/src/instance.rs` keeps the compact lifecycle boundary:
`InstanceIdentity`, `InstancePaths`, `InstanceLease`, `init_instance()`,
`rekey_instance()`, and `load_required()`. Instance identity survives restore
to another host and does not identify the physical machine. `IDENTITY.md`,
`identity_md`, and the voice `IdentityConfig` describe mutable persona identity
content, not InstanceId. The manifest remains a storage/lifecycle concern.
Gateway and Supervisor each use their generated `proto::SessionRef` directly
for runtime scope, including its named `InstanceId` and `SessionId` messages.
Small component-local helpers construct and validate these values at startup
and request boundaries. Generated message types alone do not guarantee valid
IDs; validation requires both objects and canonical, non-nil, distinct UUID
values. Runtime state and RPCs retain the same representation. The existing
`shared/deployment.rs` resolver and Cargo package layout remain unchanged;
broader configuration consolidation remains R1d.

## Explicit creation and print-only selection handoff

Copy the runtime template beside itself as a local deployment such as
`config/kaguya.local.toml` so its relative references keep their meanings. A
copy in another location needs deliberately rebased references. The checked-in
template initially has no selected instance.

From the repository root:

```sh
cargo run --manifest-path gateway/Cargo.toml -- init-instance --runtime-config config/kaguya.local.toml
```

Initialization requires an unselected deployment. It validates readable persona
sources before creating directories, generates IDs and a new UUID directory,
copies persona, snapshots an existing RAG source with SQLite `VACUUM INTO`,
then publishes and reads back the local manifest. Missing RAG source means
startup will create an empty RAG database. Sources are preserved.

Output is JSON containing `instance_id`, `default_session_id`, absolute
`manifest` and `instance_directory`, `manifest_reference`, and
`selection_changed: false`. Put the printed reference into that local root:

```toml
[instance]
manifest = "instances/<printed InstanceId>/instance.toml"
```

Neither initialization nor rekeying edits the deployment file. Normal startup
requires a valid selected manifest and readable instance persona; it never
mints an identity to recover from an invalid or missing selection.

`rekey-instance` requires a selected, valid source instance and acquires its
leases before copying. It creates new instance/session IDs and a separate
UUID directory containing a snapshot of the existing persona/RAG data. The
source manifest, data, and deployment selection remain intact. Select the
new printed reference explicitly and restart bound services to activate it.
A restore that retains the manifest retains both IDs.

Identity publication is fail-closed, not a complete interruption-recovery
protocol. Failed operations may leave incomplete new directories/manifests;
there is no directory guessing, automatic fallback, or replacement-ID recovery.
Safe identity update publication and deterministic interrupted-init/rekey
recovery are required before R1c acceptance. Full backup/export and consistent
whole-instance snapshots remain R8 follow-ups.

## Active locks, bindings, and originating context

Gateway holds an instance-ID OS lock under the host/user's temporary
`kaguya-instance-locks` directory plus a `.owner.lock` in the data directory.
These handles live in `InstanceLease` for the Gateway lifetime. They reject
duplicate local restored instances under the same host/user and concurrent
use of one data directory. Lock-file presence alone is not an active claim;
the OS lock is. Runtime lock files are separate from portable identity data.
Distributed duplicate-instance coordination is not implemented.

Terminology follows the actual relationship:

| Relationship | Term and implementation |
| --- | --- |
| Instance owns persona/RAG data and allocations | Data/resource ownership |
| Service accepts one instance/session pair | Fixed `proto::SessionRef` in Rust; immutable `InstanceSessionBinding` in Python |
| Request/callback/event carries its originating IDs | Instance/session context or correlation |

Supervisor reads and validates its configured pair before launch, stores the
generated `proto::SessionRef` in its binding state and allocation ownership
records, and enforces policy through `bind_instance_session()` /
`require_instance_session()`. The Python `InstanceSessionBinding` in
`talker/binding.py` is immutable and constructed
before voice services accept traffic. Talker, Listener gRPC, and audio ingress
share it. Missing or invalid startup IDs stop startup; no first caller can
claim the process. A different instance or session is rejected. Gateway retains
the fatal voice-binding rejection path through P0.

Gateway constructs its clients, tools, history and `SessionState` with the
validated generated `proto::SessionRef`. Tool and Reasoner callbacks are
labeled by their Gateway adapter and checked before the pipeline consumes them. This is
originating correlation, not independent verification of a remote provider.
Supervisor's HTTP adapters retain scalar `instanceId` / `sessionId` JSON and
construct or extract the generated reference at that boundary.
Authoritative history, Reasoner implementation/run lifecycle, authorization,
multiplexing, and ownership transfer remain their separate stages.

The coordinated protocol cleanup intentionally replaces the former wire
contract; no compatibility aliases remain. A protobuf `SessionRef` contains
named `InstanceId { value }` and `SessionId { value }` messages. Unary requests
carry this assertion, while `ListenerInit`, `GenerationStart`, and Reasoner's
initial `TaskRequest` establish stream scope. Voice validates the initial
message before sending response headers or mutating stream state; malformed,
misordered, duplicate or mismatched initialization fails. Clients enqueue that
first message before waiting for headers, avoiding a startup deadlock. Scope is
an assertion against the configured pair, not a routing or authentication key.
Custom instance/session gRPC metadata and `InstanceIdentity.grpc_request()` are
removed. Canonical `session_id` replaces `conversation_id` in bootstrap and
telemetry, and `generation_id` replaces the old Talker dispatch `turn_id`.

`GenerationStart` carries shared `PromptContext` plus exactly one typed trigger:
user input, tool result (`request_id`), Reasoner result/progress (`task_id`), or
silence. Active tasks are typed rather than JSON inside the prompt context.
`UpdatePersonaRequest` remains a separate scoped RPC on startup, reconnect and
persona changes; persona is not resent with every generation. Every
`TalkerOutput` carries the accepted `GenerationStart.generation_id` in its outer
envelope. Gateway validates that field against the stream's expected generation
and forwards the generated protobuf directly; there is no local output wrapper
or duplicate generation field in lifecycle payloads. Console associates events
with that generation rather than whichever response is currently open. This
preserves provenance without introducing a new stale-output policy.

For canonical UUID strings, `SessionRef` has an 80-byte protobuf payload
(82 bytes when embedded as a small-numbered message field). It is sent at
stream setup or in scoped unary/control messages, not in every output or audio
frame.

`TalkerOutput.generation_id` is a separate per-output field: a 36-character UUID
uses 38 protobuf bytes. It accompanies every semantic output, including barge-in
acknowledgments, but does not add bytes to audio frames. This pre-alpha protocol
permits coordinated breaking changes: `TalkerOutput` uses fields 1/2 for
`seq`/`generation_id` and 3-9 for payloads. `ResponseStarted` is empty and
`ResponseComplete.was_interrupted` is field 1, without compatibility reservations.
`ListenerInput` likewise uses `init = 1` and `control = 2`, without reserving the
removed in-band audio field; audio continues over the separate raw socket.
All components must use the regenerated schema together.

The raw audio connection still sends its existing header once:

```text
connection: [ KAGUYA1: 7 bytes ][ Instance UUID: 16 ][ Session UUID: 16 ]
            <-------------------- 39 bytes ------------------------>
frames:     [ length: 4 bytes ][ raw audio bytes ] ...
```

The header is computed once per Gateway audio forwarder and compared with the
configured pair on each new connection before audio acceptance. There is no
additional per-frame protobuf work or new handshake round trip. These byte
counts describe the encoding; no latency/throughput benchmark is claimed.

Sandbox `AllocationId`, opaque handle, tool request ID, and durable session
remain separate. Release permits a fresh allocation under the same session.
Docker's `supervisor_run_id` stays ephemeral and its existing container label
is retained. Full workspace/task/policy binding and failure reconciliation
remain R5.

## Read-only validation and redacted diagnostics

Gateway and Supervisor support `validate-config` and `show-config`, with the
same selection flags. For example:

```sh
cargo run --manifest-path gateway/Cargo.toml -- show-config --for-init --runtime-config config/kaguya.local.toml
cargo run --manifest-path supervisor/Cargo.toml -- validate-config --runtime-config config/kaguya.local.toml
```

These commands do not create instance/lock directories, acquire leases, open
SQLite databases, bind ports, or launch processes. `--for-init` permits an
unset or missing manifest; any existing manifest is still validated. The init
operation itself still requires an unselected deployment. Gateway also checks
its typed settings and initialization sources when no instance exists;
Supervisor checks Gateway TOML syntax and owns its topology validation.

The implemented subset reports selected root/profile, path mode, portable
root, component file, manifest/data paths, selection provenance and applied
override sources. Gateway includes initialization-source paths. Output uses
an allowlist and omits process environment values and launch commands;
configuration parse/endpoint errors omit sensitive source snippets and URLs.
This is not deployment-wide endpoint/environment validation or proof that
runtime resources can be acquired. R1d completes those diagnostics.

Console supports `npm run dev -- --runtime-config PATH --profile NAME` and
forwards other Vite arguments. Its selection/launch tests cover paths with
spaces, cwd changes, authoritative child environment, profile errors, and
nonmutating resolution. Its explicit TOML projection rejects unsupported
multiline/array-of-table syntax; full reader consolidation remains R1d.

## Launch trust and remaining acceptance

Deployment configuration contains trusted executable instructions. Current
`command` / `command_win32` values execute through platform shells, including
the existing Console launcher. Editing them can change executed code. R1d
will define structured executable/argument descriptors, explicit shell use,
and compatibility rules; this pass does not claim that migration is complete.

The canonical implementation plan records Windows checks and outstanding
macOS/deployment/live-voice acceptance. No actual instance was initialized in
this checkout during the closure pass; the existing initialization sources
were preserved. See [R1a through R1d](implementation-plan-v0.1.0.md#r1--session-identity-and-persistence)
for the accepted work split and durability gates.
