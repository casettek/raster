## On-chain Interface Assumptions

This document specifies what the Raster Core toolchain assumes about “on-chain verification” and “settlement”, based on the codebase as it exists today.

Raster Core **does not** contain an on-chain client, contract bindings, or a canonical calldata format. Instead, Raster currently produces *proof-like artifacts* (for the RISC0 backend) that an external relayer/adapter could use to interact with an on-chain verifier.

## Code audit tasks (exactly where to look)

- **RISC0 artifact identity (method ID / image ID)**
  - `crates/raster-backend-risc0/src/risc0.rs`
    - `compile_tile`: computes `method_id = risc0_zkvm::compute_image_id(&elf)` and stores `method_id.as_bytes()`.
    - `execute_tile(..., Prove{verify:true})`: recomputes `image_id = compute_image_id(&compilation.elf)` and calls `receipt.verify(image_id)`.
    - `verify_receipt`: `postcard` deserialization of `risc0_zkvm::Receipt` and `receipt.verify(image_id)`.
- **Guest public output (journal) and private input framing**
  - `crates/raster-backend-risc0/src/guest_builder.rs`
    - `generate_guest_main`: reads `(u32 length, bytes)` from the zkVM environment, calls `__raster_tile_entry_<tile_fn>(input_bytes)`, then commits the **output bytes** via `risc0_zkvm::guest::env::commit_slice(&output)`.
    - This is the *only* committed public output; the input is not committed.
- **Artifact layout and method ID encoding on disk**
  - `crates/raster-compiler/src/builder.rs`
    - `write_tile_artifacts`: writes `guest.elf` and `method_id` (hex string) and `manifest.json`.
    - `load_cached_compilation`: reads `guest.elf` and hex-decodes `method_id`.
  - `crates/raster-cli/src/commands.rs`
    - `output_dir()`: defaults to `./target/raster` for artifact output.
- **Tile ID type**
  - `crates/raster-core/src/tile.rs` (`TileId` is a wrapper around a UTF-8 string)

## What Raster produces today (verification-relevant artifacts)

### Tile identity

- A tile is identified by `tile_id: String` (`raster_core::tile::TileId`).
- Tile IDs are used in artifact paths and manifests; they are not currently hashed or canonicalized for on-chain use.

### Method ID / image ID (RISC0 backend)

When compiling with the `risc0` backend, Raster computes a “method ID”:

- `method_id_bytes = risc0_zkvm::compute_image_id(elf_bytes).as_bytes()`
- The `method_id_bytes` **MUST** be treated as an opaque byte string as returned by RISC0’s `Digest::as_bytes()`. Consumers **MUST NOT** reinterpret endianness or word order.
- Raster writes `method_id` to disk as a **lowercase hex string with no `0x` prefix**.

On disk (default CLI output directory):

- `target/raster/tiles/<tile_id>/risc0/guest.elf`
- `target/raster/tiles/<tile_id>/risc0/method_id` (hex string; decodes to `method_id_bytes`)
- `target/raster/tiles/<tile_id>/risc0/manifest.json` (JSON including `tile_id`, `backend`, `method_id`, `elf_size`, and optional `source_hash`)

### Receipt bytes (RISC0 backend)

When executing in prove mode (`ExecutionMode::Prove { .. }`), Raster returns:

- `receipt_bytes = postcard::to_allocvec(&risc0_zkvm::Receipt)`

When verifying, Raster uses:

- `receipt = postcard::from_bytes::<risc0_zkvm::Receipt>(receipt_bytes)`
- `image_id = compute_image_id(compilation.elf)`
- `receipt.verify(image_id)`

**Important gap (portable encoding):** the receipt bytes are *not* a stable, versioned interchange format. They are Rust `postcard` for an upstream type (`risc0_zkvm::Receipt`) and are not expected to be directly consumable by on-chain verifiers.

### Public output (journal bytes)

For the RISC0 backend, Raster treats the guest journal bytes as the tile output:

- `output_bytes = receipt.journal.bytes` (prove mode)
- `output_bytes = session.journal.bytes` (estimate mode)

The guest commits exactly one byte string to the journal: the return value of the tile ABI wrapper.

**Important gap (input binding):** the guest does **not** commit the tile input bytes (or a digest of them) to the journal. A proof therefore attests only that “this guest image produced these journal bytes for some private input stream”, not which input was used.

## Minimal on-chain interface Raster assumes exists (conceptual)

Raster Core does not ship contract code or an ABI. The minimal on-chain surface implied by the current design is:

- A contract (or precompile) that can verify a zk proof for a given program identity (method/image ID), and
- A way to bind the *claimed public output* (journal bytes or a digest thereof) to that verification.

Because Raster currently emits only:

- the program identity (`method_id_bytes`), and
- an opaque serialized receipt (`receipt_bytes`), and
- the public output bytes (`output_bytes`),

any on-chain integration necessarily includes an **adapter/relayer** that converts Raster’s artifacts into the chain verifier’s expected proof format.

### Required contract capability

An on-chain verifier integration **MUST** provide a function that can be modeled as:

- Inputs:
  - `method_id` (program identity; typically `bytes32` on EVM-like chains)
  - `public_output` (either the raw journal bytes, or a commitment/digest to them)
  - `proof` (the chain-verifier-specific proof encoding)
- Behavior:
  - Reverts or returns `false` if the proof is invalid for the given `method_id`.
  - Reverts or returns `false` if the proof’s public output does not match the supplied `public_output` claim.
  - Returns `true` (or does not revert) on success.

Raster does **not** currently define:

- how `TileId` is represented on-chain,
- how `public_output` is hashed/committed,
- how proofs are encoded for calldata,
- how a verifier key registry is managed.

### Suggested minimal EVM ABI (NOT implemented in Raster)

For settlement systems that want a concrete shape, the minimal EVM-like ABI that matches Raster’s produced artifacts is:

```solidity
interface IRasterTileVerifier {
    /// Verify a single tile execution claim.
    /// methodId: RISC0 image ID bytes (as produced by compute_image_id(elf).as_bytes()).
    /// journal: public output bytes (guest journal bytes).
    /// proof: chain-verifier-specific proof bytes (NOT Raster's postcard Receipt).
    function verifyTile(bytes32 methodId, bytes calldata journal, bytes calldata proof)
        external
        view
        returns (bool);
}
```

**Gap:** Raster does not emit `proof` in this form today; it emits `receipt_bytes` as `postcard(Receipt)`. A relayer would need to deserialize the receipt and translate it into the verifier’s calldata format.

## Data Raster MUST output for settlement (current reality)

For each tile execution that is intended to be verifiable by third parties, Raster tooling **MUST** make available the following tuple:

- `tile_id: String`
- `method_id_bytes: [u8]` (decoded from the `method_id` hex file, or taken from `CompilationOutput.method_id`)
- `output_bytes: [u8]` (journal bytes; `TileExecution.output`)
- `receipt_bytes: [u8]` (present only in prove mode; `TileExecution.receipt`)

Consumers that build settlement transactions **MUST** treat `receipt_bytes` as an opaque blob unless they are using the exact same Rust/RISC0 versions needed to deserialize it.

### Example settlement payload (off-chain envelope)

This JSON is not produced by Raster today, but represents the minimal data a relayer needs to carry forward from Raster execution:

```json
{
  "tile_id": "example-tile",
  "backend": "risc0",
  "method_id_hex": "3a7f... (64 hex chars typical)",
  "journal_hex": "0a0b0c...",
  "receipt_postcard_hex": "f0f1f2..."
}
```

## Gaps and divergences from “on-chain settlement” expectations

The items below are important for implementers; they are not hypothetical—each is a direct consequence of current code behavior.

- **Receipt encoding is not a contract ABI**
  - Raster serializes `risc0_zkvm::Receipt` with Rust `postcard`.
  - This is not suitable as on-chain calldata and is not a stable interchange format.
- **No input commitment**
  - The guest commits only the output bytes to the journal.
  - A verifier cannot learn or validate the tile input from the proof alone.
- **No program-level / multi-tile settlement object**
  - There is no implemented “program receipt”, “trace commitment”, “state root”, or “step commitment” that aggregates multiple tiles for on-chain settlement.
- **No canonical hashing for chain interaction**
  - `raster-core` currently provides no keccak/SHA commitment helpers for journal bytes, receipts, or traces that an EVM-like chain would consume.
- **No enforcement of artifact consistency**
  - Raster verification recomputes the image ID from the ELF; it does not currently enforce that a stored/loaded `method_id` equals `compute_image_id(guest.elf)` when artifacts are paired.

