## Trace Commitment and Fingerprinting

This document specifies Raster’s **implemented trace-item commitment scheme** (used by `#[raster::main]` via `--commit` / `--audit`) and the **intended future** commitment/fingerprinting surfaces needed for window replay and verifier workflows.

It also documents the current implementation status and explicitly identifies gaps between this spec and the codebase as it exists today.

---

### Code audit tasks (where to look)

- **Trace data models**
  - `crates/raster-core/src/trace.rs` (`Trace`, `TraceEvent`, `TraceItem`)
- **Commitment + packing implementation (authoritative today)**
  - `crates/raster-prover/src/trace.rs` (hashing + incremental Merkle commitment stream)
  - `crates/raster-prover/src/bit_packer.rs` (bit packing + first-diff locator)
- **Runtime integration**
  - `crates/raster-runtime/src/tracing.rs` and `crates/raster-runtime/src/tracing/subscriber/*`
  - `crates/raster-macros/src/lib.rs` (`#[raster::main]` parses `--commit` / `--audit` and initializes the corresponding subscriber)
- **Related hashing/ID conventions**
  - `specs/Core/0. Conventions/01. IDs and Hashing.md`

---

### 1. Current implementation status (what exists today)

Raster currently provides:

- A coarse event trace model (`Trace` / `TraceEvent`) that is serde-serializable (std-only module), but this workspace does not provide an end-to-end event-trace emitter/persistence layer.
- A tile I/O trace-item model (`TraceItem`) plus an **implemented commitment stream** over `TraceItem`s:
  - `--commit <path>`: writes a packed commitment stream to a file.
  - `--audit <path>`: recomputes and compares packed commitments against an expected file and reports the first mismatch.

However:

- There is **no windowing scheme** implemented for replay protocols.
- The commitment hashing input uses Rust `postcard` today (not a versioned, language-agnostic canonical encoding).

---

### 2. Trace object model (implemented)

#### 2.1 Trace

A `Trace` is an in-memory record of events:

- `run_id: String`
- `timestamp: u64`
- `events: Vec<TraceEvent>`

**Ordering**:

- Producers MUST append `TraceEvent`s to `Trace.events` in the order they occur.
- Consumers MUST interpret `Trace.events` as an ordered sequence.

**Time fields**:

- `Trace.timestamp` and per-event `timestamp` fields are plain `u64` values with no unit or epoch defined by current code.
- **GAP**: the runtime does not currently define whether these are wall-clock time, monotonic time, or a logical clock, nor does it populate `Trace.timestamp` with a real value.

#### 2.2 TraceEvent

`TraceEvent` is a tagged enum (`#[serde(tag = "type")]`) with variants:

- `TileStart { tile_id: TileId, timestamp: u64, depth: u32 }`
- `TileEnd { tile_id: TileId, timestamp: u64, duration_ns: u64, cycles: Option<u64> }`
- `SequenceStart { name: String, timestamp: u64 }`
- `SequenceEnd { name: String, timestamp: u64, duration_ns: u64 }`

**Tile identifiers**:

- `tile_id` is a `TileId` newtype wrapper around a string; serialization treats it as the underlying string.

**Structural invariants (recommended; not enforced)**:

- A `TileEnd` event SHOULD correspond to a prior `TileStart` with the same `tile_id` at the same `depth`.
- A `SequenceEnd` event SHOULD correspond to a prior `SequenceStart` with the same `name`.
- **GAP**: current code does not enforce pairing, nesting, or timestamp monotonicity.

---

### 3. Implemented: `TraceItem` commitment stream (native whole-program runs)

Raster’s implemented commitment scheme applies to a stream of `raster_core::trace::TraceItem` records (tile I/O transcript items), not to the coarse `TraceEvent` stream.

#### 3.1 Per-item hash (implemented)

For each `TraceItem` \(t_i\), the current implementation computes:

- `item_hash_i = SHA-256(postcard(t_i))`

**Portability caveat**: `postcard` over Rust types is not specified here as a stable, versioned canonical encoding.

#### 3.2 Commitment stream (implemented)

Raster constructs an incremental Merkle commitment stream over the item hashes using a bridge tree:

- The tree is initialized by appending a fixed **seed** (currently `EMPTY_TRIE_NODES[0]` from `raster-prover` precomputed constants) as the first leaf.
- For each `item_hash_i`, append it as a leaf, and record the current root.
- The commitment output is therefore a vector of roots \([root_0, root_1, ..., root_{n-1}]\) where `root_i` commits to the prefix up to and including item `i`.

#### 3.3 Packed fingerprint / commitment file format (implemented)

To make comparisons compact, Raster packs a fixed number of bits from each root into `u64` blocks:

- Let `bits_per_item = B` (currently `B = 16` as chosen by `#[raster::main]`).
- For each root (32 bytes), crop to the lowest `B` bits (in the current little-endian packing convention used by `raster-prover::bit_packer`).
- Pack consecutive cropped values into a `Vec<u64>` bitstream, then write the stream to disk as:
  - concatenated `u64::to_le_bytes()` blocks (little-endian), with no header.

This file is what `--commit <path>` produces.

#### 3.4 Audit / first-diff localization (implemented)

`--audit <path>` recomputes the packed stream and compares it to the expected file:

- If lengths differ: audit fails.
- Otherwise, it locates the first differing packed value index and surfaces:
  - the differing index,
  - the expected vs computed packed values, and
  - the corresponding `TraceItem` (for debugging).

---

### 4. Future: portable trace commitment scheme (for verifier/window-replay workflows)

This section specifies a concrete commitment scheme to unblock interoperable implementations. Raster does not yet implement this scheme.

#### 4.1 Commitment algorithm

Let:

- `DST` be the UTF-8 bytes of the domain separation string:
  - `raster.trace.commitment.v1`
- `ENC(trace)` be the canonical byte encoding of `trace` (see §4.2)
- `H` be a collision-resistant hash function with 32-byte output (e.g., SHA-256).

Then the trace commitment is:

\[
\mathrm{commitment} = H(\mathrm{DST} \parallel 0x00 \parallel \mathrm{ENC}(\mathrm{trace}))
\]

**Output format**:

- The commitment MUST be represented as 32 raw bytes.
- When rendered as text (e.g., JSON, logs), it MUST be lowercase hex with no `0x` prefix.

**GAP (code)**:

- Raster does not currently compute a single, portable “trace commitment digest” over `Trace`/`TraceEvent` suitable for cross-language verification.

#### 4.2 Canonical encoding for commitment input

To make commitments stable across implementations, `ENC(trace)` MUST be deterministic.

Proposed rule (v1):

- `ENC(trace)` MUST be `postcard` encoding of a dedicated struct:
  - `TraceCommitmentInputV1 { trace: Trace }`

Rationale:

- `postcard` is already a workspace dependency and is used as Raster’s canonical tile I/O encoding.

**GAP (code)**:

- Raster does not currently define `TraceCommitmentInputV1` or any “canonical trace encoding” API surface.
- The `Trace` module is `std`-gated and currently uses `serde` derives only; no commitment encoder exists.

---

### 5. Fingerprint projection scheme (future; not implemented for verifier workflows)

#### 5.1 Projection rule

Given a 32-byte commitment digest `C`, the 128-bit fingerprint `F` is:

- `F = C[0..16]` (the first 16 bytes, in byte order as produced by `H`)

**Text rendering**:

- The fingerprint MUST be rendered as 32 lowercase hex characters (16 bytes).

#### 5.2 Collision and ambiguity handling

- If a system uses fingerprints as lookup keys, it MUST treat a fingerprint match as a *candidate* match and SHOULD confirm by comparing the full commitment (or the full trace) when available.

**Current status**:

- Raster *does* compute a packed commitment stream for `TraceItem` prefixes (see §3), but it does not compute a single 128-bit “fingerprint” identifier intended for indexing/verifier protocols.

---

### 6. Security rationale (binding target: 128 bits)

This section explains the 128-bit target for fingerprints.

- A 128-bit fingerprint provides approximately \(2^{128}\) second-preimage work for an attacker trying to find an alternate trace with the same fingerprint, assuming the underlying commitment digest is computed using a collision-resistant hash function and the projection is a simple truncation.
- For accidental collisions across many traces, the birthday bound applies: roughly \(2^{64}\) traces are needed before collisions become likely for uniformly random 128-bit values.

This is intended to make fingerprints safe for:

- user-facing identifiers
- database keys with low collision risk
- “window selection” indices used in replay protocols (where the full commitment can still be used for confirmation)

---

### 7. Windowing and `window_size` parameter usage (GAP / not implemented)

Raster’s broader execution/verify specs reference “window replay”. In support of that, implementations typically introduce a `window_size` parameter that determines how a trace is partitioned into contiguous windows and how window commitments/fingerprints are computed.

**Current status**:

- There is no `window_size` parameter in Raster CLI/runtime code today.
- There is no implemented algorithm for partitioning a trace into windows or computing per-window commitments.

**Proposed semantics (for future implementation)**:

- A “step” MUST be defined as a single `TraceEvent` in `Trace.events`.
- A window MUST be a contiguous slice of the event stream.
- `window_size` MUST be a positive integer specifying the maximum number of events per window.
- Window `i` MUST cover events in index range:
  - `[i * window_size, min((i + 1) * window_size, len(events)))`
- A window commitment SHOULD bind to:
  - the parent trace commitment (or trace identity)
  - the window index `i`
  - the canonical encoding of the window’s events

**GAP (spec integration)**:

- Other specs under `specs/Core/3. Execute/` that would define “window replay execution” and “trace generation” are currently empty in this repository snapshot, so there is no end-to-end, repo-consistent definition of window replay inputs/outputs yet.

---

### 8. Examples

#### 8.1 Example trace (illustrative JSON rendering)

If a trace is serialized as JSON via `serde_json` (not currently implemented by the tracer), it would look like:

```json
{
  "run_id": "run-123",
  "timestamp": 0,
  "events": [
    { "type": "SequenceStart", "name": "main", "timestamp": 100 },
    { "type": "TileStart", "tile_id": "double", "timestamp": 110, "depth": 0 },
    { "type": "TileEnd", "tile_id": "double", "timestamp": 120, "duration_ns": 1000, "cycles": 4242 },
    { "type": "SequenceEnd", "name": "main", "timestamp": 130, "duration_ns": 2000 }
  ]
}
```

#### 8.2 Example fingerprint rendering (format)

Given a (hypothetical) 32-byte commitment:

- `C = 32 bytes`

the fingerprint is:

- `F = first 16 bytes of C`
- rendered as 32 hex characters, e.g.:

```text
9f2a0c1e6b5d4c3a1122334455667788
```

**Note**: the numeric value above is format-only; Raster does not yet implement commitment computation.

