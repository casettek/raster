# Raster Architecture

## Overview

Raster is a Rust workspace for defining, compiling, executing, and auditing tile-based programs.
The current implementation is split across crates that handle:

- Authoring surface (`#[tile]`, `#[sequence]`, `#[raster::main]`)
- Source discovery and compilation orchestration
- Backend execution (native and RISC0)
- Runtime trace emission and trace-commitment auditing

The toolchain supports both tile-level execution (`run-tile`) and whole-program native execution (`run`).

## Workspace Components

### Core crates

- `raster-core`: shared types (`TileId`, metadata, CFS, trace types), error model, and host registry helpers.
- `raster-macros`: procedural macros that generate tile wrappers/registration and runtime hooks.
- `raster`: user-facing re-export crate and prelude.

### Build and execution crates

- `raster-compiler`: AST-based tile/sequence discovery, CFS generation, artifact orchestration.
- `raster-backend`: backend trait and execution result contracts.
- `raster-backend-native`: native backend implementation (currently placeholder for tile execution).
- `raster-backend-risc0`: RISC0 guest build, execute/prove/verify implementation.

### Runtime, proving, and tooling crates

- `raster-runtime`: std-only runtime subscriber plumbing (`JsonSubscriber`, `CommitSubscriber`, `AuditSubscriber`).
- `raster-prover`: trace-item commitment primitives (incremental Merkle roots, replay helpers).
- `raster-analysis`: trace metrics and reporting helpers.
- `raster-cli`: `cargo raster` commands.

## Authoring and Compilation Model

### Tiles

Tiles are Rust free functions annotated with `#[tile]`.
The macro generates:

- A wrapper `__raster_tile_entry_<name>(input: &[u8]) -> raster_core::Result<Vec<u8>>`
- Host registration statics on supported targets (`std` and non-`riscv32`)
- Runtime trace emission hook wiring

Tile ABI serialization uses `postcard`:

- Input: `()` for 0 args, value for 1 arg, tuple for N>1 args
- Output: encoded return value (or unwrapped `Ok(T)` for `Result<T, _>`)

### Sequences

`#[sequence]` currently provides registration and static call extraction support.
It is a tooling annotation, not a runtime executor.

Current limitations:

- No schema-driven sequence runner
- No full control-flow modeling for conditionals/branches
- No end-to-end sequence proving pipeline

### Discovery and CFS

The compiler currently discovers tiles and sequences by parsing source AST (`syn`) rather than reading macro-expanded artifacts.
CFS is emitted as JSON and uses `"postcard"` as encoding metadata.

## Execution Surfaces

### Tile execution (`cargo raster run-tile`)

`run-tile` compiles/loads tile artifacts, encodes input bytes, and executes through a selected backend:

- `ExecutionMode::Estimate`
- `ExecutionMode::Prove { verify: false }`
- `ExecutionMode::Prove { verify: true }`

For RISC0, outputs include journal bytes, optional receipt bytes, cycles, and optional local verification status.

### Whole-program execution (`cargo raster run`)

`run` currently supports native backend only and executes the built project binary as a subprocess.
`#[raster::main]` configures runtime behavior:

- default: JSON `TraceItem` stream to stdout
- `--commit <path>`: write packed commitment stream
- `--audit <path>`: compare against expected commitment stream and report divergence

## RISC0 Backend Details

### Artifact layout

RISC0 artifacts are written under:

```text
target/raster/tiles/<tile_id>/risc0/
  guest.elf
  method_id
  manifest.json
```

### Host/guest I/O

Host writes:

1. `u32` input length
2. raw tile input bytes

Guest reads the framed bytes, calls the generated tile entry wrapper, and commits output bytes to the journal.

### Identity and receipts

- Image/method ID is derived from ELF via `risc0_zkvm::compute_image_id`.
- Receipt bytes are serialized/deserialized with `postcard`.
- Verification recomputes image ID from ELF and checks `receipt.verify(image_id)`.

## Trace and Commitment Architecture

Raster currently has two trace-related layers:

- `Trace` / `TraceEvent` model in `raster-core` (coarse event model)
- `TraceItem` stream with commitment/audit tooling (implemented and used by runtime subscribers)

`raster-prover` hashes each `TraceItem` as `SHA-256(postcard(TraceItem))` and builds incremental commitment roots using a bridge tree.
`CommitSubscriber` writes packed commitments; `AuditSubscriber` replays and finds first mismatch.

## Dependency View

```text
raster-core
  ├─ raster-macros
  ├─ raster
  ├─ raster-compiler
  ├─ raster-backend
  │   ├─ raster-backend-native
  │   └─ raster-backend-risc0
  ├─ raster-runtime
  ├─ raster-prover
  ├─ raster-analysis
  └─ raster-cli
```

## Extension Points

### Backends

Implement `raster-backend::Backend` in a new crate and plug it through CLI/backend selection.

### Runtime subscribers

Implement `raster_runtime::Subscriber` for custom sink/processing behavior.

### Compiler/tooling integration

Build on `raster-compiler` and CFS output for custom artifact pipelines, analysis, or validation tools.
