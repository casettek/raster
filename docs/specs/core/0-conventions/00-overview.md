# 00. Overview

This document defines the **project-level conventions** for Raster Core as implemented today. It is intended for implementers of:

- Raster crates (`raster-core`, `raster-macros`, `raster-compiler`, `raster-runtime`, `raster-backend*`)
- Tooling that consumes Raster artifacts (e.g., schema readers, artifact managers, future verifiers)

Where the broader Core specs describe behavior that is not yet implemented (or implemented differently), this document records the gap explicitly.

---

## A. Code audit tasks (where to look)

Use this section as the “map” from spec concepts to code locations.

### A.1 Public API entrypoints (what user code imports)

- **User-facing crate re-exports**
  - `crates/raster/src/lib.rs` (exports `raster_core` as `raster::core`, exports `#[tile]` / `#[sequence]`, conditionally exports runtime types)
- **Core types**
  - `crates/raster-core/src/lib.rs` (module list + feature gating + `no_std` intent)

### A.2 Macro-generated contracts (tile ABI + registries)

- **Tile macro and ABI wrapper generation**
  - `crates/raster-macros/src/lib.rs` (`#[tile]`: generates `__raster_tile_entry_<name>(input: &[u8]) -> Result<Vec<u8>>`)
- **Sequence macro (registration only; not full control-flow)**
  - `crates/raster-macros/src/lib.rs` (`#[sequence]`: registers ordered tile *names* extracted from AST)
- **Registries**
  - `crates/raster-core/src/registry.rs` (link-time distributed slices via `linkme`; not available on `riscv32`)
- **Tile identity/metadata types**
  - `crates/raster-core/src/tile.rs` (`TileId`, `TileMetadata`, static variants)

### A.3 CFS (control-flow schema) model and generation

- **CFS data model**
  - `crates/raster-core/src/cfs.rs` (`ControlFlowSchema`, `TileDef`, `SequenceDef`, `SequenceItem`, `InputBinding`, `InputSource`)
- **CFS construction from source scanning**
  - `crates/raster-compiler/src/cfs_builder.rs`
  - `crates/raster-compiler/src/ast.rs` (AST parse + call extraction)
  - `crates/raster-compiler/src/tile.rs` and `crates/raster-compiler/src/sequence.rs` (tile/sequence discovery views over the AST)
  - `crates/raster-compiler/src/flow_resolver.rs` (dataflow binding resolution)
- **CLI entrypoint that emits JSON**
  - `crates/raster-cli/src/commands.rs` (`cfs` command: `serde_json::to_string_pretty`)

### A.4 Artifacts and proofs (backend-level, not in `raster-core`)

- **Backend abstraction**
  - `crates/raster-backend/src/backend.rs` (`Backend`, `CompilationOutput`, `TileExecution`, `ExecutionMode`)
- **RISC0 backend**
  - `crates/raster-backend-risc0/src/risc0.rs` (ELF build, method/image ID computation, receipt/proof creation + verification)
- **Artifact layout and caching**
  - `crates/raster-compiler/src/builder.rs` (writes `guest.elf`, `method_id`, `manifest.json`; maintains a simple source-hash cache)

### A.5 Traces (event model exists; tile I/O tracing + commitments exist)

- **Trace data model**
  - `crates/raster-core/src/trace.rs`
- **Runtime tracing plumbing (tile I/O tracing)**
  - `crates/raster-runtime/src/tracing.rs` (init/finish + global subscriber)
  - `crates/raster-runtime/src/tracing/subscriber.rs` (subscriber trait + globals)
  - `crates/raster-runtime/src/tracing/subscriber/{json,commit,audit}.rs`
    - `JsonSubscriber` (writes JSON `TraceItem`s to a writer)
    - `CommitSubscriber` (writes packed commitment blocks to a writer)
    - `AuditSubscriber` (compares packed commitment blocks against an expected file and reports the first mismatch)

### A.6 Existing docs

- `README.md` (workspace crate breakdown + stated design principles)
- `docs/architecture.md` (artifact layout, backend overview, registry overview)
- `PROGRAM_STRUCTURE.md` (broader intended semantics; exceeds what current code enforces)

---

## B. What “Raster Core” is (as implemented)

### B.1 Component boundary

“Raster Core” in this repo refers primarily to the `raster-core` crate, plus the conventions imposed by:

- `raster` (user-facing re-exports)
- `raster-macros` (what `#[tile]` / `#[sequence]` generate)
- `raster-compiler` (how schemas/artifacts are constructed)
- `raster-backend*` (how artifacts and proofs are produced/verified)

`raster-core` itself is intentionally small: it defines **data structures**, **serialization formats**, and **error types** that other crates share.

### B.2 Feature/target constraints

Raster Core code and generated code is not uniformly available on all targets:

- `raster-core` is `no_std` at the crate level (`#![no_std]`) and uses `alloc`.
- `raster-core` modules `cfs`, `manifest`, `schema`, and `trace` are **only available when the `std` feature is enabled**.
- `raster-core::registry` is only available when `std` is enabled **and** the target is **not** `riscv32` (because the current registry mechanism uses `linkme`).

These constraints matter because zkVM guests commonly target `riscv32`, so host-only features (registry + JSON-bearing types) may be unavailable inside guests.

---

## C. Inputs, outputs, and guarantees

This section describes what the Raster toolchain consumes and produces today, and what is guaranteed to be stable for downstream tooling.

### C.1 Identities

- **Tile identifiers**
  - A tile is identified by `raster_core::tile::TileId`, which is currently a wrapper around a `String`.
  - In source-based tooling (compiler/CLI), the tile ID is the **Rust function name** discovered in `src/**/*.rs`.

- **Sequence identifiers**
  - Sequence IDs are currently the **Rust function name** of `#[sequence]`-annotated functions discovered in `src/**/*.rs`.

Guarantees:

- Tile and sequence IDs **MUST** be treated as **case-sensitive UTF-8 strings**.
- Tools **MUST NOT** assume any hashing, namespacing, or path qualification beyond what is present in these strings today.

### C.2 Tile ABI (byte-level contract)

Tiles are executed via a generated ABI wrapper function with signature equivalent to:

- `fn __raster_tile_entry_<tile_name>(input: &[u8]) -> raster_core::Result<Vec<u8>>`

Serialization guarantees (current implementation):

- The tile ABI **MUST** interpret `input` bytes as a value encoded with **`postcard`**.
- The tile ABI **MUST** produce output bytes encoded with **`postcard`**.
- When a tile has:
  - **0 arguments**: `input` **MUST** be `postcard` encoding of `()`.
  - **1 argument**: `input` **MUST** be `postcard` encoding of that argument value.
  - **N > 1 arguments**: `input` **MUST** be `postcard` encoding of an \(N\)-tuple `(a0, a1, ..., a{N-1})` in source argument order.
- If input decoding fails, the wrapper **MUST** return `raster_core::Error::Serialization`.
- If output encoding fails, the wrapper **MUST** return `raster_core::Error::Serialization`.

Notes:

- Tiles **MAY** return a plain value or a `Result<T, E>`. If the macro detects the return type as “a `Result`”, the generated wrapper uses `?` to propagate the error, which in practice means the tile’s error type must be `raster_core::Error` (or otherwise be convertible in a way Rust accepts for `?` at that callsite).

### C.3 Control Flow Schema (CFS)

Raster currently has a **project-level CFS JSON** model defined by `raster-core` and emitted by `raster-cli`.

#### C.3.1 CFS structure and format

- A CFS **MUST** serialize as JSON via `serde` using the `raster_core::cfs::ControlFlowSchema` structure.
- The top-level object **MUST** contain:
  - `version` (string; currently `"1.0"` in code)
  - `project` (string; derived from `Cargo.toml` name or directory fallback)
  - `encoding` (string; currently `"postcard"`)
  - `tiles` (array of `TileDef`)
  - `sequences` (array of `SequenceDef`)

Tile definitions:

- Each `TileDef` **MUST** contain:
  - `id` (string)
  - `type` (string; serialized field name is `"type"`)
  - `inputs` (integer)
  - `outputs` (integer)

Sequence definitions:

- Each `SequenceDef` **MUST** contain:
  - `id` (string)
  - `input_sources` (array of `InputBinding`)
  - `items` (array of `SequenceItem`)

Input bindings:

- Each `InputBinding` **MUST** be an object containing `source`.
- `source` is an `InputSource` enum tagged with a `type` field and one of:
  - `{ "type": "external" }`
  - `{ "type": "seq_input", "input_index": <usize> }`
  - `{ "type": "item_output", "item_index": <usize>, "output_index": <usize> }`

#### C.3.2 CFS generation rules (current compiler behavior)

When `raster-compiler` builds a CFS from source:

- It **MUST** scan `src/**/*.rs` for `#[tile(...)]` and `#[sequence(...)]` annotations.
- It **MUST** infer tile arity using syntactic parsing of the function signature line (best-effort).
- It **MUST** infer sequence calls by scanning sequence function bodies for simple:
  - `let x = foo(a, b);`
  - `foo(a, b);`
  - and it ignores method calls and path-qualified calls.
- For dataflow between calls, it **MUST**:
  - map a sequence parameter name to `seq_input(input_index)`
  - map a previously bound variable name to `item_output(item_index, 0)` (single-output assumption)
  - otherwise fall back to `external`

#### C.3.3 Example CFS JSON

Example for a simple “pipeline” sequence:

```json
{
  "version": "1.0",
  "project": "example-project",
  "encoding": "postcard",
  "tiles": [
    { "id": "greet", "type": "iter", "inputs": 1, "outputs": 1 },
    { "id": "exclaim", "type": "iter", "inputs": 1, "outputs": 1 }
  ],
  "sequences": [
    {
      "id": "main",
      "input_sources": [{ "source": { "type": "external" } }],
      "items": [
        {
          "item_type": "tile",
          "item_id": "greet",
          "input_sources": [{ "source": { "type": "seq_input", "input_index": 0 } }]
        },
        {
          "item_type": "tile",
          "item_id": "exclaim",
          "input_sources": [
            { "source": { "type": "item_output", "item_index": 0, "output_index": 0 } }
          ]
        }
      ]
    }
  ]
}
```

### C.4 Tile compilation artifacts

Compilation artifacts are produced by the selected backend (`raster-backend`) and written by `raster-compiler` to a stable directory layout under `target/raster/`.

For a given tile ID and backend name, the artifact directory is:

- `target/raster/tiles/<tile_id>/<backend>/`

The build process currently produces:

- `guest.elf` (bytes): the compiled guest program, when the backend produces one
- `method_id` (text): hex-encoded method/image identifier for the guest program
- `manifest.json` (JSON): build metadata used for caching and inspection

### C.5 Proofs (receipts)

Proofs are backend-defined. In the current RISC0 backend:

- The “proof” output is a serialized RISC0 `Receipt`.
- The receipt bytes returned by the backend are **opaque** to `raster-core`.
- The RISC0 backend currently serializes receipts using **`postcard`**.

Consumers:

- Any tool that stores or transmits receipts **MUST** treat them as backend-scoped opaque bytes unless it also adopts the backend’s receipt serialization format.

### C.6 Traces

Raster Core defines a trace event model:

- `raster_core::trace::Trace` contains `run_id`, `timestamp`, and an ordered list of `TraceEvent`.
- `TraceEvent` currently records coarse-grained “start/end” events for tiles and sequences (IDs + timestamps + optional cycle count).

Current implementation status:

- The runtime tracer accumulates events in memory and can emit a `Trace` object, but file output and sequence execution are not fully implemented yet.

---

## D. Relationship to the broader Core specs

The broader Core spec suite (e.g., `PROGRAM_STRUCTURE.md` and specs under `specs/Core/`) describes an intended end-state where:

- program execution is mechanically checkable against a schema,
- traces commit to the information needed to verify “what must run next”,
- backends can produce proofs tied to artifact identities,
- and verifiers can reject invalid executions.

This repository’s current implementation provides **building blocks** toward that end-state:

- a concrete tile ABI (`postcard` bytes),
- a concrete JSON CFS model and a source-based generator,
- a concrete artifact layout for zkVM backends (RISC0),
- and a preliminary trace data model.

However, the end-to-end execution + verification loop described in the broader specs is not fully implemented.

---

## E. Known gaps / divergences (code vs desired behavior)

This section documents mismatches that downstream implementers must account for.

### E.1 Tile ABI encoding name drift

- Some comments/documentation in the repo refer to “bincode” for tile input/output encoding, but the `#[tile]` macro wrapper currently uses **`postcard`** for both input decoding and output encoding.
- The CFS `encoding` field is currently hard-coded to `"postcard"`.

### E.2 CFS does not yet bind execution to artifact identities

The broader specs require schemas to bind tile IDs to artifact identities (e.g., method/image IDs) for fraud detection. The current `raster_core::cfs::TileDef` contains only:

- `id`, `type`, `inputs`, `outputs`

No artifact identity is present in the CFS today.

### E.3 External inputs are not scoped to the entry sequence

The broader specs restrict “external inputs” to the entry sequence. The current CFS builder:

- sets `SequenceDef.input_sources` to `external` for **all** sequence parameters,
- and the flow resolver falls back to `external` when it cannot resolve an argument (including literals and complex expressions).

Downstream consumers **MUST** treat `external` bindings as “unknown provenance” in today’s schemas; they are not yet a reliable indicator of entry-point-only inputs.

### E.4 Multi-output tiles and tuple bindings are not modeled

The source-based flow resolver currently assumes a single output when recording bindings:

- `result_binding` is always mapped to `(item_index, 0)`.

Schemas may therefore be incorrect for tiles that return tuples, and verifiers/runners should not rely on `output_index` semantics beyond the single-output case today.

### E.5 Recursive tiles are not fully represented in the CFS

Source discovery can detect a `foo!(...)` call and records an `is_recursive` flag internally, and tiles may be declared with `#[tile(kind = recur)]`. However:

- the current exported `ControlFlowSchema` does not encode per-call recursion,
- and `TileDef.type` is only a free-form string.

### E.6 Runtime execution/tracing is incomplete

`raster-runtime` currently contains TODOs for:

- executing sequences according to schemas,
- recording real timestamps,
- and writing trace files.

---

## F. Practical guidance for implementers

- **Schema consumers**: treat the CFS JSON as a “best-effort static hint” today. It is useful for listing tiles, listing call order, and approximating simple dataflow, but it is not yet sufficient for fraud-proof verification.
- **Artifact managers**: rely on the `target/raster/tiles/<tile_id>/<backend>/` layout and the presence of `method_id` as the stable binding for RISC0 artifacts.
- **Backend integrators**: treat `CompilationOutput.method_id` as the backend’s artifact identity; if you need schema-to-proof binding, you will need to extend the schema model beyond what is currently present in `raster-core`.