# Instance lifecycle and deployment selection (R1a)

The runtime TOML is the deployment entry point. It selects component settings,
an explicit portable data root, and one instance-local manifest. Gateway owns
instance creation, forking, persona initialization, and active data leases.
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
Gateway persona templates and file-tool workspace
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
different, non-nil, canonical UUID strings. Generation uses UUIDv4. The manifest
is the sole identity authority. Changing it does not rename the instance directory; this
does not add live session switching, and running services still enforce their
existing pair binding.

`gateway/src/instance.rs` keeps the compact lifecycle boundary:
`InstanceIdentity`, `InstancePaths`, `InstanceLease`, `init_instance()`,
`fork_instance()`, and `load_required()`. Instance identity survives restore
to another host and does not identify the physical machine. `IDENTITY.md`,
`identity_md`, and the voice `IdentityConfig` describe mutable persona identity
content, not InstanceId. The manifest remains a storage/lifecycle concern.

Each Rust process holds its instance/session pair as one `SessionScope` from
the `kaguya-protocol` crate (`protocol/`), which also owns the generated
protobuf code. It is validated once, where the pair enters the process: from
the manifest at startup, or from a gRPC `SessionRef` or HTTP request. Inside the
process the type carries that guarantee, so components do not re-validate or
compare copies of it. Validation requires canonical, non-nil, distinct UUIDs.
Clients convert the scope to the generated `SessionRef` only when they write a
wire message. The existing `shared/deployment.rs` resolver is still included
into both crates by source path; R1d moves deployment resolution into the
launcher.

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
copies persona templates, then publishes and reads back the local manifest.
Startup creates an empty instance-owned RAG database. Initialization never
borrows or snapshots a previous database, and there is no external database
source setting. Templates are preserved. Use `fork-instance` when an independent
copy of a selected instance and its existing data is intended.

`init-instance --dry-run` checks the same preconditions (no instance selected,
readable persona sources, a portable root that is absent or a directory), prints
the resolved selection, instances directory and initialization sources with
`"dry_run": true`, and creates nothing. It prints no IDs, because IDs are
generated only at creation. A dry run shows eligibility; it cannot promise that
creation will succeed.

Output is JSON containing `instance_id`, `default_session_id`, absolute
`manifest` and `instance_directory`, `manifest_reference`, and
`selection_changed: false`. Put the printed reference into that local root:

```toml
[instance]
manifest = "instances/<printed InstanceId>/instance.toml"
```

Neither initialization nor forking edits the deployment file. Normal startup
requires a valid selected manifest and readable instance persona; it never
mints an identity to recover from an invalid or missing selection.

`fork-instance` requires a selected, valid source instance and acquires its
leases before copying. It creates new instance/session IDs and a separate
UUID directory containing a snapshot of the existing persona/RAG data. The
source manifest, data, and deployment selection remain intact. Select the
new printed reference explicitly and restart bound services to activate it.
A restore that retains the manifest retains both IDs.

Identity publication is fail-closed, not a complete interruption-recovery
protocol. Failed operations may leave incomplete new directories/manifests;
there is no directory guessing, automatic fallback, or replacement-ID recovery.
Safe identity update publication and deterministic interrupted-init/fork
recovery are required before R1c acceptance. Full backup/export and consistent
whole-instance snapshots remain R8 follow-ups.

## Active locks, scope, and process boundaries

Gateway holds an instance-ID OS lock in a per-user directory,
`kaguya/instance-locks` under the platform's local data directory (created
with mode `0700` on Unix), plus a `.owner.lock` in the data directory. These
handles live in `InstanceLease` for the Gateway lifetime. They reject duplicate
local restored instances for the same user and concurrent use of one data
directory. The lock directory deliberately does not follow `TMPDIR`, which
differs between launch contexts. Lock-file presence alone is not an active
claim; the OS lock is. Runtime lock files are separate from portable identity
data. Distributed duplicate-instance coordination is not implemented.

Kaguya runs one session per instance. Multiple sessions may come later, so the
scope stays on the wire as an extension point, without defensive checks inside
a process:

| Where | What happens |
| --- | --- |
| Process startup | Gateway and Supervisor read the manifest into one `SessionScope`; voice reads `KAGUYA_INSTANCE_ID`/`KAGUYA_SESSION_ID` into its immutable `InstanceSessionBinding` |
| Messages that open a request or stream | Carry `SessionRef`: `ListenerInit`, `GenerationStart`, `PrefillRequest`, `UpdatePersonaRequest`, `ControlRequest`, `TaskRequest`, `InterruptRequest`, `TelemetrySubscribe`; plus the raw-audio header |
| Later messages on a stream | Carry no scope; the first message set it (`BargeInSignal`, `TaskContext`) |
| Receiving process | Checks the scope once against its configured pair before any effect |
| Inside a process | No scope fields on internal events or calls; clients stamp the scope on what they send |

Supervisor reads and validates its pair before launch, passes it to
`SupervisorApp::new`, and serves exactly that scope. Its HTTP adapters keep
flat `instanceId` / `sessionId` fields and check them in each sandbox and
telemetry handler (`require_scope`). `/api/sandbox/status` reports the served
pair; Gateway compares it at startup and stops on a mismatch. An unreachable
Supervisor only disables the sandbox tool. The Python `InstanceSessionBinding`
in `talker/binding.py` is constructed before voice services accept traffic and
is shared by Talker, Listener gRPC and audio ingress. Missing or invalid startup
IDs stop startup; no first caller can claim a process.

A scope mismatch has one signal and one outcome. Voice services answer
`FAILED_PRECONDITION`; Gateway treats it as fatal from both `UpdatePersona` and
Listener initialization and shuts down through P0. `INVALID_ARGUMENT` means a
malformed Gateway request: it is logged and the connection is marked degraded,
not reported as a binding failure. The audio socket closes silently on a
mismatched header; the Listener gRPC stream on the same process rejects the
mismatch first.

`GenerationStart` carries shared `PromptContext` plus exactly one typed trigger:
user input, tool result (`request_id`), Reasoner result/progress/failure (`task_id`), or
silence. Active tasks are typed rather than JSON inside the prompt context.
`UpdatePersonaRequest` remains a separate scoped RPC on startup, reconnect and
persona changes; persona is not resent with every generation. Every
`TalkerOutput` carries the accepted `GenerationStart.generation_id` in its outer
envelope. Gateway validates that field against the stream's expected generation
and forwards the generated protobuf directly. Console associates events with
that generation rather than whichever response is currently open. Filtering
stale output by generation is the generation-correctness step before R1c.
Talker acknowledges only the first barge-in of a `Converse`.

For canonical UUID strings, `SessionRef` has an 80-byte protobuf payload
(82 bytes when embedded as a small-numbered message field). It is sent at
stream setup or in scoped unary/control messages, not in every output or audio
frame.

`TalkerOutput.generation_id` is a separate per-output field: a 36-character UUID
uses 38 protobuf bytes. It accompanies every semantic output, including barge-in
acknowledgments, but does not add bytes to audio frames. The canonical protobuf
schema owns field numbering and message shapes.
The Listener request carries only `ListenerInit`; Gateway half-closes it, and
Listener keeps the subscription. `ListenerControl`/`ListenerReset` stay declared
as an extension point and answer `UNIMPLEMENTED`. Gateway likewise retains
`ApprovalSignal` in its control schema and answers approvals with
`UNIMPLEMENTED` after scope validation and before P0 dispatch. Audio uses the
separate raw socket. All components must use the regenerated schema together.

The raw audio connection sends its header once:

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
Docker's `supervisor_run_id` stays ephemeral and uses the
`kaguya.sandbox.supervisor_run` ownership label. Full workspace/task/policy binding and failure reconciliation
remain R5. A sandbox backend that is explicitly enabled but fails to initialize
stops the Supervisor; the Docker backend requires a running daemon.

## Read-only validation and redacted diagnostics

Gateway and Supervisor support `validate-config` and `show-config`, with the
same selection flags. Both require a selected, initialized instance; before
initialization, use `init-instance --dry-run`. For example:

```sh
cargo run --manifest-path gateway/Cargo.toml -- init-instance --dry-run --runtime-config config/kaguya.local.toml
cargo run --manifest-path gateway/Cargo.toml -- show-config --runtime-config config/kaguya.local.toml
cargo run --manifest-path supervisor/Cargo.toml -- validate-config --runtime-config config/kaguya.local.toml
```

These commands, including the dry run, do not create instance/lock
directories, acquire leases, open SQLite databases, bind ports, or launch
processes. Gateway checks its typed settings; Supervisor checks Gateway TOML
syntax and owns its topology validation.

The implemented subset reports selected root/profile, path mode, portable
root, component file, manifest/data paths, selection provenance (only for
settings that have a value) and applied override sources. The dry run adds
initialization-source paths. Output uses an allowlist and omits process
environment values and launch commands. Configuration errors give the line
and column, and name the key for unknown, missing or duplicate keys; errors
that could quote a configured value report only their kind. Endpoint errors
name the endpoint, never its address.
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
and validated startup inputs; that consolidation remains unimplemented.

The canonical implementation plan records validation and outstanding
macOS/deployment/live-voice acceptance. See [R1a through R1d](implementation-plan-v0.1.0.md#r1--session-identity-and-persistence)
for the accepted work split and durability gates.
