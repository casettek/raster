# Proposal: `storage-ledger-checkpoints` — storage as of a past step, by rewinding instead of rebuilding

Status: **Implemented 2026-11-06 — measured.** One deviation from the design below: the journal
is not inside `IncrementalCoordinateIndex` but in a host-only wrapper (§Implementation record).

Related:
- [`live-recorder-shared-objects`](./live-recorder-shared-objects.md) §Next step — proposed `StorageLedger::as_of(n)` by rebuilding from a list of writes. This proposal answers the same question by rewinding.
- `5f46131` (live audit builds evidence only at a divergence) — created the need: the storage state a window opens with is now produced at the divergence, after storage has moved past it.

## Problem

A fault window's storage witnesses must verify against the storage roots **as they were at each window step**. In live audit, the divergence is found at step D, when the ledger already holds storage after D, including the writes the window's own steps made. So at the divergence the code needs storage as of the window's first step S.

Today it **rebuilds** that state: the audit publisher keeps a list of every write (`AuditPublisher.writes`), and `ProofStorageState::rebuilt` replays all `w_S` writes into an empty log tree and an empty coordinate index. That costs one index insert (~7 µs) and one log append per write, so it grows with the length of the run: ~0.5 s for ~60,000 writes on hello-tiles 20k, and proportionally more on longer runs.

The offline prover does the same work differently: `ProofStorageState::for_recorded` walks the whole trace prefix (`advance`). Its log is a `DiskVec`, and each log witness rebuilds a tree over every entry (`ProofLog::Recorded`).

Only the writes **inside the window** separate "now" from the window start, and a step makes at most one storage write, so there are at most W of them.

## Idea

Give the two storage structures a checkpoint, so the ledger can step back:

| Structure | Has look-back today? | What this proposal adds |
| --- | --- | --- |
| Coordinate index (`IncrementalCoordinateIndex`, `raster-core`) | no | an **undo journal** of the last K inserts; `rewind_to(writes)` restores the exact earlier nodes |
| Storage log | the ledger keeps only a frontier | the ledger keeps the log's **leaf hashes** (32 B per write); a past log tree is **bulk-built** from a prefix of them |

The storage log's tree type, `bridgetree::BridgeTree`, already has `checkpoint`/`rewind`/`witness(position, checkpoint_depth)`. It isn't used for this for two reasons:
- its witnesses need leaves marked in advance, and marking every leaf is slow, which is why `MemoryWitnessTree` exists;
- the ledger keeps only the tree's frontier.

Bulk-building a `MemoryWitnessTree` from leaf hashes is cheaper than either: about one hash per node, ~2n hashes in total. Incremental appends cost 32 hashes each.

## Design

### 1. Undo journal in `IncrementalCoordinateIndex` (`raster-core`)

An insert changes exactly four things (`coordinate_index.rs`, `insert`):
1. a new `entries` key;
2. a new `leaves` key;
3. the `nodes` on the key's path from the root down to where it splits from its neighbour, each new or overwritten;
4. `root`.

`insert` is the index's only mutating method. The journal records, per insert, the coordinates and hashed key (to remove the entry and the leaf), each touched node with its **previous value** (`Option<Hash32>`, `None` for a new node), and the previous root. That's about one node per path level, roughly 20 at 60,000 entries, so about 1.5 KB per insert.

```rust
impl IncrementalCoordinateIndex {
    /// Keep undo records for the last `inserts` inserts (0 = off, the default).
    pub fn retain_undo(&mut self, inserts: usize);
    /// Number of inserts so far.
    pub fn len(&self) -> usize;
    /// Undo inserts until `len() == n`. Panics if the journal no longer covers n.
    pub fn rewind_to(&mut self, n: usize);
    /// Whether `rewind_to(n)` is possible.
    pub fn can_rewind_to(&self, n: usize) -> bool;
    /// Entries in insertion (log) order, for a rebuild when the journal does not cover n.
    pub fn entries_by_position(&self) -> impl Iterator<Item = (&CfsCoordinates, &StorageIndexValue)>;
}
```

Rewinding restores the exact previous bytes, so it needs no tree-merging logic: correctness doesn't depend on getting a deletion algorithm right. The journal is a ring of the last K records, so memory stays bounded by K whatever the run length. With journaling off it costs nothing.

`entries_by_position` lets the fallback rebuild (offline, early faults) read the index's own entries in log order, so the ledger doesn't need a separate list of writes. Each entry already carries its `log_position`; the method sorts by it.

### 2. Bulk-built log tree (`raster-prover`)

```rust
impl MemoryWitnessTree {
    /// The tree over `leaves`, built level by level: about one hash per node.
    pub fn from_leaves(leaves: &[[u8; 32]]) -> Self;
}
```

It must equal the tree incremental `append`s produce: the same `levels`, so the same `root`, `witness` and `frontier_at`.

### 3. `StorageLedger` answers "as of n writes" (`raster-runtime`)

The ledger adds `log_leaves: Vec<[u8; 32]>`, the seed and every entry's leaf hash. That's 32 B per write, replacing the 100–150 B per write of `AuditPublisher.writes`.

```rust
impl StorageLedger {
    pub fn writes(&self) -> usize;
    /// Journal the last `writes` index inserts (live audit: the window size).
    pub fn retain_undo(&mut self, writes: usize);
    /// Storage as it was after the first `n` writes. Rewinds the index when the
    /// journal covers n, rebuilds it otherwise. The log tree is bulk-built from
    /// `log_leaves[..=n]`. Changes the ledger: a caller that needs the current
    /// state afterwards walks forward with `LedgerAt::apply`.
    pub fn as_of(&mut self, n: usize) -> LedgerAt<'_>;
}

/// Storage at one moment, moving forward one write at a time.
pub struct LedgerAt<'a> { /* index (borrowed from the ledger), log tree */ }
impl LedgerAt<'_> {
    pub fn initial(&self, with_entrypoint: bool)
        -> (SerializableFrontier, Vec<u8>, Option<StorageReadWitness>);
    // used by build_step_io: membership and non-membership proofs, log witnesses
    pub fn apply(&mut self, write: &StorageWriteRecord);
}
```

`LedgerAt` takes over the role and methods `ProofStorageState` has today. `build_step_io` takes `&mut LedgerAt`.

### 4. Callers

- **Live audit** (`publishers/audit.rs`):
  - At construction, `ledger.retain_undo(window_size)`. Each step makes at most one write, so a window can never need more.
  - A window entry keeps only `writes_before`; `writes` and `frontier` leave its state.
  - At a divergence: `let mut storage = recorder.ledger_mut().as_of(first.writes_before);`, then `initial()` and `build_step_io` per window step, as now.
  - The process exits after the handoff, so leaving the ledger rewound is fine.
- **Offline prover** (`commands/prove.rs`):
  - `as_of(writes_before_window)` on its own ledger. The journal is off there, so it rebuilds from `entries_by_position`. That replaces both the prefix walk and `ProofLog::Recorded`'s per-witness tree rebuild.
  - The number of writes before the window comes from a prefix count, without applying them. Whether the recorder can record it per step instead is decided when implementing.
- **Removed:** `ProofStorageState`, `ProofLog`, `ProofStorageState::{rebuilt, for_recorded, advance}`, and `AuditPublisher.{writes, frontier}`.

### 5. The test oracle stays independent

`live_commitment_cli` compares the live evidence with a reference built from the saved trace. If the reference also called `as_of`, it would compare the code with itself. So today's prefix-walk logic moves into the test as a small, separate reference implementation, independent of the production path.

## Plan

0. **Baseline.** Measure on hello-tiles 20k: live-audit time from detection to handoff (late fault, `--fraud-step 130000`), peak RSS, and offline `trace audit` evidence-preparation time. Run these back to back with the change; the machine's speed varies between days.
1. **`raster-core` journal and `entries_by_position`.** Property test: random inserts with random rewinds and re-inserts; after each operation, compare the root and membership/non-membership proofs for every key against an index freshly built from the same entry set. With journaling off, behaviour and memory are unchanged.
2. **`MemoryWitnessTree::from_leaves`.** Test against incremental appends: `levels`, `root`, `witness`, `frontier_at` at every size up to ~1,100.
3. **`StorageLedger::as_of` and `LedgerAt`.** Unit tests: `as_of(n)` by rewind and by rebuild give the same `initial()` and the same proofs; walking forward from `as_of(n)` to the current count gives back the current roots.
4. **Live audit switch** and removal of the publisher's write list. Acceptance: `live_commitment_cli` byte-identical, the real-run handoff canonically equal (`cmp_handoff`), `fault_estimate_cli` passes.
5. **Offline prover switch** and removal of `ProofStorageState`/`ProofLog`, with the oracle moved into the test.
6. **Re-lock and check identities.** `raster-core` changed, so every `Raster.lock` gets new `guest_inputs` (the raster examples and Iris). Rebuild guests and confirm the tile and protocol image IDs are unchanged. Guests don't use `IncrementalCoordinateIndex`, so they shouldn't move, but verify.
7. **Measure** as in step 0.

## Expected effect (to be measured)

- **Live audit at a divergence:** the index rewind costs ≤ W undos instead of `w_S` inserts. The log tree bulk build is ~2n hashes; that's still O(n), but at about one hash per node instead of 32 per write plus an index insert.
- **Live memory:** the 100–150 B/write write list becomes 32 B/write of leaf hashes, plus a bounded journal (~W × 1.5 KB, ~200 KB at W = 128).
- **Offline proving:** the per-witness log-tree rebuild disappears; the index rebuild stays O(n), once per window.
- **Unchanged:** evidence bytes, guest, commitments, trace format.

## Risks

- **A missed mutation.** Only `insert` mutates the index today, and every future mutator must also write the journal. A debug assertion comparing the root after `rewind_to` with a recorded root at that count would catch a gap in tests.
- **Rewinding beyond the journal.** `as_of` falls back to a rebuild whenever `can_rewind_to` is false, never silently.
- **The log stays O(n) at a divergence.** If the bulk build ever matters (very long runs), the log could keep a live `MemoryWitnessTree` and truncate it instead, at 32 hashes per write on every run. Measure first.
- **Re-lock.** A one-time cost across all projects, including Iris.

## Implementation record (2026-11-06)

**Where the journal lives.** As designed, inside `IncrementalCoordinateIndex`, the change moved
`TRANSITION_GUEST_ID` and `DEFENCE_GUEST_ID`. The transition guest builds an index for the empty
root (`coordinate_index_root`), so the type's code is in its image. Measured by building the
protocol guests at `b201b0a` and after each attempt:

| attempt | protocol guest ids |
| --- | --- |
| journal fields and code in `IncrementalCoordinateIndex` | transition and defence **moved** |
| same-line visibility edits only (`pub(crate)` on four fields, `prefix`, `deepest_shared_prefix`) | **identical** |
| plus a separate journal module compiled with `std` (which guests enable) | transition and defence **moved** again, though guests never call it |
| plus that module behind a new `journal` feature that only `raster-runtime` enables | **identical** (all four) |

So the journal is `raster_core::coordinate_index_journal::JournaledCoordinateIndex`. It wraps the
index, finds the nodes an insert will write through the index's own split logic, records their
previous values, then calls the unchanged `insert`. It sits behind `raster-core`'s `journal`
feature. Guests resolve `raster-core` with their own manifests (`features = ["std"]`), so the
feature never reaches them. **Any future `raster-core` addition the guests compile can move these
ids, even unused code; check them, don't assume.**

**What landed:**
- `JournaledCoordinateIndex` (`retain_undo`, `rewind_to`, `can_rewind_to`, `entries_by_position`).
  Its property test checks that a rewound index equals a freshly built one: nodes, leaves, entries,
  root, membership and non-membership proofs.
- `MemoryWitnessTree::from_leaves`, tested equal to incremental appends at every size up to 1,100.
- `StorageLedger` keeps the log's leaf hashes and offers `writes`, `retain_undo`, `as_of` (rebuild,
  non-mutating) and `rewind_to` (journal, falling back to `as_of`).
- `LedgerAt` replaces `ProofStorageState` and `ProofLog`.
- `build_step_io` splits into `build_step_selections` and `build_step_storage`. Live audit builds
  every window step's selections *before* rewinding, while the ledger can still check reads against
  objects the window wrote, then rewinds and builds the storage proofs.
- The offline prover uses `as_of` instead of the prefix replay; `ProofLog::Recorded`'s per-witness
  tree rebuild is gone.
- `live_commitment_cli` keeps an independent reference: an empty `LedgerAt` plus the saved trace's
  prefix writes.

**Re-lock.** The six example `Raster.lock` files changed only in `guest_inputs`; no tile id or
program identity moved. Iris's locks need the same re-lock.

**Measured** on hello-tiles 20k, child process run directly, back to back with `b201b0a`:

| | `b201b0a` | after |
| --- | --- | --- |
| late fault, wall | 6.51–6.66 s | 6.12–6.51 s |
| late fault, peak RSS | 200–201 MB | **165 MB** |
| honest, wall (6 interleaved runs, mean) | 6.17 s | 6.26 s |
| honest, peak RSS | 166–168 MB | 160–162 MB |

The late-fault audit no longer replays ~60,000 writes, and no longer holds the second index that
rebuild created (the −35 MB). The honest run pays the journal on every write, about 1.5% here; a
journaled insert that records previous values in the same pass, instead of a lookup before the
unchanged `insert`, would cut that, at the cost of a second, host-side copy of the insert
algorithm. Evidence is unchanged: `live_commitment_cli` byte-identical, the real-run late-fault
handoff canonically equal and the honest handoff byte-identical. 585 tests pass (`raster-cli`,
`raster-prover`, `raster-runtime`, `raster-guest-build`, `raster-core --features journal`, ignored
`fault_estimate_cli`).

