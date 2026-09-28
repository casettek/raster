# Issue: `tile-output-commitment-unbound` — the object a tile step stores is never tied to the output its replay produced

Status: open 2026-09-28. **Soundness gap in shipped code.** Unowned; to be resolved together with
[`incremental-draft-materialization`](../proposals/incremental-draft-materialization.md), which
changes the same journal, the same store check and the same trace format (§Why together).

Related:
- [`selection-unbound-from-execution`](./selection-unbound-from-execution.md) — the **input-side
  mirror**. That issue: the bytes a tile ran on are never joined to the bytes its selection proof
  proves. This one: the bytes a tile produced are never joined to the object its step stores. The
  same shape of direction — structural roots committed by the tile guest — appears in both.
- [`program-output-unbound`](./program-output-unbound.md) — adjacent, not the same. That issue is
  about *which* stored object a reference names. This one is about whether a stored object is what
  its producing tile computed at all.
- [`recur-carried-state-unbound`](./recur-carried-state-unbound.md) — the same missing join for a
  recur site's carried state (chain commitment vs stored result).

## What happens

A tile step carries two descriptions of its output, and the transition guest checks each one
against something different — never against each other:

| field | what it is | what checks it |
| --- | --- | --- |
| `replay_journal.output_bytes` | the output the replayed tile binary produced (postcard bytes) | equals the recorded output witness bytes (`crates/raster-prover/guests/transition/src/checks/io.rs:103-107`) |
| `step_record.output_commitment` | the object commitment written into storage at the step's coordinates | **nothing** |

The three places that could join them do not:

1. **`verify_io_witness`** compares `output_commitment` with the hash of the output witness, but
   returns first for every execution step (`checks/io.rs:32-35`:
   `if step_record.is_execution_step() { return; }`).
2. **`verify_storage_transition`** receives the output witness as `_output_witness_bytes`
   (`checks/store.rs:143`) and never reads it. The expected storage entry is built from the record
   alone — `(step_record.coordinates(), step_record.output_commitment())` (`:296-304`) — and the
   write witness proves only that this entry was inserted.
3. **The no-write branch** (`checks/store.rs:326-335`) requires only unchanged storage roots.
   Nothing requires a tile whose replay produced output to write at all.

So the guest establishes *"the binary, on this input, produced bytes B"* and *"some object with
commitment C was inserted at this step's coordinates"*, and nothing establishes that C commits
to B.

The recorder does not compute C from the bytes either: `exec_step` takes it from the storage
write (`raster-runtime/src/tracing/recorder.rs:451-453`), which takes it from the `root_hash` of the
raster payload the child supplied (`internal_object_commitment`, `raster-runtime/src/storage.rs:255`).
An honest run is therefore also only as correct as the child's encoder, with no check anywhere.

## Why it is a soundness gap: false refutation of an honest commitment

The guest does not verify the committer's trace. A challenger submits a window of steps; the guest
verifies each one, and accepts the window as fraud if every item before the last matches the
committed fingerprint and the last diverges (`fraud_proof.rs:99-101`, `finalize` at `:765-800`).
A step record, `output_commitment` included, is hashed whole into its trace item
(`hash_trace_item`, `merkle_tree.rs:129`).

Take an **honest** commitment in which step `[3]`, a plain tile, stores its output under `C`.
A dishonest challenger:

1. takes the honest steps before `[3]` as the window's margin — they match the fingerprint;
2. submits `[3]` with the honest replay receipt and the honest output bytes — the I/O check
   passes;
3. sets `output_commitment = C'` for any `C' ≠ C`, with storage roots and a write witness for
   inserting `([3], C')`, all computable by the challenger — the store check passes;
   **or** drops the write and leaves the roots unchanged — the no-write branch passes.

`[3]` verifies, its trace item differs from the committed one, and the window is accepted as a
fraud proof against a commitment that was correct.

The converse still holds: a committer who stores `C'` at `[3]` is refuted by an honest challenger
proving the honest `[3]`, which also verifies and also diverges. The failure is therefore not that
lies go uncaught, but that **the guest accepts more than one version of a step**, so "valid and
divergent" stops implying "the commitment was wrong".

| actor | effect |
| --- | --- |
| honest committer | its result can be refuted by anyone, for any program with a tile that writes — essentially every program. Any bond attached to a commitment is exposed |
| consumers of an honest result | it is declared fraudulent: a denial of service on correct results |
| dishonest committer | no direct gain; still caught at its first divergent step |

## Reproduce

Measured 2026-09-28 with a throwaway test appended to
`crates/raster-prover/guests/transition/src/tests.rs` and removed afterwards. It is
`verify_storage_transition_uses_output_commitment_as_keyed_entry` with the commitment changed to
one unrelated to the output bytes, plus the I/O check:

```rust
let forged_commitment = sha(b"an object the tile never produced");
let new_entry = StorageEntry {
    coordinates: CfsCoordinates(vec![1]),
    object_commitment: forged_commitment.clone(),
};
let (mut before_frontier, root_before, _before_index, index_root_before) = build_storage_context(&[]);
let (_after_frontier, root_after, _after_index, index_root_after) = build_storage_context(&[new_entry.clone()]);
let step = tile_step_with_store_roots(
    1, new_entry.coordinates.clone(), Vec::new(), forged_commitment,
    root_before.clone(), root_after.clone(), index_root_before.clone(), index_root_after.clone(),
);
let witness = StorageWitness { reads: Vec::new(), write: Some(build_write_witness(&[], &new_entry)) };
// The replayed tile's real output, unrelated to the stored object:
let (_f, next_root, _i) = verify_storage_transition(
    &step, None, &BTreeMap::new(), Some(&b"out".to_vec()), Some(&witness),
    &mut before_frontier, &index_root_before,
);
assert_eq!(next_root, root_after);
crate::checks::io::verify_io_witness(&step, None, Some(&b"out".to_vec()));
```

Run with `cargo test probe_exec_write_commitment` in `crates/raster-prover/guests/transition`.
Result: **passes** — both checks accept a stored commitment unrelated to the output bytes.

Not built end to end: a full fraud window through `apply_verified_step` and `finalize`. The false
refutation follows from the two accepted checks plus `finalize`'s divergence rule, by reading.

## What it is not

- Not the replay itself: `env::verify` against the registry image id and the input binding
  (`checks/io.rs:95-113`) hold. The binary did produce B; the gap is after it.
- Not `selection-unbound-from-execution`: that is the input side, and fixing it leaves this open.
- Not closed by `incremental-draft-materialization`'s rule *"an `Exec` step without a storage
  write must have an empty `output_commitment`"* (§What actually happens during a sweep). That
  rule removes free bytes from a no-write record; it neither requires a write when the replay
  produced output nor ties a write to the output.
- Not specific to drafts. Every tile that writes is affected. Drafts make it matter more: under
  `incremental-draft-materialization` a deriving site opens at its base's stored commitment, so a
  base is exactly as trustworthy as this join.

## Why together with `incremental-draft-materialization`

- **Same journal break.** The shapes below add a field to `TileReplayJournal`, which moves every
  tile image id; that proposal already moves them (D1's schema assertion, D2's `draft_id`
  removal).
- **Same encoder requirement.** Its §Carried-state commitment (D5b) already requires the replay's
  raster root to equal storage's for every type, with one shared encoder in `raster-core`. The
  shapes below need exactly that encoder in the tile guest.
- **Same store check.** Its D3 makes draft- and state-returning iterations write nothing, and adds a
  rule for no-write records; the write-iff-output rule belongs in the same place.
- **Its soundness rests on this.** A deriving site's opening root is its base's stored commitment,
  and a state-only site's close compares against a stored commitment. Neither means anything if a
  stored commitment need not be what the producing tile computed.

## Directions

Shapes only; none is picked here.

- **The tile journal commits its output's object commitment.** The tile guest knows the output
  type, so it can compute the raster root with the shared encoder; the transition guest asserts
  `step.output_commitment == journal.output_commitment` when the step writes. Generic across every
  type, cheap in the transition guest. Moves every tile image id — the same cost
  `selection-unbound-from-execution`'s *"structural roots in the replay journal"* shape carries, so
  the two could share one break.
- **The transition guest recomputes the root from the output bytes.** Needs the output type in the
  transition guest, which decodes no postcard today (`paged-bytes.md:551-553`); a type-generic
  decoder or a raster-encoded output witness would be required. Keeps tile image ids but moves the
  witness format.
- **Either way, a write-iff-output rule**: a tile step writes exactly when its journal's
  `output_bytes` is non-empty. Without it, dropping the write stays a false-refutation path even
  after the commitment is bound.
- **Recorder parity.** Whatever the guest checks, `exec_step` should check at record time too, so a
  child encoder bug fails the run rather than producing a trace no honest challenger can defend.
