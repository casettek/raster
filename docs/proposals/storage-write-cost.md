# Proposal: `storage-write-cost` — a storage append costs 200–360 µs, and almost none of it is the append

Status: Proposed 2026-09-21. **Measured, not estimated** — see §Measurement.

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

## Term 1: the roots are recomputed from a cloned frontier, twice per append

```rust
fn frontier_root(frontier: &TraceTreeFrontier) -> Vec<u8> {
    TraceTree::from_frontier(1, frontier.clone())   // clone the frontier, rebuild a tree
        .root(0).expect("storage root should exist").0
}

pub fn current_root(&self) -> Vec<u8> { frontier_root(&self.frontier) }
```

`append` calls `current_root()` **twice** — for `store_root_before` and `store_root_after` — and
additionally clones the frontier for `frontier_after`. Three clones and two tree rebuilds per
write, ~110 µs.

`store_root_before` is, by construction, the previous append's `store_root_after`. It never needs
computing at all.

**Fix.** Cache the current root and index root on `StorageManager`, recompute once after a
mutation, and serve `*_before` from the cache. No format change, no semantic change, no witness
change: the same bytes are produced. Expected to remove ~30–55% of every storage write.

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
