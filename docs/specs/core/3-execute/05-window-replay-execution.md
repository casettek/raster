## Window Replay Execution

This document specifies the contract for “window replay” execution: re-executing a contiguous portion (“window”) of a prior execution in a way that can be checked against recorded artifacts and (eventually) tied to trace commitments / fingerprints.

### Status in the current codebase

- **Window replay execution is not implemented yet.**
  - There is no code that selects a replay window, reconstructs the necessary inputs for that window, or executes only that window against a stored trace.
  - There is no trace commitment / fingerprint implementation in `raster-core` (though `raster-prover` implements a commitment stream for `TraceItem`).
- **Tile-level execution exists for the RISC0 backend**, including a concrete guest input/output ABI.
- **Trace data structures exist**, but tracing, persistence, and trace analysis are not complete.

This spec therefore contains:
- A **contract section** that exactly matches the behaviors implemented today (tile execution ABI, what outputs exist).
- A **replay contract** describing required inputs/outputs and their relationship to trace commitments/fingerprints, with **explicit gaps** where the code does not yet satisfy the contract.

---

## Code audit tasks (exact places to look)

### Replay logic (runner) and window selection

- **(Missing today)** Search for a window replay runner:
  - There is no schema-driven runtime runner in this workspace that implements replay or windowing.
  - `crates/raster-cli/src/commands.rs` provides a “sequence preview” loop (`preview`) that executes *all* tiles in a sequence, but it does not read a trace, does not select a window, and does not persist results.

### Trace structure and capture

- **Trace types**
  - `crates/raster-core/src/trace.rs`: `Trace` and `TraceEvent` definitions.
    - Events currently include: `TileStart`, `TileEnd`, `SequenceStart`, `SequenceEnd`.
    - Events currently do **not** include per-invocation input/output bytes, hashes, or stable invocation IDs.
 - **Trace-item commitments (implemented; not window replay)**
  - `crates/raster-core/src/trace.rs`: `TraceItem` (tile I/O transcript item)
  - `crates/raster-prover/src/trace.rs` and `crates/raster-prover/src/bit_packer.rs`
  - `crates/raster-runtime/src/tracing/subscriber/{commit,audit}.rs`

### Guest inputs provided to the zkVM (RISC0)

- **Host→guest input ABI**
  - `crates/raster-backend-risc0/src/risc0.rs`: `Risc0Backend::execute_tile`
    - Writes `input_len: u32` followed by the raw `input` bytes into the RISC0 `ExecutorEnv`.
- **Guest program main + guest input parsing**
  - `crates/raster-backend-risc0/src/guest_builder.rs`: `GuestBuilder::generate_guest_main`
    - Guest reads `input_len: u32` via `risc0_zkvm::guest::env::read()`,
    - allocates `input_len` bytes,
    - reads exactly that many bytes via `read_slice`,
    - calls the tile wrapper `__raster_tile_entry_<tile_id>` with the raw bytes,
    - commits the output bytes via `commit_slice` (journal).

### Native backend execution (for completeness)

- `crates/raster-backend/src/native.rs`: `NativeBackend::execute_tile` is currently a placeholder and does not execute via the registry (TODO).

---

## Definitions

- **Tile execution**: A single invocation of a tile (identified by a `TileId`) producing output bytes, and possibly a proof receipt depending on backend/mode.
- **Trace**: A sequence of `TraceEvent` values associated with a `run_id`.
- **Tile invocation**: A matched `TileStart` + `TileEnd` pair for the same `tile_id` in a `Trace`’s `events` list.
  - **Note**: The code does not currently enforce pairing invariants; consumers must defensively parse.
- **Window**: A contiguous subsequence of tile invocations from a larger execution, identified by indices into the invocation list.
  - **Note**: The code does not currently provide stable invocation IDs; this spec uses index-based selection as the only representation consistent with the current `Trace` type.

---

## Implemented contract today: tile execution ABI (RISC0 backend)

### Host-to-guest input format

When executing a tile in the RISC0 backend, the host MUST provide guest input as:

1. A `u32` input length (`input_len`), equal to the number of bytes in `input`.
2. Exactly `input_len` raw bytes (`input`).

The encoding of the `u32` length MUST match the encoding produced by `risc0_zkvm::ExecutorEnv::builder().write(&input_len)`, and the guest MUST decode it using `risc0_zkvm::guest::env::read::<u32>()`.

### Guest behavior and output

The guest program MUST:

- Read `input_len` then `input` bytes exactly as described above.
- Call the tile’s generated ABI wrapper function with `&input`.
- Commit the returned output bytes to the journal using `risc0_zkvm::guest::env::commit_slice`.

The host MUST treat the journal bytes as the tile’s output bytes.

### Execution modes and receipts

- In `ExecutionMode::Estimate`, the host MUST execute without proving and SHOULD return the observed cycle count.
- In `ExecutionMode::Prove { verify: false }`, the host MUST produce a receipt and MUST return the serialized receipt bytes.
- In `ExecutionMode::Prove { verify: true }`, the host MUST attempt receipt verification and MUST return whether verification succeeded.

**Current implementation detail** (for implementers):
- `crates/raster-backend-risc0/src/risc0.rs` serializes receipts using `postcard`.
- Proof “cycle count” reporting is taken from `prove_info.stats.total_cycles`.

---

## Window replay contract (target behavior) and current gaps

### Goal

Window replay is intended to allow an implementer to:

- Select a window of tile invocations from a previously recorded execution,
- Reconstruct the input bytes to the first invocation in that window,
- Re-execute exactly that window (and only that window),
- Produce outputs that can be checked against the recorded execution and/or against a trace fingerprint.

### Replay request (required inputs)

A window replay implementation MUST accept the following inputs:

- **Execution artifacts**
  - The compiled artifacts needed to execute tiles in the window, including (at minimum) each tile’s ELF bytes and method/image ID (backend-specific).
  - For RISC0: a `CompilationOutput { elf, method_id, ... }` per tile.
- **Backend and mode**
  - Backend choice (e.g., RISC0 vs native) and execution mode (`Estimate` vs `Prove{...}`).
- **Trace context**
  - A prior `Trace` OR a reference to a prior trace by identifier, together with enough data to validate replay outputs against the prior execution.
- **Window selector**
  - `start_invocation_index` (inclusive) and `end_invocation_index` (exclusive), indexing into the list of tile invocations derived from the trace.
  - The selected range MUST be non-empty and MUST be within bounds of the trace’s parsed invocation list.
- **Window initial input bytes**
  - The exact input bytes that were provided to the first tile invocation in the window.

#### Current gap: trace does not carry inputs/outputs

The existing `TraceEvent` does not include per-invocation input bytes (nor output bytes).

Because of that, a correct replay implementation cannot reconstruct `Window initial input bytes` from `Trace` alone today.

Implementers must currently obtain initial input bytes out-of-band.

### Replay execution behavior (required)

Given the replay request above, a correct implementation MUST:

- Parse the prior trace into a linear list of tile invocations (pairing `TileStart`/`TileEnd`).
  - If the trace contains malformed or unmatched events, the replay implementation MUST fail with a diagnostic error identifying the first inconsistency.
- Execute tile invocations in-order from `start_invocation_index` to `end_invocation_index - 1`.
- Feed bytes between invocations according to the execution model being replayed.
  - **Current codebase model (CLI preview)**: the output bytes of tile \(i\) become the input bytes of tile \(i+1\).
  - **Current gap**: the `raster-runtime` executor and the CFS-driven dataflow are not integrated into execution yet, so there is no authoritative multi-input/multi-output binding behavior at runtime.
- Produce:
  - The output bytes of the last tile invocation in the window.
  - Per-invocation execution metadata (cycles, receipt, verification result) when supported by the backend/mode.

### Replay outputs and verification (required)

A window replay implementation MUST provide:

- **Replay result**
  - `final_output: Vec<u8>` for the window.
  - A per-invocation record of at least: `tile_id`, `mode`, and the backend’s execution result (cycles and optional receipt).
- **Replay verification result**
  - A boolean indicating whether the replayed window matched the recorded execution for the same window.

#### Current gap: there is nothing to match against

Today, the recorded trace does not contain the expected outputs (or output hashes) per tile invocation. Therefore, “matched the recorded execution” cannot be checked from `Trace` alone.

---

## Relationship to trace commitments / fingerprints

Window replay is intended to support a higher-level contract:

- A prior execution produces a **trace fingerprint** that commits to the trace semantics.
- Any correct replay of a window should be able to produce evidence that is consistent with that fingerprint.

#### Current gap: no trace fingerprint exists in code

There is currently no `TraceFingerprint` type, no hash construction over trace contents, and no commitment verification code in `raster-core`.

Until a trace fingerprint exists, window replay cannot be formally bound to a commitment.

### Required fingerprint binding (target behavior)

Once implemented, a trace fingerprint scheme MUST define:

- **What is committed**
  - At minimum: the ordered list of tile invocations and each invocation’s input/output bytes (or cryptographic hashes of those bytes).
  - The identity of each tile artifact used (e.g., method/image ID for zkVM backends).
- **Determinism requirements**
  - Given the same tile artifacts and the same invocation input bytes, the replayed output bytes MUST be deterministic.
- **Window verification**
  - There MUST exist a way to verify that the replayed window’s per-invocation I/O is consistent with the global trace fingerprint (e.g., via a Merkle proof over per-invocation records).

---

## Examples

### Example: RISC0 tile guest I/O (bytes on the wire)

For an input payload `input = [0xAA, 0xBB, 0xCC]`:

- Host writes:
  - `input_len = 3u32` via `ExecutorEnv::write(&input_len)`
  - then writes the raw bytes `[0xAA, 0xBB, 0xCC]` via `write_slice`
- Guest reads:
  - `let input_len: u32 = env::read();`
  - `env::read_slice(&mut input_buf[..input_len])`
- Guest commits:
  - `env::commit_slice(&output_bytes)`

### Example: window selection over a trace (index-based)

Given a parsed invocation list:

- invocation 0: `tile_id = "greet"`
- invocation 1: `tile_id = "exclaim"`
- invocation 2: `tile_id = "hash"`

Selecting `start_invocation_index = 1`, `end_invocation_index = 3` replays `["exclaim", "hash"]` in-order.

---

## Required future work (explicit)

To make this spec fully realizable, the codebase needs, at minimum:

- **A real executor / runner capable of replay and windowing**
  - Likely in a future schema-driven runtime runner (not present in this workspace today).
- **Trace persistence**
  - `Trace`/`TraceEvent` persistence must be defined (currently there is no stable on-disk format for event traces).
- **Trace data needed for replay**
  - Per-invocation input bytes and output bytes (or hashes), and stable invocation identifiers.
  - Extension points belong in `crates/raster-core/src/trace.rs`.
- **A trace fingerprint / commitment scheme**
  - A `TraceFingerprint` definition plus hashing utilities in `raster-core`, and verification APIs.

