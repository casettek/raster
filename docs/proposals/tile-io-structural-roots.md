# Proposal: `tile-io-structural-roots` — bind what a tile reads and writes to its replay

Status: proposed 2026-09-28. Closes
[`tile-output-commitment-unbound`](../issues/tile-output-commitment-unbound.md) and
[`selection-unbound-from-execution`](../issues/selection-unbound-from-execution.md) in one tile
image-id break.

Related:
- [`incremental-draft-materialization`](./incremental-draft-materialization.md) — its D1/D2
  already change the tile replay journal, its D3 makes draft- and state-returning iterations write
  nothing, and its D5b needs the same shared encoder (step 0). Batch the breaks.
- [`program-output-unbound`](../issues/program-output-unbound.md) — pins *where* every value was
  written (`ProgramEnd`, arguments, nested returns). This proposal pins *what*.
- [`paged-bytes`](./paged-bytes.md) §3.3 — first sketch of the input half ("tile guests commit a
  structural root per decoded input"), a release gate on `Bytes`.
- [`lazy-list-recur`](./lazy-list-recur.md) §6 — rule 8, the sweep cross-check (step 1).

## Problem

A tile step is replay-proven: the journal shows *the binary, on these input bytes, produced these
output bytes*. Nothing joins either side of that statement to storage.

| side | proven | stored / selected | joined? |
| --- | --- | --- | --- |
| output | `journal.output_bytes` (postcard) | `step.output_commitment`, written at the step's coordinates | **no** — `verify_io_witness` returns early for execution steps; the store check ignores the output witness; a tile with output need not write at all |
| input | `journal.input_commitment` = hash of the recorded input bytes | each storage-bound argument's selection proof, folded to the source's root | **no** — the guest never decodes the input, so the selected bytes and the executed bytes are never compared (§2b) |
| sweep position | `RecurPosition { iteration_index, consumed_elements, … }` | a chunked iteration's `ListRange { start, len }` selection | **no** — rule 8 is unimplemented; a sweep can re-read chunk 0 every iteration (§2a, demonstrated) |

Both issues measured their gap with a guest probe or a `poc_` test; see them for reproductions.

## Design

### Step 0 — one encoder, proven

The tile guest must compute a value's **raster root** — the commitment storage uses — without
`raster-runtime`, which it does not link. `raster-core` already has a no-std encoder
(`draft::draft_value_from_serialize` + `draft_value_payload_and_root`) whose own comment says it
must track `raster-runtime`'s `assemble_subtree` exactly; the two are maintained by hand, and the
models differ (the runtime's `TreeValue` has a `ListHandle` node for a `List<T>` field).

A corpus test compares `draft_value_root(draft_value_from_serialize(v))` with
`encode_raster_value(v)`'s root for every value shape: all integer widths, strings, unit,
options, tuples, all four enum variant kinds, maps, `List` fields vs `Block`, `Bytes<N>` /
`BytesPage`, and nesting. Any disagreement is fixed — in the core encoder, or by moving the
runtime's encoder into `raster-core` so one implementation exists. D5b of
`incremental-draft-materialization` needs the same guarantee.

**Done 2026-09-28 — one encoder.**

1. *Measured first.* A 55-case corpus compared the two encoders: every root agreed, but **13
   payloads differed** — every value containing a `List<T>` or `Bytes<N>`. The core encoder had
   no `ListHandle` node (added to the runtime only, by `bounded-collections`), so it wrote the
   list inline where storage writes a 49-byte `(root, len)` header first. Roots agreed only
   because a handle's root is defined as its list's root. No caller stored a core-encoded
   payload, so nothing was wrong yet.
2. *Moved.* The runtime's `TreeValue`, its serializer and `subtree_payload_and_root` now live in
   `raster_core::tree` (`no_std` + `alloc`); `raster-runtime` re-exports them and keeps what is
   host-only (the deserializer, index building, selection proofs). `DraftValue` is now
   `pub type DraftValue = TreeValue`, and the second serializer and encoder in `draft.rs` are
   deleted. `ListHandle` is the enum's **last** variant, so the serde encoding of every existing
   `DraftOp` / `DraftWitnessField` — which carry `DraftValue` into the transition guest — is
   unchanged.
3. *One behaviour change, payload only.* The runtime's `runtime_tree_value` turned **every**
   draft `List` into a `ListHandle` at finalize, because `DraftValue` could not say which lists
   were `List<T>`. Its intent — an append field is a `List<T>` — now sits in
   `draft_tree_from_fields` (append fields finalize to `ListHandle`); a `Vec` / `Block` inside a
   set-once field or an appended element now keeps its inline form, as when a tile writes the
   same value directly. Roots are unchanged (the pinned draft-root literal still passes).
4. *Guard.* `crates/raster-core/tests/tree_roots.rs` runs the same corpus against the one other
   place the hash rule is spelled — `payload_structural_root`, which recomputes a root from
   payload bytes for the chain verifier, the reader and the selection checks. 49 shapes
   re-derive their encoder root; 6 (`u128`, `i128`, `f32`, `f64`, `char`, `serialize_bytes`) are
   refused. A mutated hash tag fails it.
5. *Cost: a tile image-id break, taken now.* Every tile guest commits a `TileReplayJournal`, whose
   `draft_transition` carries `DraftOp`s and so `DraftValue`'s serialize code — linked into every
   tile, draft or not. The new variant changes it, so all 6 tracked locks were regenerated: every
   tile image id and every `program_commitment` moved (the chain examples too). Step 2 breaks
   tile image ids again; batch the two before a release rather than shipping step 0 alone.

### Step 1 — rule 8, the sweep cross-check (no break)

In `advance_recur_progress`'s recur-tile iteration branch, require the iteration's recorded range
selection to agree with the replay journal and the frame:

- `ListRange.len == L` (the frame's `source_len`);
- `ListRange.start == consumed_total` before this iteration;
- the payload's element count `k == consumed_elements`.

A per-element sweep's `Index(i)` selection is held to `i == consumed_total` the same way. The
existing `poc_a_sweep_that_rereads_the_first_chunk_passes_every_completeness_rule` inverts into
the regression test.

### Step 2 — structural roots in `TileReplayJournal` (one image-id break)

- **Output.** The tile guest computes `output_root: Option<Hash32>` from its typed result with the
  step-0 encoder — `None` for a draft- or state-returning recur iteration, which writes nothing.
  The transition guest requires a tile step to write **iff** `output_root` is `Some`, and
  `step.output_commitment == output_root` when it does.
- **Input.** The tile guest commits `input_roots: Vec<Option<Hash32>>`, one per decoded argument.
  For each storage-bound argument the transition guest requires the selection proof to start
  from that root — the payload's subtree root, or `selected_root` for a reference witness.
- **Recorder parity.** At record time the recorder recomputes the stored object's root from its
  payload bytes and compares it with the claimed `root_hash`, so a child encoder bug fails the run
  rather than producing a trace no honest challenger can defend.

### Step 3 — verify and regenerate

Invert both issues' probes (a forged output commitment; the re-read sweep), replay real traces
host-side as for `program-output-unbound`, run the proving tests (GPU), and regenerate the locks
with `cargo raster build --backend risc0` (the tile cache key now tracks raster sources).

## Order

Step 0, then step 1 (independent, small), then step 2, batched with
`incremental-draft-materialization`'s journal changes if that lands first.

## Costs

Step 2 moves every tile image id, hence every `program_commitment`, and grows each replay journal
by one root per argument plus one for the output. Step 1 changes no format.
