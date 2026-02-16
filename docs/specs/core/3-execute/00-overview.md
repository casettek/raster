## Purpose and scope

This section defines the **Execute** stage of Raster: how a previously-authored and compiled tile (or a collection of tiles) is executed, what artifacts are consumed, and what outputs are produced for downstream analysis and verification.

This document describes what the codebase does today and highlights known gaps where the implementation does not yet match the intended architecture.

## Code audit tasks (where to look)

This file is meant to help implementers navigate the relevant entrypoints and data shapes.

- **Runner entrypoints**
  - CLI entrypoint: `crates/raster-cli/src/main.rs`
  - CLI command implementations: `crates/raster-cli/src/commands.rs`
- **Runtime tracing / commitments (used by whole-program runs)**
  - `crates/raster-runtime/src/tracing.rs` and `crates/raster-runtime/src/tracing/subscriber/*`
- **Backend execution API**
  - Backend trait + execution types: `crates/raster-backend/src/backend.rs`
  - Native backend implementation: `crates/raster-backend/src/native.rs`
  - RISC0 backend implementation: `crates/raster-backend-risc0/src/risc0.rs`
- **Artifact loading and cache layout**
  - Build orchestration + artifact layout + caching: `crates/raster-compiler/src/builder.rs`
  - RISC0 guest artifact writer: `crates/raster-backend-risc0/src/guest_builder.rs`
- **Tile ABI boundary (input/output encoding)**
  - Tile wrapper generation: `crates/raster-macros/src/lib.rs` (`#[tile]`)
  - Guest program generation (RISC0): `crates/raster-backend-risc0/src/guest_builder.rs`
- **Trace types (shape of “trace”)**
  - Trace structs and events: `crates/raster-core/src/trace.rs`
  - Tile I/O trace items + commitment subscribers: `crates/raster-runtime/src/tracing/subscriber/*`

## Execution entrypoints (as implemented today)

Raster currently has **two practical execution entrypoints**:

- **RISC0 execution via CLI** (works end-to-end for single tiles)
  - Command: `cargo raster run-tile --backend risc0 --tile <tile_id> [--input <json>] [--prove] [--verify]`
  - Implementation: `crates/raster-cli/src/commands/tile/run_tile.rs` calls:
    - `raster_compiler::Builder` to compile the tile (using cache when possible)
    - `raster_backend_risc0::Risc0Backend` to execute the resulting ELF
  - Output surface: prints cycles/proof-cycles/receipt bytes to stdout; does not currently persist a trace file.

- **Native whole-program execution** (runs the project binary as a subprocess)
  - Command: `cargo raster run --backend native [--input <json>] [--commit <path> | --audit <path>]`
  - Implementation: `crates/raster-cli/src/commands/run.rs` builds the project, then executes the built binary with `--input/--commit/--audit` forwarded.
  - Tracing/commitment behavior is controlled by the program’s `#[raster::main]` macro expansion, which initializes `raster-runtime` subscribers based on `--commit/--audit`.

### Important gap: schema-driven sequence runner is not implemented

Although `raster-core` defines `SequenceSchema`/`ControlFlow`, there is no runtime component in this workspace that interprets those schemas to execute sequences end-to-end. The CLI currently supports:

- tile-level execution (`run-tile`) and
- a separate “whole program” runner (`run`) that delegates execution to the user binary.

## Artifact inputs to execution

### Tile compilation output consumed by execution

Backends execute tiles using `raster_backend::CompilationOutput`:

- `elf: Vec<u8>`: the guest program bytes for zkVM backends.
- `method_id: Vec<u8>`: the “image id” / method identifier for the compiled program (see below).
- `artifact_dir: Option<PathBuf>`: where artifacts were written, if applicable.

### Artifact directory layout (current)

The compiler and the RISC0 guest builder both write artifacts under:

- `target/raster/tiles/<tile_id>/<backend>/`

For the RISC0 backend, the following files are produced:

- `guest.elf`: the compiled guest program.
- `method_id`: hex encoding of the computed image id.
- `manifest.json`: JSON with `tile_id`, `method_id`, and `elf_size`.

The CLI `run` path currently loads:

- `guest.elf` bytes (if present)
- `method_id` as hex (decoded into bytes)

and constructs a `CompilationOutput` in-memory before calling `Backend::execute_tile`.

### Method ID meaning (current)

For the RISC0 backend:

- The method id is computed as `risc0_zkvm::compute_image_id(&elf)` at compile time.
- During verification, the backend recomputes the image id from `compilation.elf` and verifies the receipt against that value.

For the native backend:

- `compile_tile` sets `method_id = metadata.id.0.as_bytes().to_vec()` as a placeholder and produces no ELF.

## Inputs: bytes, and how they are formed

### Tile ABI wrapper encoding (required for zkVM execution)

The `#[tile]` macro generates a public wrapper function:

- Name: `__raster_tile_entry_<fn_name>`
- Signature: `fn(&[u8]) -> raster_core::Result<Vec<u8>>`
- Encoding: uses `postcard` to deserialize inputs and serialize outputs.

Input decoding rules (as implemented):

- If the tile function has **0 parameters**, the wrapper does not read input bytes.
- If the tile function has **1 parameter**, the wrapper deserializes that parameter from the entire input byte slice.
- If the tile function has **N>1 parameters**, the wrapper deserializes a tuple `(T1, ..., TN)` from the input byte slice.

Output encoding rule (as implemented):

- The wrapper serializes the tile’s return value with `postcard` into a `Vec<u8>`.

Error behavior (as implemented):

- If deserialization fails, the wrapper returns `Error::Serialization("Failed to deserialize input: ...")`.
- If serialization fails, the wrapper returns `Error::Serialization("Failed to serialize output: ...")`.

### CLI input encoding (current convenience behavior)

The CLI accepts `--input <json>` and currently does:

- Parse the JSON string into `serde_json::Value`
- Serialize that value with `postcard`
- Provide the resulting bytes as the tile input

If `--input` is omitted, the CLI serializes `()` via `postcard` and uses that as the tile input.

Important implication:

- The bytes passed into the tile ABI wrapper must match the wrapper’s expected `postcard` encoding for the tile’s Rust parameter type(s). A JSON value serialized with postcard is only compatible when the tile expects `serde_json::Value` (or a compatible type).
- The CLI does not currently provide typed input authoring beyond this JSON-to-`serde_json::Value` path.

## Execution outputs (result + trace)

Execution produces **a serialized output** and may also produce **auxiliary execution data** depending on backend and mode.

### Tile execution output (backend API)

`raster_backend::Backend::execute_tile` returns `raster_backend::TileExecution` with:

- `output: Vec<u8>`
  - For RISC0: the zkVM journal bytes emitted by the guest (see below).
  - For Native: currently an empty placeholder output (native runner is not implemented yet).
- `cycles: Option<u64>`
  - For RISC0 estimate: populated from the session cycle count.
  - For RISC0 prove: populated from prover stats.
  - For Native: a simulated constant when enabled (placeholder).
- `proof_cycles: Option<u64>`
  - Computed as `max(2^16, next_power_of_two(cycles))` when cycles are known.
  - This is intended to represent the padded cycle count that drives proving cost.
- `receipt: Option<Vec<u8>>`
  - Present only in `Prove` mode for zkVM backends.
  - For RISC0: `postcard` serialization of `risc0_zkvm::Receipt`.
- `verified: Option<bool>`
  - Present only when a receipt is generated.
  - For RISC0: indicates whether the backend performed local verification when asked.

#### RISC0 journal semantics (current)

The generated guest program commits the ABI wrapper output to the journal:

- Guest reads input length (`u32`) then that many bytes from the environment.
- Guest calls `__raster_tile_entry_<fn_name>(&input_bytes)`.
- Guest commits the returned output bytes via `risc0_zkvm::guest::env::commit_slice(&output)`.

Therefore, `TileExecution.output` for the RISC0 backend is exactly the journal bytes committed by the guest, and it is expected to be a `postcard`-encoded value matching the tile’s return type.

### Trace output (runtime API)

Raster defines a trace type in `raster-core`:

- `raster_core::trace::Trace { run_id, timestamp, events: Vec<TraceEvent> }`

and a runtime tracer abstraction:

Raster also defines a tile I/O “trace item” record used by runtime subscribers:

- `raster_core::trace::TraceItem` (includes `fn_name`, base64 input/output bytes, and signature metadata)

#### Implemented today (tile I/O trace + commitments)

When a program uses the `#[raster::main]` macro:

- By default, it initializes a `JsonSubscriber` that writes serialized `TraceItem`s to stdout.
- With `--commit <path>`, it initializes a `CommitSubscriber` that writes packed commitment blocks to the given file.
- With `--audit <path>`, it initializes an `AuditSubscriber` that recomputes packed commitment blocks and compares them against the given file, panicking on the first mismatch.

#### Important gaps (trace)

- The stdout JSON subscriber currently writes a stream of JSON objects without an explicit delimiter (newline/prefix), so downstream tools must not assume line-delimited JSON.
- The CLI `cargo raster run` trace pretty-printing logic currently expects a `RASTER_TRACE:` line prefix; this does not match the current subscriber output format (gap/mismatch).

## Execution modes and their guarantees

Backends are invoked in one of two modes:

- **Estimate mode**
  - Backends MUST return `output`.
  - Backends SHOULD return `cycles` when available.
  - `receipt` MUST be `None`.
- **Prove mode**
  - zkVM backends MUST return `output` and `receipt`.
  - zkVM backends SHOULD return `cycles` when available.
  - If “verify” is requested, the backend SHOULD verify the receipt against the compiled program identity and return `verified = Some(true)` on success, `Some(false)` on failure.
  - Native backend MUST reject prove mode (current behavior).

Note: the native backend currently returns placeholder outputs and does not execute the tile function; this is an implementation gap, not a guarantee.

## How execution relates to verification (as implemented today)

### What a “proof” attests to

In the current RISC0 backend implementation:

- A receipt is verified against the image id computed from the executed ELF.
- The receipt’s journal bytes are treated as the tile output.

This provides a verifiable statement of the form:

- “The program with image id \(I\) produced journal bytes \(J\) for some execution.”

### Important gap: proofs are not bound to the caller-supplied input

The guest program currently **does not commit the input bytes (or a digest of them) to the journal**. The input is read privately from the environment and only the output is committed.

As a consequence:

- A verifier can validate that the program ran and produced `output` bytes, but cannot validate which input bytes were used unless the input (or an input commitment) is also included in the journal or otherwise bound by convention outside the receipt.

If the intended protocol requires “output for a specific input,” then the guest program MUST also commit either:

- the full input bytes, or
- a cryptographic digest of the input bytes, or
- a higher-level commitment derived from the input according to the canonical encoding rules.

That behavior is not present today.

### Method ID vs. receipt verification

Although artifacts store a `method_id` file, receipt verification currently recomputes the image id from `compilation.elf`. Implementations that wish to avoid ELF dependence during verification MAY rely on the stored method id, but this is not currently wired into the backend.

## Examples

### Example: running a tile in estimate mode (RISC0)

```bash
cargo raster run-tile --backend risc0 --tile double
```

Expected behavior:

- The tile is compiled (or loaded from cache under `target/raster/tiles/...`).
- The guest is executed in estimate mode.
- The CLI prints compute cycles and proof-cycles estimate.

### Example: running a tile with proving and local verification (RISC0)

```bash
cargo raster run-tile --backend risc0 --tile double --prove --verify
```

Expected behavior:

- The CLI requests `ExecutionMode::Prove { verify: true }`.
- The backend returns `receipt` bytes (`postcard`-serialized `risc0_zkvm::Receipt`) and `verified = Some(true|false)`.

### Example: artifact directory (RISC0)

After a successful build/run, the tile artifacts are expected at:

- `target/raster/tiles/<tile_id>/risc0/guest.elf`
- `target/raster/tiles/<tile_id>/risc0/method_id`
- `target/raster/tiles/<tile_id>/risc0/manifest.json`

