# Proposal: `storage-write-cost` — a storage append costs 200–360 µs, and almost none of it is the append

Status: Term 1 **implemented** 2026-09-21; Term 2 open. **Measured, not estimated** — see §Measurement.

Related:
- [`incremental-draft-materialization`](./incremental-draft-materialization.md) — found this while
  decomposing `finalize`, and is **blocked behind it**: the draft work removes 7–16% of a term that
  this document shows is itself 83–93% of the wrong thing.
- [`incremental-draft-witness`](./incremental-draft-witness.md) (implemented) — the same shape of
  finding one level up: an `O(N)` recompute per step replaced by an `O(log N)` frontier.

## Problem

Every storage write — every tile output, every `store_value`, every draft finalize — goes through
`StorageManager::append`. Decomposed:

| per `append` | `hello-tiles` | `prompt-prepare` |
| --- | --- | --- |
| **total** | **363 µs** | **194 µs** |
| `current_root()` ×2 + frontier clones | 116 µs — 32% | 107 µs — 55% |
| coordinate-index update (`INDEX_BITS = 256`) | 245 µs — 68% | 86 µs — 44% |
| **frontier append — the actual record of the modification** | **1.0 µs — 0.3%** | **0.7 µs — 0.4%** |

The operation that records the write is **0.3–0.4%** of the write. The rest is bookkeeping.

## Term 1: the root fold, twice per append — RESOLVED

Two separate defects sat behind one symptom. Both are fixed; the second was not what this
proposal originally claimed, so the original claim is kept below and corrected.

### 1a. Called twice per append (fixed: cache)

```rust
pub fn current_root(&self) -> Vec<u8> { frontier_root(&self.frontier) }
```

`append` called `current_root()` **twice** — for `store_root_before` and `store_root_after`.
`store_root_before` is, by construction, the previous append's `store_root_after`, so it never
needed computing. `StorageManager::cached_root` now serves it and one recompute happens per
mutation. Verified by the commit/audit round trip: byte-identical commit, `Verification Success`.

### 1b. Each call refolded the empty-subtree roots (fixed: memoize)

**The original claim here was that the cost was `TraceTree::from_frontier(1, frontier.clone())` —
a frontier clone and a tree rebuild. That was wrong.** Removing the clone and calling
`NonEmptyFrontier::root(Some(Level::from(32)))` directly changed the measured time by nothing.

The real cost is inside `root()`. It calls `H::empty_root(l)` once per level, and the `Hashable`
default (`incrementalmerkletree/src/lib.rs:671`) is an **unmemoized fold from level 0**:

```rust
fn empty_root(level: Level) -> Self {
    Level::from(0).iter_to(level).fold(Self::empty_leaf(), |v, lvl| Self::combine(lvl, &v, &v))
}
```

So one `frontier_root` at depth 32 did `0+1+..+31 = 496` hashes rederiving the same constants,
against ~32 hashes of actual spine — 94% waste. `Bytes::empty_root` now reads a `OnceLock` memo
built by that same fold (identical by construction; `empty_root_memo_matches_fold` guards it).

The same override is in the guest's `Bytes`, where those are proven cycles and `frontier_root`
runs at ~11 call sites per execution.

**Not `EMPTY_TRIE_NODES`.** That table's doc comment claims `Level N = Hash(level, empty[N-1],
empty[N-1])`, which is exactly this sequence — but it is not. A test written against it failed at
level 1, and none of `combine(l-1)`, `combine(l)` or a level-less `sha256(e ‖ e)` reproduces the
table. Only `EMPTY_TRIE_NODES[0]` is load-bearing here (it is `empty_leaf`, and matches the
guest's `EMPTY_LEAF`); what the rest of the table actually is has not been established and is
worth its own look.

### Measured

In-process A/B in release, against a twin type identical but for the override, with the two roots
asserted equal in the same test:

| | ns/call |
| --- | --- |
| trait default fold | 54 014 |
| memoized | 3 224 |
| | **16.8x** |

End to end on `hello-tiles`, median over the run's storage appends:

| | before | after |
| --- | --- | --- |
| `append_root_recompute_ns` | 52–56 µs | **3.2 µs** |
| `append_roots_ns` | 53–56 µs | **3.6 µs** |
| `store_append_ns` | ~170 µs | **56–60 µs** |

Term 2 (the coordinate index) is now the whole remaining cost of an append at ~51–54 µs.

## Term 2: the coordinate index is 256 levels deep

```rust
const INDEX_BITS: usize = 256;
...
for depth in (0..INDEX_BITS).rev() { ... combine_node_hash(...) ... }
```

A fixed **256 hashes per write**, independent of how many entries the index holds, and 256
siblings in every membership proof the fraud-proof guest verifies.

The depth follows from keying the index by a 256-bit hash of the coordinates
(`coordinates_key`). Whether the keyspace needs to be that wide is a design question this document
raises rather than answers — the tree is sparse and the number of live coordinates in any real
program is small. A shallower key, or a different index structure, would cut both host time and
guest witness size.

**Not proposed here**, because unlike Term 1 it changes the index root and therefore every
commitment. Worth its own argument; noted so Term 1 is not mistaken for the whole story.

## Measurement

`--features profiling`, `ProfileStreamEvent::DraftFinalize`, counters added 2026-09-21:
`draft_materialize_ns`, `draft_store_ns`, and the `append` split
(`append_roots_ns` / `append_frontier_ns` / `append_index_ns`). Read directly from
`target/raster/runs/<run>/profile.ndjson`.

Two programs, both authenticated native runs. The finalize path was instrumented first, so the
numbers above are drawn from draft finalizes — but `append` is shared by every storage write, so
the per-append split applies to all of them.

## Why this is worth doing first

It is the only item in this cluster that is **pure win**: no format change, no trace change, no
witness change, no semantic argument. It benefits every program and every storage write, and it
makes the draft proposals' numbers smaller and clearer to reason about.

## Verification

- Roots produced before and after the change must be byte-identical — the existing commit/audit
  round trip (`cargo raster run --commit` then `--audit`) is the check, since any root divergence
  breaks it immediately.
- Re-run the two programs with `--features profiling` and compare `append_roots_ns`.
- `examples/chain-example` and `hello-tiles` through rungs 3–5 of the authoring skill's ladder.
