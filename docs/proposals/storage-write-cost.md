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

Term 2 (the coordinate index) is now the whole remaining cost of an append at ~51–54 µs — **on a
small object.**

> **Scope corrected 2026-09-22.** What this proposal removes is *per storage write*, so it is flat
> in a draft's element count. `incremental-draft-materialization`'s `large_draft_finalize_scaling`
> measures the whole fixed append overhead at 0.12 ms against a finalize growing at ~1.6 µs per
> element — so Term 2 is **0.45% of closing a 16 K-element draft**, and the crossover with the
> per-element term is around **80 elements**. Term 2 is still worth doing, but it is a
> *small-object* optimization: it pays on the many ordinary tile-output writes a program makes,
> not on large drafts. Sequencing the two against each other by percentage requires saying which
> regime the program is in.

## Term 2: the coordinate index is 256 levels deep — now the whole cost of an append

With Term 1 done, this is **93% of a storage `append`** (66.4 µs of 71.6 µs, `hello-tiles` median)
and ~65% of a whole draft finalize. It is also the most variable thing in the profile: the five
finalizes in one run measured 51.8, 183.7, 66.4, **810.4** and 52.8 µs.

```rust
let mut current = leaf;
for depth in (0..INDEX_BITS).rev() {          // INDEX_BITS = 256, always
    let bit = bit_at(&key, depth);
    let sibling = self.child_hash(&key, depth, !bit);
    current = if bit { combine_node_hash(depth, &sibling, &current) }
              else    { combine_node_hash(depth, &current, &sibling) };
    self.node_hashes.insert(node_key(&key, depth), current.clone());
}
```

`coordinates_key` is `sha256(domain ‖ postcard(coordinates))`, so keys are uniform over 256 bits
and the tree is a full-depth sparse Merkle trie. Every insert walks all 256 levels.

### The 256 hashes are irreducible for this root definition

Worth stating because it rules out the obvious fix. Below the depth at which this key diverges from
every other occupied key, the *sibling* is the empty-subtree hash — but `current` never is, so every
one of the 256 `combine_node_hash` calls is a genuine hash of two distinct values. Precomputing
empty subtree roots (which `empty_hashes` already does) removes nothing, because no combine has
two empty children. There is no memo of the kind that fixed Term 1b here.

### 2a — remove the constant, change no commitment

256 SHA-256 over ~72-byte inputs is ~25 µs at the rate measured in Term 1b. The insert costs
52–66 µs. **So roughly half to two-thirds of this term is not hashing**, it is:

- a `Vec<u8>` allocated per level by `combine_node_hash`, plus `current.clone()` per level;
- a `HashMap<NodeKey, Vec<u8>>` insert per level, where `NodeKey` is `{ depth, prefix: [u8;32] }`
  hashed with the default SipHash — 256 map inserts and 256 33-byte key hashes per storage write;
- the 810 µs outlier, which has the shape of a map growth/rehash.

Switching node hashes to `[u8; 32]`, giving the map a cheap hasher (or replacing it with a
depth-indexed structure), and not re-inserting nodes whose value is unchanged are all local to
`coordinate_index.rs` and **produce the same root**. Verifiable exactly as Term 1 was: a
byte-identical `commit.bin` through the commit/audit round trip. Expected to roughly halve an
append again, to ~30–40 µs, with ~25 µs the floor.

### 2b — go below the floor, and move every commitment

Getting from ~25 µs to ~log2(N) hashes needs the root *redefined* as a compressed sparse Merkle
trie (collapse each all-empty subtree into a constant with an explicit skip encoding). That changes
`coordinate_index_root` for every program, so it moves `init_storage_index_root`, the membership and
non-membership proof shapes, and the guest checks that verify them. It is a real proposal with a
real payoff (~10x on top of 2a) and it should not be smuggled in as a performance tweak.

**Do 2a first.** It is worth ~2x, is verifiable byte-for-byte, and does not spend the compatibility
budget that 2b needs.

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
