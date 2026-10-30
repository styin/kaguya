# Telemetry and Logging End-to-End Testing

Current Phase 1 observability has two Supervisor-owned cold paths:

```text
Gateway / Talker stdout+stderr
        -> Supervisor process log forwarder
        -> LogStore ring buffer
        -> GET /api/logs and GET /api/logs/stream
        -> dev console / test client

Gateway / Supervisor sandbox raw events
        -> Supervisor TelemetryHub
        -> aggregation worker
        -> GET /api/telemetry/events and GET /api/telemetry/stream
        -> GET /api/metrics/snapshot
```

The proto `Telemetry` RPC describes a planned background-context subscription;
there is no implementing Reasoner service.
Do not use it as the Phase 1 telemetry acceptance test.

## What to verify

An end-to-end telemetry test should prove four things:

1. A real runtime event happens, such as RAG retrieval, Talker dispatch, or
   sandbox tool execution.
2. The responsible component emits a raw event with correlation fields.
3. Supervisor stores the event and feeds it to the aggregation worker.
4. The HTTP snapshot, SSE stream, and metrics snapshot expose the event and
   derived counters to clients.

Useful correlation fields:

- `instance_id` / `session_id`: the configured pair; serialized as `instanceId` / `sessionId` in the HTTP telemetry envelope.
- `generation_id`: the Talker generation, serialized as `generationId` when applicable.
- `request_id`: the tool call, serialized as `requestId` when applicable.
- `allocation_id`: the sandbox allocation, serialized as `allocationId` when applicable.
- `tool`, `backend`, `handle`: operation-specific log/event fields.

Gateway telemetry ingress requires its configured instance/session pair. Missing
or mismatched ownership is rejected before event storage/aggregation. Supervisor's
own process-resource events describe managed processes and need not identify a
Talker generation. Numeric event IDs, `since` cursors, and `sample_id` counters
remain separate from application identity.

## Raw telemetry API

Gateway uploads raw events without blocking the hot path:

```text
POST /api/telemetry/events
```

Supervisor exposes:

```text
GET /api/telemetry/events?since=<id>
GET /api/telemetry/stream
GET /api/metrics/snapshot
```

Initial event kinds:

| Event kind | Source | Purpose |
| --- | --- | --- |
| `rag.retrieve.completed` | Gateway | RAG duration, hit count, top score, source mix |
| `talker.dispatch.started` | Gateway | Context size/position summary before Talker dispatch |
| `talker.first_output` | Gateway | Dispatch-to-first-TalkerOutput latency |
| `talker.first_sentence` | Gateway | Dispatch-to-first-sentence latency |
| `sandbox.exec.completed` | Supervisor | Sandbox backend, duration, exit/timeout/output size |
| `process.resource.sample` | Supervisor | Managed process CPU/RSS/virtual memory sample |

## Local smoke test

Select an initialized local deployment first, following [the instance setup
instructions](r1a-identity.md). Both terminals must use that same runtime file.
The examples below use `config/kaguya.local.toml`.

Start Supervisor:

```powershell
$runtimeConfig = (Resolve-Path config/kaguya.local.toml).Path
cargo run --manifest-path supervisor/Cargo.toml -- --runtime-config $runtimeConfig
```

In another terminal, verify the log snapshot endpoint:

```powershell
Invoke-RestMethod http://127.0.0.1:3001/api/logs
```

Verify the streaming endpoint:

```powershell
curl.exe -N http://127.0.0.1:3001/api/logs/stream
```

Then trigger a sandbox execution through the Supervisor control plane:

```powershell
$base = "http://127.0.0.1:3001"
$runtimeConfig = (Resolve-Path config/kaguya.local.toml).Path
$deployment = cargo run --quiet --manifest-path gateway/Cargo.toml -- `
  show-config --runtime-config $runtimeConfig | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw "Gateway deployment validation failed" }
$instanceId = $deployment.instance.instance_id
$sessionId = $deployment.instance.default_session_id
$acquireBody = @{
  instanceId = $instanceId
  sessionId = $sessionId
  allocationId = [guid]::NewGuid().ToString()
} | ConvertTo-Json -Compress
$acquired = Invoke-RestMethod "$base/api/sandbox/acquire" `
  -Method POST -ContentType "application/json" -Body $acquireBody
if (-not $acquired.handle) { throw $acquired.error }

$body = @{
  instanceId = $instanceId
  sessionId = $sessionId
  requestId = [guid]::NewGuid().ToString()
  argsJson = '{"language":"python","code":"print(\"telemetry-smoke-ok\")"}'
} | ConvertTo-Json -Compress
$executed = Invoke-RestMethod "$base/api/sandbox/$($acquired.handle)/execute" `
  -Method POST -ContentType "application/json" -Body $body
$executed.content | ConvertFrom-Json

$releaseBody = @{ instanceId = $instanceId; sessionId = $sessionId } | ConvertTo-Json -Compress
Invoke-RestMethod "$base/api/sandbox/$($acquired.handle)" `
  -Method DELETE -ContentType "application/json" -Body $releaseBody
```

Expected result:

- the sandbox response contains `telemetry-smoke-ok`;
- `/api/logs` includes Supervisor sandbox acquire/execute/release entries;
- `/api/logs/stream` emits JSON `data:` frames for new log entries;
- `/api/telemetry/events` includes a `sandbox.exec.completed` event;
- `/api/metrics/snapshot` increments `sandbox.executions`;
- after app processes are started, `/api/telemetry/events` includes
  `process.resource.sample` events for managed PIDs;
- `/api/metrics/snapshot` exposes `process.lastTotalRssBytes`,
  `process.lastTotalCpuPercent`, and process RSS peaks;
- log entries include a `level` field when the line contains a recognizable
  level token such as `INFO`, `WARN`, `WARNING`, `ERROR`, `DEBUG`, or `TRACE`.

Inspect telemetry:

```powershell
Invoke-RestMethod "$base/api/telemetry/events"
Invoke-RestMethod "$base/api/metrics/snapshot"
curl.exe -N "$base/api/telemetry/stream"
```

## Gateway-to-Supervisor tool-chain smoke test

The Gateway test suite already has an in-process end-to-end sandbox tool test:

```powershell
cargo test --manifest-path gateway/Cargo.toml sandbox_tool_dispatch_completes_full_supervisor_chain -- --nocapture
```

That test verifies:

```text
ToolRegistry.dispatch
  -> SandboxClient.acquire/execute over HTTP
  -> Supervisor sandbox API
  -> SandboxProvider
  -> SandboxManager
  -> selected backend
  -> P3 ToolResult
```

For a running app, use Supervisor as the process owner so Gateway stdout/stderr
is captured:

```powershell
$runtimeConfig = (Resolve-Path config/kaguya.local.toml).Path
cargo run --manifest-path supervisor/Cargo.toml -- --runtime-config $runtimeConfig
```

Then inspect:

```powershell
Invoke-RestMethod http://127.0.0.1:3001/api/app/status
Invoke-RestMethod http://127.0.0.1:3001/api/logs
curl.exe -N http://127.0.0.1:3001/api/logs/stream
```

When a `sandbox_exec` tool request is triggered by Gateway, the log stream
should show the same `request_id` at Gateway dispatch and
the matching `handle`/`backend` at sandbox execution. Raw telemetry adds the
configured `instanceId`/`sessionId`, the execution's `requestId` and `allocationId`,
and (for generation/RAG events) `generationId` where applicable. There is no
separate conversation identity or duplicate session alias.

The telemetry stream should also show:

```text
rag.retrieve.completed
talker.dispatch.started
talker.first_output
talker.first_sentence
sandbox.exec.completed
process.resource.sample
```

## Console generation correlation

At WebSocket open, `session_init.data` supplies the scalar `instance_id` and
`session_id`. `response_started` and
`response_complete` identify `generation_id`. Sentence and emotion events also
carry `data.generation_id`, projected from the outer `TalkerOutput.generation_id`
after Gateway validates it against the originating Converse stream. Inner
protobuf payloads do not repeat it. The Console associates those events with
that generation, including delayed
output, and does not append unknown/missing references to an unrelated response.

For a manual Console check, start a second generation while output from the first
is still queued. Sentence/emotion events must retain the first generation's ID.
This checks provenance and display association; it does not establish a new
stale-event rejection or cancellation policy. `/ws/debug` and generation snapshot
inspection remain deferred.

## Automated checks

Run the log-store tests:

```powershell
cargo test --manifest-path supervisor/Cargo.toml logs
```

Run the telemetry hub/API tests:

```powershell
cargo test --manifest-path supervisor/Cargo.toml telemetry
```

Run the sandbox backend contract suite:

```powershell
cargo test --manifest-path supervisor/Cargo.toml --test sandbox_backend_contract -- --nocapture
```

On Windows, include Job Object coverage:

```powershell
cargo test --manifest-path supervisor/Cargo.toml --features sandbox-jobobject --test sandbox_backend_contract -- --nocapture
```

Docker coverage requires:

- Docker Desktop running;
- `kaguya-sandbox:latest` built from `docker/sandbox.Dockerfile`.

Build the image:

```powershell
docker build -f docker/sandbox.Dockerfile -t kaguya-sandbox:latest .
```

Bubblewrap coverage requires a Linux host with `bwrap` installed.

## Logging expectations

- Rust components use `tracing` key-value fields instead of embedding IDs into
  prose.
- Python Talker logs use standard `logging` output; Supervisor derives `level`
  from the formatted line.
- Supervisor never stores empty lines, ANSI color codes, or transient voice UI
  spinner lines.
- Sensitive payloads such as full tool code or large file contents should not
  be logged. Log IDs, backend names, handles, and state transitions instead.
