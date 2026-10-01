# Proposal: `tile-io-structural-roots` — bind what a tile reads and writes to its replay

Status: proposed 2026-09-28; steps 0 and 1 done 2026-09-30; step 2 done 2026-10-01 (with
`incremental-draft-materialization` batch C). Closes
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
5. *Cost, measured against `HEAD` with the workspace CLI:* `build_recur_draft_greeting` is the only
   tile whose image id moved (it calls the draft serializer); no chain-example tile moved. Every
   `program_commitment` moved, but that diff also carries the uncommitted nested-returns CFS change
   and step 1's new transition guest, so it is not step 0's alone. (A first measurement said every
   tile moved: it had been built with the stale installed `cargo raster`, not the workspace binary.)

### Step 1 — rule 8, the sweep cross-check

**Done 2026-09-30**, for recur tiles and recur sequences.

*Finding first.* Recur iterations skip the CFS input check (`verify_step_record_inputs` and the
sequence-scope check both return early on an iteration coordinate). So beyond the missing position
check, **nothing tied an iteration's item to the site's source at all** — `checks::store` proves
only that the item is a correct slice of *some* stored object. The three equalities above would
have pinned the position of a slice of any list of length `L`.

*Design.*

- `RecurProgressFrame` gains `source: Hash32` — `recur_progress::source_identity` of the site
  `Start`'s `"input"` binding: `H("recur-source" ‖ postcard(coordinates, commitment,
  selection.path))`. `Start` passes the full CFS input check, so its object is bound; holding the
  identity in the frame is what lets an iteration — or a fraud window opening mid-sweep, through
  the seed — be checked against it, the same way `L` is. The recorder and the guest call the same
  function.
- `RecurProgressStack::check_iteration_item` (rule 8), called by the guest before
  `advance_tile_iteration`, on the iteration's item — argument 0, the `RecurInput`, found by
  position since the driver records it under the parameter's own name:

  | fact | frame / journal | item selection |
  | --- | --- | --- |
  | which list | `source` | coordinates + commitment + path minus its last segment |
  | where it sat | `consumed_total` | `ListRange.start` (chunked) / `List.index` (unchunked) |
  | source length | `L` | `ListRange.len` / `List.len` |
  | how much | `consumed_elements` | payload element count; the `Range` segment's `end` held to it |

  The last segment must be `Range` for a site whose CFS declares `chunk`, a literal `Index`
  otherwise. A `Range` segment is pinned to its proof step by `start` only
  (`step_proves_segment`), which is why the width comes from the payload.

*Verified.*

- `raster-core`: the two PoCs are inverted into regressions (`a_sweep_that_rereads_the_first_chunk_is_rejected`
  — rejected at iteration 1, `ItemOutOfPlace { expected: 2, actual: 0 }`; the element-sweep twin),
  plus one test per fact (other object, other path, `len ≠ L`, width ≠ journal, `end` ≠ payload,
  wrong mode).
- Guest: the window-seeding fixtures now carry an honest item; a seeded window re-reading chunk 0,
  and an iteration with no item, are rejected through `advance_recur_progress`.
- Real traces: `hello-tiles` fraud proofs in `RISC0_DEV_MODE` (the real transition guest executes)
  over windows holding `[11,1]` (chunked, window opening after the site `Start` — the source
  identity arrives through the seed), `[16,1..2]` and `[18,1..2]` (unchunked) all verify. Negative
  control: with the expected start off by one, all three are rejected at those iterations.
  Wider windows hit two known blockers before any recur iteration —
  [`fraud-evidence-storage-unavailable`](../issues/fraud-evidence-storage-unavailable.md) (draft
  coordinates) and [`sequence-scope-forbids-narrowing`](../issues/sequence-scope-forbids-narrowing.md)
  (inside recur sequence `[15]`).

*Recur sequences (same day).* An iteration `Start` `[s, i]` runs the same
`check_iteration_item` (never chunked, one element) on the body's parameter 0, the
`RecurSequenceInput`: its recorded value is an inline marker, and its storage data sits under the
parameter's name. A `Start` is a sequence boundary with no storage roots, so `checks::store` folds
**none** of its witnesses — the check verifies the item's witness itself (reference or payload
form) before reading its proof. The object is authenticated later, when a body tile reads the
item and `verify_sequence_scope_parent` ties that read to this binding.

- Guest: an honest two-element sweep with real proofs, a re-read element, a witness that does not
  fold, and an item from another object.
- Real trace: `hello-tiles` windows holding `[15,1]` and `[15,2]` pass rule 8; with the expected
  start off by one they are rejected there.

*Recur-sequence bodies could not be fraud-proven — fixed the same day.* The iteration `Start`
records `RecurSequenceInput` as an inline handle (`{kind, index, len, item}`), while a body tile
reading the item (`into_ref!`) records the storage binding itself. The scope check compared the
two by kind and panicked — "Sequence scope source kind does not match consumer binding" — on
every honest body. It now looks through the handle for parameter 0 of a recur-sequence iteration
frame (`recur_sequence_item_source`): the handle must decode with `kind ==
"raster::RecurSequenceInput"`, and the scope source is the item's storage entry — the one rule 8
held to the source at that `Start`. So the chain is: body tile's store-verified read = iteration
item = element `consumed_total` of the site's source.

- Guest: a body tile reading its own element is accepted, another element is rejected, a scope
  value that is not a handle is refused.
- Real trace: windows `[15,1..]` (size 2), `[15,2..]` (sizes 2 and 4) now produce fraud proofs;
  all four windows tried failed before.
- The fourth window also holds the site `Start` `[15]` and fails on a different, pre-existing
  mismatch: the CFS binds `output = sequence_output` — a draft produced by the previous tile — as
  a storage input, while the trace records the draft replay handle inline ("Expected storage input
  source … arg 1"). Any window containing a recur site `Start` whose `output` draft came from an
  earlier step hits it. `incremental-draft-materialization` (drafts never cross a step boundary)
  removes the case; not fixed here.

*The site `Start`'s `L` was unverified — fixed 2026-09-30 (option (a)).* `authenticated_source_len`
said `checks::store` had folded the `"input"` metadata witness; it had not — the `Start` is a
`SequenceStart`, which carried no storage roots, so nothing read its object or folded its witness,
and the CFS check pins coordinates only. Rule 8 cross-checks `L` against the first iteration's
proven list length, so the hole was the empty sweep: a `Start` naming a fabricated empty list at
the right producer, followed by zero iterations, passed rule 7. Demonstrated first by a guest PoC
(`poc_a_site_start_claiming_an_empty_source_is_accepted`), now inverted.

- **Format.** `StepKind::SequenceStart` gained `storage: Option<StorageRoots>` — `Some` exactly at a
  recur site `Start`, read-only (`root_before == root_after`), the way `ProgramEnd` claims roots
  for its read. *Moved the same day* into `RecurStartStep.storage` (non-optional) with
  `incremental-draft-materialization`'s batch A; the guest's "must read its source" and "only a
  site start may claim roots" rules became type-level and were deleted with their tests. `StepRecord::storage_roots()` returns it. The recorder stamps the current roots
  (`storage_roots(None)`); `prove.rs`'s read-witness gate is `storage_roots().is_some()`, so the
  `Start`'s reads are built with no other change.
- **Guest.** Nothing new in `checks::store`: with roots, the `Start` takes the existing path —
  `root_before ==` the carried root, `verify_storage_read_witness` (the object is in the store),
  `commitment == selection.source_root_hash`, and `verify_selection_witness` on the `0x0A` payload.
  `advance_recur_progress` requires the roots where the site frame opens ("must read its source
  from storage") and forbids them on every other `SequenceStart` ("Only a recur site start may
  claim storage roots"). Because the record pins the roots, a window opening **on** the `Start`
  is anchored like any `Exec` step.
- **Tests.** Guest: a `Start` without roots; a fabricated empty source with roots (no read witness
  matches it); the real object with doctored metadata (the fold fails); an honest non-empty and an
  honest empty source (`L = 0` is authenticated, not forbidden); roots on an ordinary boundary.
- **Real traces.** `hello-tiles` fraud windows (dev mode) containing each recur tile site `Start`
  — `[11]`, `[12]`, `[16]`, `[18]`, window sizes 2 and 4, several opening on the `Start` itself —
  produce proofs. Negative control: with the guest's roots requirement inverted, every one fails
  at its site `Start`, so the read ran on each.
- **Moves** into `RecurStartStep.storage` when `incremental-draft-materialization`'s step kinds
  land; that proposal is corrected to match.
- **Cost.** A trace-format break (every trace hash; old commitments must be re-recorded) and the
  transition guest's image id. No tile id, no `program_commitment`: the locks still verify.

*Not covered.* §2b is untouched: the bytes a tile ran on are still not the bytes the selection
proves — step 2.

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

**Done 2026-10-01**, in batch C.

- `TileReplayJournal.output_root` — `Some(value_root(result))` exactly when the tile publishes an
  output; `transition` guest `checks::io::verify_output_root` requires `output_commitment` to equal
  it, or to be empty when `None`; `checks::store` requires a step with an empty commitment to write
  nothing and refuses a write witness on a non-writing step kind. Together: a tile writes iff its
  replay produced an output, and writes exactly that root.
- `TileReplayJournal.input_roots` — the raster root of each decoded argument (a recur item's
  value, a carried state's inner value, `None` for a draft handle), captured between decode and
  call. `checks::io::verify_input_roots` requires every storage-bound argument's selected payload
  root (`payload_structural_root`, the root its selection proof folds from; `selected_root` for a
  reference witness) to equal it, and the root count to equal the recorded argument count.
- **Recorder parity for outputs** is by construction rather than a recomputation: the native
  wrapper's stored payload and the replay's `output_root` come from the same encoder
  (`raster_core::tree`), and an encoder disagreement fails the honest run's fraud windows, which
  were run (see the draft proposal's batch C notes). A host-side recomputation of `root_hash` from
  payload bytes was not added.
- Measured on real windows: every storage-bound argument of every tile replayed in the
  `hello-tiles` windows run — strings, structs, a stored carried state, recur items and `chunk = 2`
  blocks — agrees with its selection's root.

### Step 3 — verify and regenerate

Invert both issues' probes (a forged output commitment; the re-read sweep), replay real traces
host-side as for `program-output-unbound`, run the proving tests (GPU), and regenerate the locks
with `cargo raster build --backend risc0` (the tile cache key now tracks raster sources).

## Order

Steps 0 and 1 are done. The rest, and `incremental-draft-materialization`:

Decided 2026-09-30, with `incremental-draft-materialization` implemented **in full** alongside
this proposal. Batches are grouped by what they break: a *trace-shape* break re-records traces; a
*tile-image-id* break rebuilds every lock.

| batch | contents | breaks |
| --- | --- | --- |
| **A — trace shape** | `RecurStart`/`RecurEnd` step kinds, site closes at `[-s]`, `ExecTarget::RecurTile`/`RecurSequence` removed; D4 (nested `SequenceEnd` at `[-s]`); `RecurStartStep.storage` (read-only) takes over this proposal's `SequenceStart.storage`, which is removed again; site-ordering checks | trace shape, transition guest; **done 2026-09-30** — also moves the image ids of tiles that link the edited `raster-core` code |
| **B — language and object ownership** | §One storage rule and §The restriction: `new!`/`finalize`/`finalize = false` removed, a site owns its object at `[s]` (creates, or derives with `output = base`), plain tiles return values, `DRAFT_NAMESPACE` deleted, host anchor `anchor_for_schema([s], S)`; D1b (`RecurOutputDecl` in the CFS via `schema_walk`); programs rewritten (`examples/`, `crates/raster/tests`, `raster-inference`) | CFS, programs, `program_commitment` |
| **C — tile journal** (one tile-image-id break) (**done 2026-10-01**) | D3 (recur iterations publish no output; an `Exec` with no write has an empty `output_commitment`); D2 (`draft_id` removed); D1a (replay tile asserts its schema); D5b (replayed state as raster roots, recur-sequence state by reference); **this proposal's step 2** (`output_root`, `input_roots`); guest: the frame's draft entry replaces `active_drafts`, `RecurEnd` writes exactly one object | every tile image id, `program_commitment` |
| **D — materialization** | `DraftBuffer`, one seal for store and `RecurEnd` output, `rindex04` relative offsets, derivation sharing (`Derived` backing, delta output) | `.rindex` format; no guest or tile change |
| **E — verification** | both proposals' Verification lists; this proposal's step 3; GPU proving; all locks, `raster-inference` included | — |

What implementing the draft proposal in full changes here:

- Step 2's write rule needs no exceptions: a plain tile returns a value (`output_root = Some`), a
  recur iteration never writes, and `RecurEnd`'s one write is bound by the frame (draft root or
  D5b state commitment), not by a journal.
- D2 is not deferred: with no draft crossing a step boundary, the frame's draft entry replaces
  `active_drafts`, the only reader of `draft_id`.
- D3 covers recur iterations only; plain `Draft`-returning tiles no longer exist.
- The draft-argument binding mismatch (CFS says storage, trace says an inline handle — the `[7]`
  and `[15]` fraud windows) disappears with batch B.

Open: whether batch B verifies under today's guest on its own. If not, B and C merge. Checked
first, on one rewritten recur site in `hello-tiles`.

## Costs

Step 1 adds a field to `RecurProgressFrame`, so every `recur_progress_commitment` changes and
traces must be re-recorded; it moves the transition guest's image id and no tile's. Step 2 moves
every tile image id, hence every `program_commitment`, and grows each replay journal by one root
per argument plus one for the output.
