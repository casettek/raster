## Fraud Proof Construction

This document specifies how Raster constructs and verifies “fraud proof” artifacts **as implemented today**. It also documents the missing pieces required to support end-to-end program-level fraud proofs (window replay + invalid transition) and explicitly calls out where the implementation diverges from that target behavior.

At present, Raster **does not implement** a program-level fraud proof system. The only implemented proving primitive is **tile-level zkVM execution receipts** in the RISC0 backend.

---

## Code audit tasks (exactly where to look)

### zkVM guest logic (what runs inside the zkVM)

- `crates/raster-backend-risc0/src/guest_builder.rs`
  - `GuestBuilder::generate_guest_main`
    - Guest input framing (reads a `u32` length, then that many bytes)
    - Invocation of tile ABI wrapper `__raster_tile_entry_<tile_id>()`
    - Journal semantics (commits exactly one byte string: the tile output bytes)
    - Failure behavior: wrapper failure triggers a guest panic via `.expect("Tile execution failed")`

### zkVM host logic (proof generation + receipt verification)

- `crates/raster-backend-risc0/src/risc0.rs`
  - `Risc0Backend::compile_tile`
    - Builds guest ELF and computes method/image ID via `risc0_zkvm::compute_image_id(&elf)`
    - Writes artifacts (`guest.elf`, `method_id`, `manifest.json`) via `GuestBuilder::write_artifacts`
  - `Risc0Backend::execute_tile`
    - Host→guest input framing via `risc0_zkvm::ExecutorEnv::builder().write(u32).write_slice(bytes)`
    - Receipt generation (`default_prover().prove(...)`)
    - “Local verify” behavior: recomputes image ID from `compilation.elf` and runs `receipt.verify(image_id)`
    - Receipt bytes encoding: `postcard::to_allocvec(&risc0_zkvm::Receipt)`
  - `Risc0Backend::verify_receipt`
    - Receipt decoding: `postcard::from_bytes::<risc0_zkvm::Receipt>(receipt_bytes)`
    - Verification: `receipt.verify(image_id)`

### Tile ABI wrapper (how inputs/outputs are encoded and errors are produced)

- `crates/raster-macros/src/lib.rs`
  - `#[tile]` macro expansion
    - Wrapper naming convention: `pub fn __raster_tile_entry_<tile_fn>(input: &[u8]) -> raster::core::Result<Vec<u8>>`
    - Input decoding and output encoding: `postcard::{from_bytes,to_allocvec}`
    - Error mapping: (de)serialization errors become `raster_core::Error::Serialization(...)`

### “Fraud proof / window replay / invalid transition” (current implementation status)

- **There is no implemented module** for:
  - window replay proving
  - invalid transition proving
  - trace commitments and verifier bindings used for fraud proofs

If you are looking for these concepts in code today, the closest implemented building blocks are the tile-level receipt described above.

---

## Implemented artifact: tile receipt proof (RISC0 backend)

### Artifact shape

When executing a tile in `ExecutionMode::Prove { .. }`, Raster produces:

- **`receipt_bytes`**: a byte string equal to `postcard::to_allocvec(&risc0_zkvm::Receipt)`.
- **`output_bytes`**: the committed RISC0 journal bytes, extracted as `receipt.journal.bytes.clone()`.
- **`method_id` / image ID bytes**: computed from the executed ELF by `risc0_zkvm::compute_image_id(&elf)` and stored in:
  - `CompilationOutput.method_id` (raw bytes)
  - `target/.../tiles/<tile_id>/risc0/method_id` (hex string)

The on-disk artifact layout is produced by `GuestBuilder::write_artifacts`:

- `target/<output_dir>/tiles/<tile_id>/risc0/guest.elf`
- `target/<output_dir>/tiles/<tile_id>/risc0/method_id`
- `target/<output_dir>/tiles/<tile_id>/risc0/manifest.json`

### Public vs private data

For a tile receipt:

- **Private input** (to the prover):
  - The tile’s input bytes provided to the zkVM environment (length-prefixed framing).
- **Public output**:
  - The tile’s output bytes committed to the RISC0 journal.
- **Program identity binding**:
  - Receipt validity is checked against an image ID computed from the tile’s guest ELF.

### Host↔guest input framing (implemented)

The host MUST provide the tile input to the zkVM as:

- a 32-bit little-endian length (`u32`), followed by
- exactly that many raw bytes.

The guest MUST read the input as:

- `let input_len: u32 = risc0_zkvm::guest::env::read();`
- `env::read_slice(&mut input_bytes[..])`

### Tile ABI wrapper encoding (implemented)

The bytes passed into the guest are passed verbatim to the tile ABI wrapper `__raster_tile_entry_<tile>()`. The wrapper:

- MUST interpret the input bytes as `postcard`-encoded values according to the tile’s Rust signature:
  - 0 args: no decode
  - 1 arg: decode that type
  - N args: decode a tuple `(T0, T1, ... TN-1)`
- MUST serialize the return value via `postcard::to_allocvec`.
- MUST return `Err(raster_core::Error::Serialization(...))` on decode/encode failure.

### Statement proven by a tile receipt (implemented)

Given:

- receipt bytes \(R\), which deserialize to a RISC0 `Receipt`,
- an image ID \(I\) computed from an ELF \(E\),

`receipt.verify(I)` establishes, at minimum:

- the prover can produce a valid RISC0 proof that the guest program with image ID \(I\) executed, and
- the program committed some journal byte string \(J\), which is observable as `receipt.journal.bytes`.

Equivalently, the verified statement is:

- “The RISC0 guest image identified by \(I\) produced journal bytes \(J\) for some private input stream.”

### Binding to “original program artifacts” (current behavior)

For tile receipts, Raster binds receipts to artifacts as follows:

- The verifier MUST compute `image_id = risc0_zkvm::compute_image_id(&compilation.elf)` and verify the receipt against that value.
- The verifier MUST treat the receipt as invalid if `receipt.verify(image_id)` fails.

**Gap (artifact binding surface):**

- Raster writes a `method_id` artifact to disk, but receipt verification currently recomputes the image ID from the ELF bytes and does not cross-check `CompilationOutput.method_id` or the on-disk `method_id`.

### Binding to trace commitments (current behavior)

**Gap (no verifier-grade trace commitments):**

- Raster Core currently defines a `Trace` type (`crates/raster-core/src/trace.rs`) but does not implement:
  - trace persistence,
  - verifier-grade trace hashing/commitment over schema-driven step records, or
  - inclusion of trace commitments in zkVM public outputs.

Note: the workspace does implement a `TraceItem` commitment stream (and `--audit` first-diff check) for native whole-program runs, but this is not bound into zk receipts and is not part of an on-chain/verifier-facing fraud-proof format today.

Therefore, there is currently **no mechanism** for a receipt to bind to an execution trace commitment.

### Receipt encoding (implemented)

Raster MUST encode `receipt_bytes` as `postcard::to_allocvec(&risc0_zkvm::Receipt)`.

Raster MUST decode `receipt_bytes` as `postcard::from_bytes::<risc0_zkvm::Receipt>(receipt_bytes)`.

**Gap (stability):**

`postcard` encoding here is not specified as a stable, versioned wire format. Receipt bytes are currently an implementation detail and are not guaranteed to be portable across Raster/RISC0 versions.

### Verification result reporting (implemented)

When `ExecutionMode::Prove { verify: true }` is used during `execute_tile`, the backend:

- MUST attempt local verification `receipt.verify(image_id)`.
- MUST return `verified = Some(true)` if verification succeeds, otherwise `Some(false)`.

When `ExecutionMode::Prove { verify: false }` is used during `execute_tile`, the backend MUST return `verified = Some(false)`.

**Gap (error signaling):**

- In `execute_tile`, receipt verification failure is not returned as an error; it is returned as `verified = false`.

### Error cases (implemented)

A verifier or prover MUST treat the following as errors:

- receipt bytes cannot be deserialized as `risc0_zkvm::Receipt` (`postcard` decode failure)
- image ID cannot be computed from the ELF (`compute_image_id` fails)
- zkVM execution/proving fails (e.g., guest panics, ELF invalid, prover errors)

Additionally:

- If the tile ABI wrapper returns `Err(...)` in the guest, the guest currently panics due to `.expect("Tile execution failed")`. This causes proving/execution to fail rather than producing a structured, typed “tile error” output.

---

## Unimplemented: program-level fraud proofs (window replay + invalid transition)

Raster’s specs in `Specs/Core/3. Execute` and `Specs/Core/4. Verify` describe (or imply) a program-level verification story (window replay, divergence detection, fraud proofs). The current codebase does not yet implement those artifacts or algorithms.

This section states what is currently missing and what additional bindings are required to meet the intended “fraud proof” goal.

### Missing: zkVM guest for window replay

**Gap:**

- There is no dedicated zkVM guest that:
  - replays a window of a multi-tile trace,
  - checks transitions against a control flow schema, or
  - emits a structured proof of an invalid transition.

Today, the only generated guest is per-tile (`GuestBuilder::generate_guest_main`) and commits only the tile output bytes.

### Missing: fraud proof assembly (host-side)

**Gap:**

- There is no host-side code that assembles a “fraud proof” object containing:
  - the disputed window,
  - the relevant program artifacts (CFS / manifests / tile method IDs),
  - prior commitments (trace commitments, state commitments),
  - and a zkVM receipt proving the invalidity claim.

### Required bindings for a future fraud proof (not implemented)

To support end-to-end fraud proofs, an implementation would need to ensure the zkVM public outputs bind to (at least):

- **Program identity**:
  - a commitment to the program bundle / artifact set (tiles + schemas), not just a single tile ELF
- **Trace identity**:
  - a commitment to the trace being disputed (and a way to prove inclusion of the replayed window)
- **Window statement**:
  - the claimed pre-state at window start,
  - the claimed post-state after replay,
  - the exact window boundaries (e.g., start index and length),
  - and the specific failure type (e.g., invalid next-tile selection, invalid dataflow binding, invalid output commitment)

**Gap (current receipt journaling):**

- The generated guest does not commit the input bytes (or a digest) into the journal, so a verifier cannot tell which inputs were used unless inputs are separately committed and proven consistent.

---

## Examples

### Example: tile receipt verification (Rust-like pseudocode)

```rust
// Inputs:
// - compilation.elf: Vec<u8>
// - receipt_bytes: Vec<u8>
//
// Output:
// - ok: bool

let receipt: risc0_zkvm::Receipt = postcard::from_bytes(&receipt_bytes)?;
let image_id = risc0_zkvm::compute_image_id(&compilation.elf)?;
let ok = receipt.verify(image_id).is_ok();

// If ok == true, the public output bytes are:
let output_bytes: Vec<u8> = receipt.journal.bytes.clone();
```

### Example: RISC0 artifact manifest written today

`target/.../tiles/<tile_id>/risc0/manifest.json` is written as:

```json
{
  "tile_id": "double",
  "method_id": "<hex string>",
  "elf_size": 123456
}
```
