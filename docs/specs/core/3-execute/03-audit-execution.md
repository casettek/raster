## Audit Execution

This document specifies what Raster checks while executing in an “audit” posture: i.e., execution that is intended to *reject* results that are inconsistent with the expected program structure and artifacts.

It is written to match the current codebase. Where the intended behavior (CFS enforcement, strict binding validation, artifact identity enforcement) is not yet implemented, this document calls that out explicitly and describes the required behavior.

### Status in the current codebase

- There is **no schema-driven “audit execution” runner** today (no runtime component enforces a CFS while executing a sequence/program).
- There *is* an implemented **trace-commitment audit posture** for whole-program native runs:
  - `#[raster::main]` supports `--commit <path>` and `--audit <path>`.
  - `--commit` writes a packed commitment stream for the tile I/O trace (`TraceItem`s).
  - `--audit` recomputes the packed stream and compares it to an expected file, reporting the first mismatch.
- For zkVM execution, the RISC0 backend also supports **receipt verification** (`ExecutionMode::Prove { verify: true }`), but that is distinct from trace-commitment auditing.

---

## Code audit tasks (exact places to look)

### Execution entry points and “modes”

- **CLI mode selection and what it actually runs**
  - `crates/raster-cli/src/commands.rs`
    - Tile execution lives under `crates/raster-cli/src/commands/tile/*` and maps `--prove`/`--verify` to `ExecutionMode` for RISC0.
    - `preview(...)`: discovers a sequence and executes its tiles sequentially in `ExecutionMode::Estimate` (no proving, no verification, no trace persistence).
- **Execution mode surface**
  - `crates/raster-backend/src/backend.rs`
    - `ExecutionMode::{Estimate, Prove{verify}}`
    - `TileExecution { output, cycles, proof_cycles, receipt, verified }`

### CFS generation and binding model (used for documentation, not enforced at runtime)

- **CFS data model**
  - `crates/raster-core/src/cfs.rs`: `ControlFlowSchema`, `TileDef`, `SequenceDef`, `SequenceItem`, `InputBinding`, `InputSource`
- **CFS construction**
  - `crates/raster-compiler/src/cfs_builder.rs`: `CfsBuilder::build`
- **Sequence call parsing and data-flow resolution**
  - `crates/raster-compiler/src/ast.rs`
    - `CallVisitor` extracts `CallInfo { callee, arguments, result_binding }` from parsed Rust syntax trees
    - Note: macro invocations like `callee!(...)` are not extracted as calls today
  - `crates/raster-compiler/src/sequence.rs`
    - `SequenceDiscovery` resolves call infos into tile/sequence steps
  - `crates/raster-compiler/src/flow_resolver.rs`
    - `FlowResolver::resolve_argument`
    - **Current gap**: unresolved arguments fall back to `InputSource::External` instead of producing a validation error.
    - **Current gap**: output bindings assume `output_index = 0` unconditionally.

### Artifact identity and zkVM verification

- **Artifact writing (ELF + method_id + manifest)**
  - `crates/raster-backend-risc0/src/guest_builder.rs`
    - `write_artifacts(...)` writes:
      - `guest.elf`
      - `method_id` (hex; computed image ID bytes)
      - `manifest.json` including `tile_id`, `method_id`, `elf_size`
- **Method ID / image ID computation**
  - `crates/raster-backend-risc0/src/risc0.rs`
    - `compile_tile(...)`: computes `method_id = risc0_zkvm::compute_image_id(&elf)`
    - `execute_tile(..., Prove{verify:true})`: recomputes `image_id = compute_image_id(&compilation.elf)` and calls `receipt.verify(image_id)`
  - **Current gap**: `compilation.method_id` is not used to validate that the provided ELF matches the expected method ID.
- **Guest input ABI (host ↔ guest)**
  - Host side: `crates/raster-backend-risc0/src/risc0.rs` (`Risc0Backend::execute_tile`)
  - Guest side: `crates/raster-backend-risc0/src/guest_builder.rs` (`generate_guest_main`)

### Native backend behavior (relevant to “audit failures” as currently surfaced)

- `crates/raster-backend/src/native.rs`
  - `NativeBackend::execute_tile(...)` rejects proving modes and otherwise returns a placeholder estimate result (does not invoke the registry; TODO).

---

## Definitions

- **Audit execution**: an execution mode whose output is only accepted if the execution is consistent with:
  - the expected control/data flow (CFS), and
  - the expected compiled artifacts (ELFs and their identities), and
  - any requested proof verification.
- **Audit failure**: a condition that MUST cause audit execution to be rejected (see below). In a future implementation, audit failures should surface as structured errors; today they may only be observable as `verified = false` or as backend errors (gaps noted).
- **Artifact identity / method ID**: in the RISC0 backend, the **method ID** is the image ID computed from an ELF via `risc0_zkvm::compute_image_id(&elf)`.
- **Binding**: how each input to a tile/sequence call is sourced: external input, a sequence input, or a previous item’s output (`InputSource::{External, SeqInput, ItemOutput}`).

---

## Implemented contract today: what is verified during execution

### 1) zkVM receipt verification (RISC0 backend, prove+verify)

When executing a tile via the RISC0 backend with `ExecutionMode::Prove { verify: true }`, the backend:

- Executes the guest ELF to obtain a receipt and a journal (output bytes).
- Computes `image_id = risc0_zkvm::compute_image_id(&compilation.elf)`.
- Attempts to verify the receipt against that `image_id`.

**Current behavior (important)**:

- Receipt verification failure does **not** return an error.
- Instead, the result sets `TileExecution.verified = Some(false)` and still returns `Ok(TileExecution::proved(...))`.

This means “prove+verify” is informational today; it does not enforce an “audit must fail on invalid proof” contract.

### 1b) Trace-commitment auditing (native whole-program runs)

When a program is built using `#[raster::main]`, it can be run in one of three tracing postures:

- Default: stdout JSON subscriber (`raster_runtime::JsonSubscriber`) emits `TraceItem`s.
- `--commit <path>`: commitment subscriber (`raster_runtime::CommitSubscriber`) writes a packed commitment stream to `path`.
- `--audit <path>`: audit subscriber (`raster_runtime::AuditSubscriber`) recomputes the packed stream and compares it to `path`, panicking on the first mismatch and including the mismatching `TraceItem` in the panic message.

This is the closest implemented mechanism to “audit” in the sense of *rejecting* inconsistent executions, but it applies only to the trace-commitment stream and does not enforce CFS legality.

### 2) Host↔guest input ABI correctness (RISC0 backend)

For RISC0 tile execution, the host provides the guest with:

1. `input_len: u32` (the length of the raw input in bytes)
2. `input: [u8; input_len]` (exactly that many bytes)

and the guest reads exactly those bytes, calls the tile ABI wrapper, and commits the wrapper output bytes to the journal.

If the guest panics (e.g., due to wrapper `expect("Tile execution failed")`), the backend surfaces an execution/proving error from the underlying RISC0 machinery.

### 3) What is *not* verified today (gaps vs audit intent)

The following checks are not currently performed during execution:

- **CFS enforcement**: no runtime component checks that the executed tile/sequence steps match a CFS, or that a given “next step” is permitted.
- **Binding checks**:
  - no runtime validation that each input binding resolves to a valid source;
  - no runtime validation that binding indices are in-range;
  - no runtime validation that the number of inputs for a call matches the callee’s arity.
- **Artifact identity checks**:
  - execution does not verify that `compilation.method_id` matches `compute_image_id(compilation.elf)`;
  - execution does not verify that a loaded artifact directory’s `method_id` file matches its `guest.elf`.

---

## Audit execution contract (target behavior) and current gaps

This section specifies the required behavior of audit execution, and notes where the current implementation does not yet satisfy it.

### A) CFS enforcement

Audit execution MUST be driven by an authoritative control/data-flow description (CFS or an equivalent internal representation).

Given a `ControlFlowSchema` and a chosen `SequenceDef`:

- The runner MUST execute items in `SequenceDef.items` order.
- Each `SequenceItem` MUST be treated as either:
  - a tile invocation (`item_type == "tile"`), or
  - a nested sequence invocation (`item_type == "sequence"`).
- If `item_type` is not recognized, audit execution MUST fail.

**Current gap**: `raster-runtime` does not execute sequences from CFS today; `cargo raster preview` runs a best-effort tile list discovered from source and does not enforce CFS semantics.

### B) Binding validation

For each `SequenceItem`, audit execution MUST validate each `InputBinding` before attempting execution:

- If the binding is `External`, the runner MUST require the caller to provide that input value at runtime.
- If the binding is `SeqInput { input_index }`:
  - `input_index` MUST be in-range for the current sequence’s declared inputs.
- If the binding is `ItemOutput { item_index, output_index }`:
  - `item_index` MUST refer to a prior item (`item_index < current_item_index`).
  - `output_index` MUST be in-range for the referenced item’s output arity.

If any of these conditions fail, audit execution MUST fail.

**Current gap**:

- `FlowResolver::resolve_argument` falls back to `External` when it cannot resolve an argument, which can silently turn an unbound variable or expression into an “external input”.
- The compiler path currently assumes a single output (`output_index = 0`) for all bindings.

### C) Artifact identity checks

When audit execution uses compiled artifacts, it MUST ensure the artifact identity matches the bytes actually executed.

For the RISC0 backend:

- The runner MUST compute `computed_method_id = compute_image_id(elf_bytes)`.
- If an expected `method_id` is available (e.g., from `CompilationOutput.method_id`, from an artifact `method_id` file, or from a manifest), it MUST compare it to `computed_method_id`.
- If they differ, audit execution MUST fail.

**Current gap**: `Risc0Backend` verifies receipts against the ELF’s computed image ID, but it does not compare `CompilationOutput.method_id` (or on-disk `method_id`) against the ELF.

### D) Proof verification behavior

If audit execution requests proof verification (e.g., “prove+verify”):

- Audit execution MUST treat any proof verification failure as an audit failure.
- Audit execution MUST return an error (not a “successful” execution result with a `verified=false` flag).

**Current gap**: verification failure currently produces `verified = false` in the returned `TileExecution`, and the CLI still prints “Execution complete!”.

---

## Examples

### Example: artifact manifest written by the RISC0 backend

The artifact writer emits a `manifest.json` of the form:

```json
{
  "tile_id": "my_tile",
  "method_id": "<hex image id bytes>",
  "elf_size": 123456
}
```

### Example: audit failure conditions (illustrative)

- **Invalid binding**: a `SequenceItem` refers to `ItemOutput { item_index: 5, output_index: 0 }` while executing item 3.
  - Audit execution fails because `item_index < current_item_index` is violated.
- **Artifact mismatch**: `method_id` file says `abcd...` but `compute_image_id(guest.elf)` yields `0123...`.
  - Audit execution fails due to artifact identity mismatch.
- **Invalid proof**: receipt verification fails for the ELF’s computed image ID.
  - Audit execution fails (today this is only observable as `verified=false`; gap).
