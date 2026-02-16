## Verifier Inputs and Outputs

This document specifies what a Raster verifier consumes and produces, based on the codebase as it exists today. Where the desired verification interface (multi-tile execution verification, divergence detection, trace commitments, fraud proofs) is referenced elsewhere in the specs, this document explicitly marks the missing implementation surface.

---

## Code audit tasks (where to look)

### Receipt verification (implemented)
- **Backend verification API surface**
  - Inspect `crates/raster-backend/src/backend.rs`
    - `trait Backend::verify_receipt(compilation, receipt) -> Result<bool>`
    - `struct CompilationOutput { elf, method_id, artifact_dir }`
    - `struct TileExecution { receipt: Option<Vec<u8>>, verified: Option<bool>, ... }`
- **RISC0 receipt verification implementation**
  - Inspect `crates/raster-backend-risc0/src/risc0.rs`
    - Receipt serialization: `postcard::{to_allocvec, from_bytes}`
    - Image ID derivation: `risc0_zkvm::compute_image_id(&compilation.elf)`
    - Verification: `receipt.verify(image_id)`
    - Note that verification currently recomputes the image ID from the ELF and does not use `compilation.method_id` as the verification target.
- **Guest I/O binding (what the receipt commits to)**
  - Inspect `crates/raster-backend-risc0/src/guest_builder.rs`
    - Guest reads input as: `u32 input_len` followed by `input_len` raw bytes.
    - Guest commits output to the receipt journal via `env::commit_slice(&output)`.
  - Inspect `crates/raster-macros/src/lib.rs`
    - The `#[tile]` macro generates the ABI wrapper invoked by the guest (`__raster_tile_entry_<tile_id>`), which defines how inputs are decoded and outputs are encoded for the guest/host boundary.

### Artifact inputs / data availability (partially implemented)
- **On-disk tile artifact layout used by the CLI**
  - Inspect `crates/raster-compiler/src/builder.rs`
    - Artifact directory: `target/raster/tiles/<tile_id>/<backend>/`
    - Files: `guest.elf`, `method_id`, `manifest.json`
    - Types: `TileArtifact`, `TileManifest`
- **CLI behavior (how artifacts + inputs are assembled today)**
  - Inspect `crates/raster-cli/src/commands.rs`
    - `run(...)`: builds tile artifacts, loads `guest.elf` and `method_id`, executes via backend
    - Input encoding path: JSON string → `serde_json::Value` → `postcard::to_allocvec(...)`
    - Note: the CLI does not persist receipts; it only prints whether a receipt was generated and whether local verification succeeded.

### CFS / schema inputs (implemented for generation; not used by any verifier today)
- **Control Flow Schema (CFS) types and serde representation**
  - Inspect `crates/raster-core/src/cfs.rs`
  - Inspect CLI command `cfs(...)` in `crates/raster-cli/src/commands.rs`

### Trace material (types exist; persistence + verification not implemented)
- **Trace types**
  - Inspect `crates/raster-core/src/trace.rs`
- **Trace-item commitment audit (implemented; not a verifier API)**
  - Inspect `crates/raster-runtime/src/tracing/subscriber/{commit,audit}.rs`
  - Inspect `crates/raster-prover/src/trace.rs` and `crates/raster-prover/src/bit_packer.rs`

---

## Spec output (what to document)

## Verifier inputs (as implemented today)

Raster’s only implemented “verification” in the strict zk-proof sense is **tile-level zkVM receipt verification** via the RISC0 backend. There is no program/sequence verifier, no “what-must-run-next” verifier, and no fraud-proof construction pipeline.

The workspace *does* implement a local trace-commitment checker for native whole-program runs (`--commit` / `--audit` over `TraceItem` prefixes), but there is no verifier API that consumes a claimed commitment and produces verifier outputs suitable for dispute protocols.

### 1) Tile artifact identity inputs

#### 1.1 Guest ELF bytes (required for receipt verification in current code)
- A verifier **MUST** have the exact compiled guest ELF bytes for the tile being verified when using the RISC0 backend implementation in this repository.
- In current code, the image ID used for verification is recomputed as:
  - `image_id = risc0_zkvm::compute_image_id(&guest_elf_bytes)`

**Minimum required data availability (implemented today)**:
- **`guest.elf` bytes**
- **`receipt` bytes** (see §2)

**Gap (artifact binding)**:
- Although tile artifacts store a `method_id` value, receipt verification currently does **not** verify against that stored value; it recomputes the image ID from `guest.elf`. This means:
  - A verifier that only has `method_id` but not `guest.elf` cannot verify a receipt using the Raster code path.
  - Tooling can accidentally pair a mismatched `guest.elf` and `method_id` without an immediate error unless it adds an explicit consistency check.

#### 1.2 Method ID bytes (available; not required by current verifier code)
Tile builds store a “method ID” (RISC0 image ID) alongside the ELF:
- On disk: ASCII lowercase hex in the file `method_id`.
- In memory: `CompilationOutput.method_id: Vec<u8>` (raw bytes) and `TileArtifact.method_id: String` (hex).

**Recommended check (not enforced today)**:
- When both are available, implementations **SHOULD** check:
  - `hex_decode(method_id_file) == risc0_zkvm::compute_image_id(guest_elf).as_bytes()`

### 2) Execution evidence inputs

#### 2.1 RISC0 receipt bytes (required for receipt verification)
The RISC0 backend produces and verifies a receipt with these properties:
- Receipt bytes are produced as `postcard::to_allocvec(&risc0_zkvm::Receipt)`.
- Receipt bytes are consumed by verification as `postcard::from_bytes::<risc0_zkvm::Receipt>(receipt_bytes)`.

Therefore:
- A verifier implementation that matches the current Raster code **MUST** treat the receipt encoding as **`postcard` of the RISC0 `Receipt` type**.

**Gap (portable receipt encoding)**:
- Raster does not define a stable, language-agnostic receipt encoding format at this layer. Any non-Rust verifier cannot decode receipts using this spec alone.

#### 2.2 What the receipt binds (public I/O)
In the generated guest program, the guest commits the tile’s output bytes to the journal:
- `risc0_zkvm::guest::env::commit_slice(&output)`

Therefore, successful receipt verification establishes at least:
- The guest program identified by the verification image ID executed and produced the journal bytes present in the receipt.

**Gap (input binding)**:
- The receipt does not, by itself, bind the host-provided input bytes unless the guest explicitly commits them (or a commitment to them) to the journal. The generated guest currently commits only the output bytes.

### 3) External inputs (host-provided input bytes)

Raster currently treats “tile input” as an opaque byte string passed from host to guest:
- The `#[tile]` macro defines the tile ABI wrapper as:
  - **Decode**: `postcard::from_bytes::<T>(input_bytes)` (or `postcard::from_bytes::<(T1, T2, ...)>(input_bytes)` for multiple args)
  - **Encode**: `postcard::to_allocvec(&result)`
  - Decode/encode failures are surfaced as `Error::Serialization(...)`.
- The CLI builds `input_bytes` by serializing either:
  - a JSON value via `postcard::to_allocvec(&serde_json::Value)`, or
  - unit `()` via `postcard::to_allocvec(&())` when no input is supplied.
- The RISC0 host writes into the guest environment:
  - `u32 input_len` followed by `input_len` raw bytes.

Therefore:
- A verifier **MAY** validate receipt correctness without any knowledge of the input bytes (because input is not committed by the guest).
- A verifier that needs “full statement” binding (program + input + output) cannot obtain it from current Raster receipts alone.

**Gap (CLI input encoding vs tile ABI)**:
- The CLI’s “JSON input” path currently postcard-encodes a `serde_json::Value`, but the tile ABI wrapper expects postcard encoding of the tile’s declared Rust input type(s). Unless the tile input type is itself `serde_json::Value`, this encoding will not decode correctly in the guest and is not a stable basis for verification statements.

---

## Additional inputs that exist in the codebase (not consumed by any verifier today)

### 1) Control Flow Schema (CFS)
Raster can generate a JSON Control Flow Schema describing tile/sequence structure and dataflow bindings.

#### CFS JSON shape
The CFS is the serde-JSON encoding of `raster_core::cfs::ControlFlowSchema`:
- `version: String` (currently `"1.0"`)
- `project: String`
- `encoding: String` (currently `"postcard"`)
- `tiles: [TileDef]`
- `sequences: [SequenceDef]`

Input bindings use a tagged enum shape:
- `InputSource` is encoded as an object with `"type"` plus fields:
  - `{ "type": "external" }`
  - `{ "type": "seq_input", "input_index": <usize> }`
  - `{ "type": "item_output", "item_index": <usize>, "output_index": <usize> }`

**Gap (verifier integration)**:
- No verifier consumes the CFS today. There is no implemented algorithm that uses CFS dataflow to determine what must run next, or to validate a trace against the CFS.

### 2) Trace material
The codebase defines an in-memory trace representation (`Trace`, `TraceEvent`), but does not persist traces to disk and does not verify them.

**Gap (trace availability and commitment)**:
- There is no on-disk persistence format for `Trace`/`TraceEvent` and no verifier integration for traces.
- There is an on-disk packed commitment stream format for `TraceItem` prefixes (produced by `--commit`), and a local checker (`--audit`) that can locate the first mismatch between an expected and recomputed stream.

---

## Verifier outputs (as implemented today)

### 1) Receipt verification result
The implemented verifier output is:
- `Result<bool>` from `Backend::verify_receipt(...)`
  - `Ok(true)` iff receipt verification succeeds against the computed image ID
  - `Ok(false)` iff receipt verification fails (invalid proof or mismatch)
  - `Err(...)` iff receipt deserialization fails or image ID computation fails

The tile execution path additionally surfaces proof material and (optional) local verification status via:
- `TileExecution.receipt: Option<Vec<u8>>`
- `TileExecution.verified: Option<bool>`

**Gap (verified meaning in tile execution)**:
- In the current RISC0 backend, when proving is requested but verification is not, the execution result still reports `verified = Some(false)`. Callers **MUST NOT** interpret `false` as “verification failed” without also knowing whether verification was requested.

**Gap (diagnostics)**:
- Verification failure details are reduced to a boolean in the `TileExecution` path; the underlying verification error is not surfaced in a structured way.

### 2) Divergence point (not implemented)
Raster does not currently output:
- a divergence index / “first mismatching step”
- a “what-must-run-next” set
- per-invocation/step proof objects

Any spec consumer expecting a divergence point **MUST** treat it as unavailable in current Raster.

---

## Exact input bundle format (implemented today)

There is no single “verifier bundle” file format in the repository today. The effective bundle format is the in-memory parameter set required by `Backend::verify_receipt` for the RISC0 backend:

### TileReceiptVerificationInput (effective, in-memory)
- **`guest_elf_bytes: bytes`** (the full contents of `guest.elf`)
- **`receipt_bytes: bytes`** (`postcard` serialization of `risc0_zkvm::Receipt`)

### Verification algorithm (RISC0 backend)
An implementation matching the current code performs:
- Deserialize: `receipt = postcard::from_bytes::<risc0_zkvm::Receipt>(receipt_bytes)`
- Compute: `image_id = risc0_zkvm::compute_image_id(guest_elf_bytes)`
- Verify: `valid = receipt.verify(image_id).is_ok()`
- Return: `valid`

**Gap (bundle portability / stability)**:
- Because the receipt is encoded from a Rust type (`risc0_zkvm::Receipt`) and Raster does not define a versioned wire envelope, this input bundle is not specified as a stable cross-language interchange format.

---

## On-disk artifact format (implemented today; used to source inputs)

Raster’s CLI build pipeline produces per-tile artifacts under:
- `target/raster/tiles/<tile_id>/<backend>/`

For the RISC0 backend, `<backend>` is `risc0`, and the directory contains:
- `guest.elf` (raw bytes)
- `method_id` (ASCII hex of the method/image ID)
- `manifest.json` (JSON metadata)

The CLI’s build system overwrites/standardizes `manifest.json` into the serde-JSON encoding of `raster_compiler::builder::TileManifest`:
- `tile_id: String`
- `backend: String`
- `method_id: String` (hex)
- `elf_size: usize`
- `source_hash: Option<String>`

---

## Examples

### Example: artifact directory (RISC0 backend)

```
target/raster/tiles/double/risc0/
  guest.elf
  method_id
  manifest.json
```

### Example: `manifest.json` (shape)

```json
{
  "tile_id": "double",
  "backend": "risc0",
  "method_id": "0123abcd... (hex)",
  "elf_size": 123456,
  "source_hash": "00112233445566778899aabbccddeeff0011223344556677"
}
```

### Example: CFS snippet (shape only)

```json
{
  "version": "1.0",
  "project": "hello-tiles",
  "encoding": "postcard",
  "tiles": [
    { "id": "double", "type": "iter", "inputs": 1, "outputs": 1 }
  ],
  "sequences": [
    {
      "id": "my_sequence",
      "input_sources": [{ "source": { "type": "external" } }],
      "items": [
        {
          "item_type": "tile",
          "item_id": "double",
          "input_sources": [{ "source": { "type": "seq_input", "input_index": 0 } }]
        }
      ]
    }
  ]
}
```

---

## Summary of current gaps vs desired verification stage

- There is **no program-level verifier**: verification is per-tile receipt verification only.
- There is **no divergence-point output** and no “what-must-run-next” implementation.
- There is **no event-trace (`Trace`/`TraceEvent`) persistence** and no verifier-grade trace commitment/fingerprint verification pipeline.
- Receipt encoding is **Rust `postcard` over `risc0_zkvm::Receipt`**, not a versioned, portable format.
- Receipt verification uses **`guest.elf` → computed image ID**, not the stored `method_id` value; mismatches are not checked by default.
