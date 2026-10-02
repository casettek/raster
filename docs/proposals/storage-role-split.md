# Proposal: `storage-role-split` — the running program does not need an authenticated store

Status: **Implemented 2026-09-24.** Proposed 2026-09-22. **Measured, not estimated** —
§Measurement, and §Implementation record for what the measurement turned out to be worth.

Related:
- [`storage-write-cost`](./storage-write-cost.md) — **supersedes its Term 2 as the next move.**
  That proposal measured the coordinate index at 93% of a storage append and proposed shaving its
  constant (Term 2a) or redefining its root (Term 2b). This proposal observes that *half of those
  appends compute an index nobody reads*, so the cheaper action is not to build it there. Term 2
  remains correct and still applies to the half that is real.
- [`incremental-draft-materialization`](./incremental-draft-materialization.md) — its
  `large_draft_finalize_scaling` establishes the regime split this proposal lives in: the fixed
  per-write cost matters below ~80 elements and vanishes above it. This proposal is entirely about
  that fixed cost.
- [`unauthenticated-execution`](./unauthenticated-execution.md) — adjacent and *not* the same
  thing. That proposal removes authentication from a **posture** the user selects (`--no-auth`),
  and says so in the output. This one removes work that is unused in the **authenticated** posture,
  with no change to what is committed or checked.

## Problem

A `cargo raster run` is two processes. The CLI spawns the compiled user program as a child with
piped stdio (`raster-cli/src/commands/run.rs:183`) and reads its trace events back.

Each process has a `StorageManager`, and they have different jobs:

| | child process (the user program) | CLI (`TraceRecorder.storage`, `recorder.rs:297`) |
| --- | --- | --- |
| how it is filled | the program writes as tiles run | rebuilt from trace events (`recorder.rs:884`, `:963`, `:1085`, `:1163`) |
| what it is for | serving reads to the running program | producing the commitment and the evidence |
| reads | `resolve` / `select` → `verify_reference` (`storage.rs:607`) | — |
| roots in the trace | — | `exec_step` (`recorder.rs:451-454`) |
| membership proofs | — | `run.rs:925`, `:1122`, `:1154`, `:1158`, `:1189` |
| `selection_witness` | — | `recorder.rs:392`, `:636`; `run.rs:960`, `:1214` |

They are the same type, so the child pays for the second column's machinery and uses none of it.

### What the child computes and discards

`StorageManager::append` returns a `StorageWriteRecord` with seven fields. Every child-side call
site keeps exactly one of them:

| call site | uses |
| --- | --- |
| `store_value_at_coordinates` (`storage.rs:1299`) | `entry.object_commitment` |
| `store_execution_output_value` (`storage.rs:1416`) | `entry.object_commitment` |
| `load_authorized_sources` (`entry_arguments.rs:146`) | `entry.object_commitment` |

So `store_root_before`, `store_root_after`, `index_root_before`, `index_root_after` and
`frontier_after` are computed on every child write and dropped. Producing them costs, per write, a
Merkle frontier append plus **a 256-level coordinate-index insert** — 256 SHA-256 hashes, 256
`Vec<u8>` allocations and 256 `HashMap<NodeKey, Vec<u8>>` inserts
(`raster-core/src/coordinate_index.rs:88-98`, `INDEX_BITS = 256`).

Nothing else in the child reads them:

- **Reads do not.** `verify_reference` (`storage.rs:607`) — the gate on every `resolve` and
  `select` — looks only in `self.objects: BTreeMap<CfsCoordinates, StoredObject>`. It touches
  neither the frontier nor the index.
- **The child sends no roots.** `FnOutput { data, ty, raster }` (`raster-core/src/trace.rs:243`)
  has no root field, and the recorder fills `StepRecord.storage` from **its own** write
  (`recorder.rs:451-454`). The child's roots never reach a commitment.
- **`global_storage_snapshot()` (`storage.rs:1038`) has zero callers** in the workspace. It is
  defined and re-exported (`lib.rs:43`) and called by nothing. (`StorageSnapshot` itself is *not*
  dead — the recorder uses it at `recorder.rs:423` for steps that did not write.)
- **Chain mode does not.** `raster-cli/src/chain.rs` reaches storage only through the recorder.

**`log_position` goes with them.** It is derived from the frontier —
`let log_position: u64 = self.frontier.position().into();` (`storage.rs:478`) — and stored on every
`StoredObject`, which looks at first like a reason the child must keep a frontier (or at least a
counter standing in for one). It is not: every reader of `log_position` is CLI-side, building the
storage log witness (`run.rs:848`, `:856`, `:927`). Nothing in the child reads it, so the field
leaves with the frontier rather than needing a replacement.

> **Stronger than stated, found at implementation.** `StoredObject::log_position` had **no readers
> at all** — not CLI-side either. The `run.rs` sites above read a *different* `log_position`:
> `StorageWriteRecord`'s (`:878`) and `StorageIndexValue`'s (`:927`, `:1135`, `:1202`), both of
> which stay on the recorder. So the field did not need the frontier removed in order to go; it
> was simply dead, and deleting it was a self-contained step.

### What the child does need, and why

Stating this positively, because the line between the two halves is not where the names suggest.

- **The `objects` map** (`BTreeMap<CfsCoordinates, StoredObject>`) — a sequence binds *references*,
  not values, so the bytes a later tile consumes have to be fetched back. `resolve` (`:575`) and
  `select` read through `verify_reference` (`:607`), which looks the coordinate up in this map.
  Without it the second tile in any program has no way to reach the first's output.
- **Reads** are that map used two ways: whole-value (`resolve`) and selector-path (`select`, for
  `select!(String, personal_data.name)`). The child stores data not to authenticate it later — the
  recorder does that from events — but because the next `call!` needs it materialized *now*.
- **Object commitments**, and this is the important one: **`entry.object_commitment` is alive and
  leaves the process, unlike the roots.** Two uses. It is half of a reference —
  `StorageRef::new(coordinates, object_commitment)`, coordinates saying *where* and the commitment
  *which value*, compared on every read (`:614`). And it is copied into the trace for every tile
  input:

  ```rust
  let storage = TraceStorageData {
      coordinates: resolved.reference.coordinates,
      commitment: resolved.reference.commitment,     // raster/src/input.rs:2019
      selector: resolved.selector,
      selection: resolved.selection,
  };
  ```

  The guest checks that field. So the child's *object commitments* reach the guest while the
  child's *storage roots* do not, and that asymmetry is the whole basis of this proposal. Anything
  that moves the commitment computation changes what is verified; removing the roots does not.
- **The duplicate-coordinate assert** — the one child-side consumer of the coordinate index:

  ```rust
  assert!(
      !self.coordinate_index.contains_key(&coordinates),
      "Duplicate storage write at coordinates {:?}", coordinates,
  );                                          // storage.rs:446
  ```

  It is needed because `self.objects.insert` (`:491`) overwrites: two writes to one coordinate
  would silently replace an object that outstanding `AuthRef`s still point at, surfacing later as a
  commitment mismatch at `:614` with nothing to say why. But it is a *correctness* guard, not an
  authentication one, so a `HashSet<CfsCoordinates>` serves it as well as a 256-level Merkle trie.
  (**No new set is needed at all** — see §Implementation record: `objects` already has the same
  key set, so `objects.contains_key` *is* the guard.)
  The index's other two uses in the file — `root()` (`:436`) and `insert()` (`:486`) — exist only
  to maintain a root the child never reads.

## Measurement

The profiling counters live in `StorageManager::append` and are emitted by `finalize_and_store`
(`storage.rs:1579`), which runs **in the child**. So every number already recorded in
`storage-write-cost` measures precisely the work this proposal removes.

`hello-tiles`, idle machine, `--features profiling`, medians over five finalizes, **after**
`storage-write-cost` Term 1 landed:

| child-side append, per write | |
| --- | --- |
| coordinate index | **66.4 µs — 93%** |
| storage roots | 3.4 µs — 4.7% |
| frontier append | 0.8 µs — 1.1% |

Same run, spread across the five: 51.8, 183.7, 66.4, **810.4**, 52.8 µs. The outlier has the shape
of a `HashMap` growth, which is itself a cost of a structure the child does not need.

**Expected effect.** Removing the index and frontier from the child removes ~94% of every child
storage write. On `hello-tiles` that is most of what remains of a draft close after Term 1 —
finalize's store half is currently ~96 µs, of which ~71 µs is the append and ~66 µs of that is the
index. It is **not** a win on large drafts: `large_draft_finalize_scaling` shows the whole fixed
append cost at 0.12 ms against a finalize growing at ~1.6 µs/element, so above ~80 elements this
proposal is noise and `incremental-draft-materialization` is the whole story. It is a win on the
many small writes an ordinary program makes — every tile output is one.

## Design

Split the role, not the process. The two cannot share one store — they are in different address
spaces, and the parent rebuilding from events rather than trusting the child is deliberate
(`recorder.rs:628-630`: *"so the recorded output commitment reflects committed storage rather than
a claim from the user process"*). What they can stop sharing is the *type*.

- **Child: a plain object store.** `objects: BTreeMap<CfsCoordinates, StoredObject>`, the
  commitment computation that produces `entry.object_commitment`, the read paths (`resolve`,
  `select`, `verify_reference`), the source resolver, and a `HashSet` for the duplicate-coordinate
  assert. No frontier, no coordinate index, no roots.
- **Recorder: `StorageManager` unchanged.** It keeps the frontier, the index, the roots and the
  proof construction, because it is the only thing that uses them.

The shared part is the object store and the commitment rule; the authenticated structure wraps it
on the recorder side only. `StorageWriteRecord`'s root fields become recorder-side.

### Why this changes no commitment

The child's roots are already absent from every commitment: the recorder computes
`StepRecord.storage` from its own write, and `FnOutput` carries no roots. So this deletes
computation whose results never left the child. That is the claim the verification below is
designed to falsify.

## Verification

- **Byte-identical `commit.bin`** on `examples/hello-tiles` across the change, plus
  `--audit` returning `Verification Success`. This is the same check that validated
  `storage-write-cost` Term 1, and it is the one that matters: if any child-side root did reach a
  commitment, the bytes move.
- **The fraud path still builds evidence.** `--fraud-step 22` reproduces
  [`authenticated-chain-draft-output`](../issues/authenticated-chain-draft-output.md) in seconds
  and exercises `build_storage_selection_witnesses`, which is recorder-side and must be untouched.
- **Re-measure `append_index_ns`** on the same fixture; it should go to zero for child writes.
- `cargo test -p raster-core --lib && cargo test -p raster-runtime --lib`, the transition guest's
  suite, and `examples/chain-example`'s `chain run`.

## Costs and risks

- **Two storage types where there was one.** The duplication is the point, but it is still a
  second thing to keep correct; the object-store half should be shared code, not a copy.
- **A latent intention may be being deleted.** `global_storage_snapshot()` and
  `TraceRecorder::storage_snapshot()` are both public and both uncalled, which suggests someone
  expected the child's roots to be used — most plausibly to cross-check the child against the
  recorder. **That cross-check does not exist today**, and if it is wanted it should be proposed
  on its own terms rather than preserved by accident. Worth one deliberate decision before the
  code goes.
- **`--stage` re-runs** were checked only by grep (`chain.rs` reaches storage through the
  recorder). A reviewer who knows that path should confirm.

## Not in this proposal

- The constant-factor and path-compression fixes to the index itself:
  [`storage-write-cost`](./storage-write-cost.md) Term 2a and 2b. They still apply to the
  recorder's index, which remains real work.
- Anything about what is committed, checked, or proven. This proposal removes unread computation
  and nothing else.

## Implementation record

Landed 2026-09-24 in three steps, each verified on its own before the next.

| step | change |
| --- | --- |
| 1 | `StorageManager` split into **`ObjectStore`** (objects + source resolver) and **`AuthenticatedObjectStore`** (which holds an `ObjectStore` plus the frontier, cached root and coordinate index). Pure refactor — the child still held the authenticated type. |
| 2 | `StoredObject::log_position` deleted (dead everywhere — see the note in §What the child computes and discards). |
| 3 | `THREAD_STORAGE: RefCell<ObjectStore>`. `ObjectStore` gained `append_serialized_bytes` / `load_authorized_sources` returning a `StorageEntry`; the three child call sites take `.object_commitment` directly; `global_storage_snapshot()` removed. |

**Naming.** `StorageManager` was renamed rather than kept: "Manager" named nothing, and the pair
`ObjectStore` / `AuthenticatedObjectStore` states the relationship the split is about — one is the
other plus authentication. `ObjectStore` follows its element type (`StoredObject`,
`object_commitment`), which is fixed by names the guest checks. Note the distinction from
*authorized* (`AuthorizedSource`): authorization is a permission the manifest grants before any
bytes exist; authentication is the structure that makes a value provable.

**The duplicate-coordinate guard needed no new structure.** `objects` and `coordinate_index` were
mutated in exactly one place each, in the same `append`, under the same coordinate — so their key
sets are identical by construction and `!self.objects.contains_key(&coordinates)` is an equivalent
guard at zero extra memory. It lives in `ObjectStore::put`. The authenticated store keeps its own
against the index, which `IncrementalCoordinateIndex::insert` asserts anyway.

**Shared code, not a copy.** §Costs and risks asked that the object-store half be shared. Two free
functions, `owned_backing` and `referenced_backing`, hold the backing-and-commitment construction
that both stores call, so `internal_object_commitment` keeps exactly one call site per backing
kind. `AuthenticatedObjectStore::append_serialized_bytes` went from 8 lines to 2 and
`load_authorized_sources` from 21 to 2.

**`global_storage_snapshot()` was removed, deliberately.** §Costs and risks asked for one
considered decision rather than preservation by accident. It went. If a child-vs-recorder
cross-check is wanted it should be proposed on its own terms; nothing was checking anything here.
`TraceRecorder::storage_snapshot()` stays — `storage_roots(None)` uses it.

### Measured result

`examples/hello-tiles` for commitments, `raster-inference`'s `prompt-prepare` for cost — 1997 exec
steps and 1275 tiles, a far better fixture than `hello-tiles`' 58. Release CLI and release program,
`--features profiling`, same fixture before and after step 3:

| per-tile phase, summed over 1275 tiles | before | after | |
| --- | --- | --- | --- |
| **`output_store_ns`** | **81.08 ms** | **3.01 ms** | **−96.3%** |
| `total_duration_ns` (all tiles) | 102.85 ms | 27.35 ms | −73% |
| `output_record_build_ns` | 10.81 ms | 11.48 ms | untouched |
| `storage_input_resolve_ns` | 5.01 ms | 6.54 ms | untouched |
| `user_duration_ns` | 0.56 ms | 0.61 ms | untouched |

The prediction was ~94% of a child append; measured, it is **96%**. Storing a tile output went
from the most expensive thing a tile does — 79% of tile time — to the fourth.

### What that is worth, in a whole run

**It is 0.6% of `prompt-prepare`'s wall clock, and the proposal should not be read as a performance
change.** The 94%/96% figure is 94% of *an append*, and this proposal never said what share of a
run that is. It is small. The full budget, release CLI and release program:

| component | time | share |
| --- | --- | --- |
| cargo no-op build check | 8.4 s | 67% |
| child program | 2.75 s | 22% |
| &nbsp;&nbsp;└ `input_trace_ns`, one `merge_round` recur sequence | 2.40 s | 19% |
| &nbsp;&nbsp;└ all 1275 tiles | 0.10 s | 0.8% |
| &nbsp;&nbsp;&nbsp;&nbsp;└ `output_store_ns` — *what this proposal removes* | **0.081 s** | **0.6%** |
| recorder (`trace_recorder.record`, 1997 events) | 1.02 s | 8% |
| total | 12.59 s | |

A wall-clock A/B of a step-3 prototype measured **−0.10%**, inside a ±1% run-to-run band. That is
the correct result, not a failed measurement.

Two things this surfaced, both far larger than anything in this proposal or in
[`storage-write-cost`](./storage-write-cost.md) Term 2, and neither owned by anyone:

- **`record()` runs whether or not a commitment was asked for.** `load_trace_from_file`
  (`run.rs:297`) is gated only by `--no-auth`; with no `--commit` and no `--audit` the whole
  authenticated replica is built and then consumed by a print loop that takes 3 ms. 1.02 s in
  release — and note 14.9 s with a *debug* CLI, which is what an unwary measurement reports.
- **`input_trace_ns` — 2.40 s in one recur sequence** against 6 ms of body. `prompt-prepare` passes
  its merge table and vocabulary as recur-*sequence* arguments precisely so they travel as
  `AuthRef`s and materialize nothing; 2.4 s of input tracing says that intent is not being met.

### Verification performed

- **Byte-identical `commit.bin`** on `examples/hello-tiles` after **each** of the three steps:
  `2afe19cb437fd82ec47961bf3375269dc5375861defb977c5d8ec581ac2f1bb0`, equal to a baseline built
  from restored pre-refactor sources. This is the claim §Why this changes no commitment makes, and
  it holds with the child reduced to a plain object store.
- `--audit` → `Verification Success` after each step.
- `cargo test -p raster-core --lib` (155), `-p raster-runtime --lib` (**67** — two added), the
  transition guest suite (92). No new warnings.
- **Two tests added**, for a gap step 3 creates: the running program now depends on
  `ObjectStore`'s duplicate guard, and only the authenticated store's was covered.
  `object_store_rejects_duplicate_coordinate_writes` and
  `object_store_round_trips_a_value_through_its_commitment`.

### Not verified, and why

- **`examples/chain-example` `chain run` fails** — stage 3, `Missing storage object at coordinates
  CfsCoordinates([-2147483648, 2])`. **Pre-existing**, confirmed by running the same command
  against restored pre-refactor sources: identical panic, identical coordinates, identical
  stage-2 hashes. It is [`authenticated-chain-draft-output`](../issues/authenticated-chain-draft-output.md).
- **`--fraud-step 22` audit not completed.** The injection reproduces exactly (index 22,
  `exec_index` 40, tile `concat_messages`, `CfsCoordinates([9])`), but the audit no longer fails
  fast at the witness build as that issue documents — on this tree it proceeds into proving and
  was still running after 15 minutes at 16 cores. §Verification above promises this check "in
  seconds"; that is no longer true and the check needs replacing with one that still exercises
  `build_storage_selection_witnesses` cheaply.
- **`--stage` re-runs** remain grep-only, as §Costs and risks said.
