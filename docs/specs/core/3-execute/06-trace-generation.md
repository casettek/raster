## Execute: Trace Generation

This document specifies the **current execution trace data model and tracing hooks as implemented today**, and the **intended step-level trace records** needed to support analysis, replay, and verification workflows.

This spec is written to match the code as it exists. Where the runtime does not yet emit the records described here (or where the core trace types are missing required fields), those gaps are explicitly called out.

---

## Code audit tasks (where to look)

### Core trace data types

- `crates/raster-core/src/trace.rs`
  - `Trace`
  - `TraceEvent` (`#[serde(tag = "type")]`)
    - `TileStart { tile_id, timestamp, depth }`
    - `TileEnd { tile_id, timestamp, duration_ns, cycles }`
    - `SequenceStart { name, timestamp }`
    - `SequenceEnd { name, timestamp, duration_ns }`
- `crates/raster-core/src/tile.rs`
  - `TileId` (serde newtype; serialized as a string in JSON-like formats)

### Runtime tracing hook points (tile I/O trace items + commitments)

- `crates/raster-runtime/src/tracing.rs`
  - `init`, `init_with`, `finish`, `__emit_trace`
- `crates/raster-runtime/src/tracing/subscriber.rs`
  - `trait Subscriber::{on_trace, on_complete}`
- `crates/raster-runtime/src/tracing/subscriber/json.rs`
  - `JsonSubscriber` (serializes `raster_core::trace::TraceItem` to a writer; stdout by default)
- `crates/raster-runtime/src/tracing/subscriber/commit.rs`
  - `CommitSubscriber` (writes packed commitment blocks to a writer)
- `crates/raster-runtime/src/tracing/subscriber/audit.rs`
  - `AuditSubscriber` (compares recomputed packed blocks against an expected file and reports the first mismatch)

### “Step record” required fields (where they come from in today’s code)

- **Artifact identity**
  - `crates/raster-backend/src/backend.rs`
    - `CompilationOutput::{method_id}` (bytes)
  - `crates/raster-backend-risc0/src/risc0.rs`
    - `compute_image_id(&elf)` produces the RISC0 image id (“method id”)
  - `crates/raster-backend/src/native.rs`
    - Native backend uses `tile_id` bytes as a placeholder `method_id`
  - `crates/raster-compiler/src/builder.rs`
    - `TileArtifact::{method_id}` (hex string) and `TileManifest::{method_id, backend, source_hash?}`
- **Input/output bytes**
  - `crates/raster-backend/src/backend.rs`
    - `Backend::execute_tile(..., input: &[u8], ...) -> TileExecution { output: Vec<u8>, ... }`
  - `crates/raster-backend-risc0/src/risc0.rs`
    - zkVM guest input convention: executor env writes a `u32` byte-length prefix, then the raw input bytes

### Recursion / iteration shape (control flow)

- `crates/raster-core/src/schema.rs`
  - `ControlFlow::Loop { body, max_iterations }` (iteration is modeled, but **no trace fields record iteration index yet**)

---

## Trace: data model

### Trace container

A trace is a single ordered event log:

- `Trace.run_id` **MUST** uniquely identify the run within the trace sink (file, database, etc.).
- `Trace.timestamp` **SHOULD** be the trace creation time in nanoseconds since the Unix epoch.
  - **Current implementation gap**: Raster does not provide an event-trace emitter/persistence layer in this workspace that populates `timestamp`; tooling often uses placeholder values (e.g., `0`).
- `Trace.events` **MUST** be ordered in the exact order the tracer observed them.

Current struct shape (authoritative):

- `Trace { run_id: String, timestamp: u64, events: Vec<TraceEvent> }`

### TraceEvent variants (current code)

`TraceEvent` is a tagged enum with tag key `"type"` (serde `#[serde(tag = "type")]`). The currently defined variants are:

- `TileStart { tile_id: TileId, timestamp: u64, depth: u32 }`
- `TileEnd { tile_id: TileId, timestamp: u64, duration_ns: u64, cycles: Option<u64> }`
- `SequenceStart { name: String, timestamp: u64 }`
- `SequenceEnd { name: String, timestamp: u64, duration_ns: u64 }`

**Current implementation gap**: there are **no variants** for (a) step-level input/output bytes, (b) artifact identity, (c) receipt/proof metadata, or (d) failure/error recording.

---

## Serialization format (as supported by current types)

The trace types are serde-serializable. Any serialization used by a trace sink **MUST** preserve:

- Event ordering (`events[i]` happens-before `events[i+1]` as observed by the tracer).
- Exact variant names for `TraceEvent` (unless a versioned wrapper is introduced).
- Exact field names as written in code (`tile_id`, `duration_ns`, etc.).

Example JSON encoding (illustrative of serde’s default externally tagged behavior with `tag = "type"`):

```json
{
  "run_id": "run-2026-01-22T12:34:56Z",
  "timestamp": 0,
  "events": [
    { "type": "SequenceStart", "name": "main", "timestamp": 100 },
    { "type": "TileStart", "tile_id": "hello", "timestamp": 110, "depth": 0 },
    { "type": "TileEnd", "tile_id": "hello", "timestamp": 210, "duration_ns": 100, "cycles": 12345 },
    { "type": "SequenceEnd", "name": "main", "timestamp": 220, "duration_ns": 120 }
  ]
}
```

**Current implementation gap**: Raster does not provide an event-trace persistence implementation in this workspace; `Trace`/`TraceEvent` are data types only.

## `TraceItem` JSON emission (implemented; delimiter caveat)

In addition to the coarse `TraceEvent` model, Raster defines a tile I/O transcript record:

- `raster_core::trace::TraceItem` (includes `fn_name`, signature metadata, and base64 `postcard` input/output bytes).

The default runtime subscriber (`raster_runtime::JsonSubscriber`) serializes each `TraceItem` using `serde_json::to_writer` onto the configured writer (stdout by default).

**Delimiter caveat (important)**: the current implementation does not write an explicit delimiter (e.g., newline) between items. Consumers MUST treat this as a raw JSON stream rather than line-delimited JSON.

---

## Event ordering and nesting invariants

When traces are emitted by an executor, the following invariants apply:

- A `TileStart` event **MUST** appear before the corresponding `TileEnd` event for the same logical tile invocation.
- Tile invocations **MUST NOT** overlap at the same `depth` unless the executor explicitly defines a concurrency model and the trace format is extended to disambiguate spans.
- If nested execution is supported (e.g., sequences invoking sub-sequences), then:
  - `depth` in `TileStart` **MUST** equal the dynamic nesting depth at the moment the tile begins execution.
  - `depth` **SHOULD** increase by exactly 1 when entering a nested scope and decrease by exactly 1 when exiting it.
  - **Current implementation gap**: there is no runtime emitter today, so the meaning of `depth` is not enforced anywhere.

Sequence events:

- A `SequenceStart { name }` event **MUST** appear before the corresponding `SequenceEnd { name }` event for that dynamic sequence invocation.
- Sequence nesting is not currently represented explicitly (no `depth` field on sequence events).
  - Implementations that need nested sequence traces **SHOULD** either:
    - Add `depth` to `SequenceStart/SequenceEnd`, or
    - Introduce a span identifier model (see “Step records” and “Iteration/recursion recording” below).

---

## Step record format (required behavior; not fully implemented yet)

The runtime and analysis tooling require per-invocation “step records” that include at minimum:

- `TileId` (which tile ran),
- Artifact identity (which compiled artifact ran),
- Input bytes (what was provided to the tile),
- Output bytes (what the tile produced).

### Required fields

For each tile invocation, the trace **MUST** contain a logical step record with the following fields:

- `tile_id: TileId`
- `artifact` (artifact identity):
  - `backend: String` (e.g., `"risc0"`, `"native"`)
  - `method_id: Bytes` (the backend’s method/image id)
    - For the RISC0 backend, this corresponds to `CompilationOutput.method_id` (derived from the ELF).
    - **Current implementation gap**: the core trace model has nowhere to store `backend`/`method_id`.
- `input_bytes: Bytes`
  - These bytes **MUST** be exactly the `input: &[u8]` slice passed into `Backend::execute_tile`.
  - Note (RISC0 guest convention): the backend writes an internal `u32` length prefix into the zkVM executor environment, but `input_bytes` here refers to the raw input payload bytes (excluding the prefix).
- `output_bytes: Bytes`
  - These bytes **MUST** be exactly the `TileExecution.output` returned from `Backend::execute_tile`.
- Optional performance fields (when available):
  - `cycles: Option<u64>` (actual cycles)
  - `proof_cycles: Option<u64>` (padded proof cost estimate for zkVM backends)
- Optional proof fields (when available):
  - `receipt: Option<Bytes>` (serialized receipt/proof)
  - `verified: Option<bool>`

### Representation in the current TraceEvent model (gap)

**Current implementation gap**: `TraceEvent` does not define any variant that can carry `artifact`, `input_bytes`, `output_bytes`, or proof data. The current model only supports coarse `TileStart/TileEnd` markers.

Until a step-carrying variant exists, implementations **SHOULD** treat a “step record” as a conceptual record derived from:

- One `TileStart { tile_id, ... }`, and
- One `TileEnd { tile_id, ... }`,

but this derived record will necessarily be missing required fields.

### Suggested event extension (for implementers)

To make the spec satisfiable, `TraceEvent` **SHOULD** be extended with either:

- A single `TileStep` event containing all required fields, or
- A span-based model with `TileStart` containing `input_bytes`+`artifact` and `TileEnd` containing `output_bytes`+metrics, tied together by a unique `span_id`.

This spec does not mandate the exact encoding choice yet, but it **MUST** be versioned to avoid breaking existing deserializers.

---

## Failure records

When a tile invocation or sequence fails, the trace **MUST** include a failure record that captures:

- Which operation failed (tile or sequence),
- A stable error code/category (when available),
- A human-readable message,
- Enough context to correlate the failure to a specific invocation (span id / depth + ordering + iteration index).

**Current implementation gap**: no failure variants exist in `TraceEvent`. For tile I/O tracing, failures typically surface as process errors/panics (e.g., when auditing commitments) or as `Result::Err(...)` returns from execution entry points.

Recommended minimum failure records (for implementers) are:

- `TileFailure { tile_id, timestamp, error: { code?, message }, depth, artifact?, input_bytes? }`
- `SequenceFailure { name, timestamp, error: { code?, message } }`

If a failure occurs after a `TileStart` has been emitted, the trace **SHOULD** include a terminal event for that invocation (either `TileFailure` or a `TileEnd` with an error field) so that analyzers do not hang waiting for completion.

---

## Iteration and recursion recording

Raster control flow can include loops (`ControlFlow::Loop`) and nested invocations (sequences calling other sequences).

For iteration/recursion to be analyzable and replayable, step records **MUST** be attributable to:

- A specific loop instance and iteration index, when applicable, and
- A specific dynamic call stack (sequence nesting), when applicable.

**Current implementation gap**: the current trace model includes only `depth` on `TileStart` and does not record:

- Which loop body produced a given tile invocation,
- The iteration index within a loop,
- A stable span identifier model to correlate start/end and parent/child relationships.

Implementations that add step records **SHOULD** add one of:

- `iteration: Option<u64>` on tile step/span events, where `iteration` counts from 0 within the loop body execution, or
- A general `span_id`/`parent_span_id` model plus a `path` field (e.g., sequence name + item index + iteration index) to uniquely locate the invocation in the control-flow execution.

---

## Implementation status summary (what exists vs what’s required here)

- Trace container and four event marker variants exist (`TileStart/TileEnd/SequenceStart/SequenceEnd`).
- There is currently **no event-trace emitter/persistence** implementation in this workspace for `TraceEvent`s.
- There is an implemented **tile I/O trace subscriber** surface (`TraceItem`), including:
  - stdout JSON emission (`JsonSubscriber`),
  - a packed commitment file writer (`CommitSubscriber`), and
  - a packed commitment checker (`AuditSubscriber`).
- The spec-required step record fields (artifact identity, input bytes, output bytes) are **available in other subsystems** (`Backend::execute_tile`, `CompilationOutput.method_id`, `TileExecution.output`) but are **not representable** in `TraceEvent` yet.
- Failure and iteration/recursion recording are **not representable** in `TraceEvent` yet.
