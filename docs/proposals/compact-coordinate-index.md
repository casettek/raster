# Proposal: `compact-coordinate-index` — a coordinate index whose proofs are log₂(objects) deep, not 256

Status: **Proposed 2026-10-21.** Not implemented. The guest-side cost it targets is measured
(`storage-prover` §Where the storage guest's cycles go), and so is the host-side cost
(`storage-write-cost` Term 2). The cost after the change is estimated (§Cost). Step 0 measures the
one unknown, the actual depth distribution on real programs.

This is `storage-write-cost`'s **Term 2b**, which that proposal deferred as "a real proposal with a
real payoff … and it should not be smuggled in as a performance tweak", because it changes every
program's storage commitments.

Related:
- [`storage-prover`](./storage-prover.md): the storage guest, now about 70% of a fraud-proof
  window's proving segments (28 of 40, 19 of 28), and two thirds of it is this index.
- [`storage-write-cost`](./storage-write-cost.md): Term 2, the same index as 93% of a storage
  `append` on the host. This proposal is its 2b. Its 2a (same root, cheaper code) is subsumed:
  the new code is written cheap from the start.
- [`transition-segments`](./transition-segments.md): the transition guest's genesis,
  program-output and returned-state reads use the same membership proof and get the same saving.
- [`storage-role-split`](./storage-role-split.md): `AuthenticatedObjectStore`, the recorder that
  owns the index.

## Problem

Every stored object is indexed by its CFS coordinates in a **full-depth sparse Merkle tree**
(`raster-core/src/coordinate_index.rs`). The key is `sha256("raster-storage-index-key" ‖
postcard(coordinates))`, and a leaf always sits at depth 256 (`INDEX_BITS`). The tree's root is
`StorageRoots::index_root_{before,after}` in every storage-touching `StepRecord`, so it is in every
trace leaf and in `commit.bin`.

A program holds hundreds to a few thousand objects, so about 10 bits of a key are enough to tell
it apart from every other. The other ~246 levels are a single occupied path through empty
subtrees, and every proof walks all 256 of them anyway.

**Guest** (storage guest, default mode, after `storage-prover`'s fix 1):

| | `hello-tiles`, 32 steps | `prompt-prepare`, 16 steps |
| --- | --- | --- |
| storage guest, user cycles | 24.18M (28 segments) | 15.81M (19 segments) |
| index proofs (read membership; write non-membership + membership) | ≈16.5M (68%) | ≈10.4M (66%) |
| per proof | ≈289k cycles (256 levels × ~1,130) | ≈289k |
| frame decode, mostly the proofs' 256 siblings (8 KB a proof) | 4.59M | 3.19M |

**Host** (`storage-write-cost` Term 2): the index insert is 93% of a storage `append`, 52–66 µs
median with an 810 µs outlier, and the prover replays every insert again to build witnesses
(`ProofStorageState`, `commands/prove.rs`).

`storage-write-cost` §"The 256 hashes are irreducible for this root definition" shows that no
memoisation helps. Every one of the 256 combines hashes two distinct values. Only a different
root definition makes proofs shorter.

## Goal

1. Redefine the index root so that a key's leaf sits at the **shallowest depth where it is alone
   in its subtree**. Membership and non-membership proofs are then about log₂(objects) deep.
2. Let the guest **compute the index root after a write** from the write's non-membership proof,
   instead of verifying a second, membership proof against a host-claimed root.
3. Keep everything else: the key derivation, `StorageIndexValue`, the append log, the read and
   write rules, and which steps read and write.

Non-goals:
- No change to the append log (the 32-level frontier), which is a different tree.
- No change to the trace tree, the fingerprint, `StepRecord`'s fields or `commit.bin`'s format.
  Their *values* change, because the index roots inside records change (§What moves).
- No multiproofs across a segment's reads. That is §Deferred, and with shallow proofs it saves
  little.

## Design

### The tree

Keys are unchanged: `key(c) = sha256("raster-storage-index-key" ‖ postcard(c))`, 256 bits, read
most significant bit first. For a set of `(key, value)` pairs, the **compact index** is defined
recursively over the subtree at depth `d` with key prefix `p`:

```
node(d, p) =
    EMPTY                                   if no key has prefix p
    leaf(k, v)                              if exactly one key k has prefix p
    internal(node(d+1, p‖0), node(d+1, p‖1))  otherwise

EMPTY          = [0u8; 32]                                     (a constant, not a hash)
leaf(k, v)     = sha256("raster-index/v2/leaf" ‖ k ‖ postcard(v))
internal(l, r) = sha256("raster-index/v2/node" ‖ l ‖ r)
root           = node(0, ε)
```

This is the construction described in Haider, "Compact Sparse Merkle Trees" (2018). It has three
properties the rest of the design relies on:
- **Canonical:** the shape is a function of the key set only, so insertion order does not matter.
  The recorder (incremental) and any batch construction agree byte for byte.
- **Shallow:** for `N` uniformly distributed keys, the expected leaf depth is about `log₂ N`, and
  the maximum is about `2·log₂ N` with high probability. Keys are SHA-256 outputs, so they are
  uniform.
- **Self-delimiting:** a leaf commits to its full key, so a leaf at depth `d` says "the only key
  under this prefix is `k`". A verifier holding `k` checks the prefix itself.

The three node kinds are domain-separated, and `EMPTY` is not a hash output. So a leaf can't pass
for an internal node, and a subtree with keys can't pass for an empty one, short of a SHA-256
collision. The depth is not hashed into internal nodes: a node's position is fixed by the path
the verifier walks, using the key it holds.

### Proofs

```rust
// raster-core/src/transition.rs (replacing the two 256-sibling proofs)
pub struct CoordinateIndexMembershipProof {
    pub coordinates: CfsCoordinates,
    pub value: StorageIndexValue,
    /// Siblings from the leaf's depth up to the root: `siblings.len()` is the
    /// leaf's depth. `EMPTY` siblings are carried as-is.
    pub siblings: Vec<Hash32>,
}

pub struct CoordinateIndexNonMembershipProof {
    pub coordinates: CfsCoordinates,
    /// What sits at depth `siblings.len()` on `key(coordinates)`'s path.
    pub terminal: IndexTerminal,
    pub siblings: Vec<Hash32>,
}

pub enum IndexTerminal {
    /// An empty subtree: no key has this prefix.
    Empty,
    /// The one key with this prefix, which is not ours.
    Leaf { key: [u8; 32], value: StorageIndexValue },
}
```

**Verify membership** (`k = key(coordinates)`, `d = siblings.len()`): fold `leaf(k, value)` up
through `siblings`, taking each level's side from `k`'s bit at that depth, and compare with the
root. A path that is too long (the leaf placed below where it is alone) gives
`internal(leaf, EMPTY)`, which the canonical tree never contains, so the root doesn't match. A
path that is too short folds an internal node's worth of siblings into a leaf, which also doesn't
match. There is no extra shape check to forget.

**Verify non-membership:** `d = siblings.len()`. For `Empty`, fold `EMPTY` up. For `Leaf { key: k',
value }`, require `k' ≠ k` and that `k'` and `k` agree on their first `d` bits, then fold
`leaf(k', value)` up. Compare with the root. The prefix check is implied by the root match against
a canonical root, but it costs nothing and keeps the argument local.

### Writes: the guest computes the new root

Today a write carries a non-membership proof against `index_root_before`, a membership proof
against `index_root_after`, and an assertion that both have the same siblings
(`verify_storage_write_witness`, "Coordinate-index update proof siblings changed across
insertion"). That assertion is right for a full-depth tree and wrong for this one: inserting next
to an existing leaf pushes that leaf down, and the path grows.

Instead the guest derives the root after the write. The siblings above the terminal are
unchanged by the insert, and the subtree at the terminal is a function of what it held and the new
`(k, v)`:

```rust
/// The compact index's root after inserting `(k, v)`, given a verified
/// non-membership proof of `k` against the root before.
pub fn root_after_insert(proof: &CoordinateIndexNonMembershipProof, k: [u8; 32],
                         v: &StorageIndexValue) -> Hash32 {
    let d = proof.siblings.len();
    let subtree = match &proof.terminal {
        IndexTerminal::Empty => leaf(&k, v),
        IndexTerminal::Leaf { key: k2, value: v2 } => {
            // The first bit where the two keys differ; they agree on [0, d).
            let j = first_differing_bit(&k, k2);
            let (ours, theirs) = (leaf(&k, v), leaf(k2, v2));
            let mut node = if bit(&k, j) { internal(&theirs, &ours) } else { internal(&ours, &theirs) };
            for depth in (d..j).rev() {                     // one-child levels between d and j
                node = if bit(&k, depth) { internal(&EMPTY, &node) } else { internal(&node, &EMPTY) };
            }
            node
        }
    };
    fold_up(subtree, &k, &proof.siblings)                   // same siblings, same sides
}
```

`StorageWriteWitness` loses `index_membership_witness`. The guest builds the new leaf's value
itself, `StorageIndexValue { log_position: next_position, object_commitment }`, from the append it
just made. It then asserts `root_after_insert(...) == record.index_root_after`. That replaces three
checks (membership after, its log position, siblings unchanged) with one computation over values
the guest already trusts. A write proves one shallow path instead of two 256-deep ones.

### Host

`IncrementalCoordinateIndex` keeps its API (`new`, `insert`, `root`, `membership_proof`,
`non_membership_proof`, `contains_key`, `get`), so the recorder (`AuthenticatedObjectStore`) and
the prover's replay (`ProofStorageState`) change only where a write witness is assembled. Inside:
- `leaves: BTreeMap<[u8; 32], (CfsCoordinates, StorageIndexValue)>`, sorted by key, so the
  neighbour a new key collides with is a range lookup,
- `nodes: HashMap<(u16, [u8; 32]), Hash32>`: the internal nodes that exist, keyed by
  `(depth, prefix)`, with a cheap hasher (`storage-write-cost` 2a's point about SipHash on 33-byte
  keys),
- `insert`: find the terminal on the key's path, build the new subtree as `root_after_insert`
  does, and rehash the path to the root. About `log₂ N` hashes and map writes instead of 256,
- hashes are `[u8; 32]` throughout, with no `Vec` per level.

The guest's verifier and the host's builder share `leaf`, `internal`, `fold_up` and
`root_after_insert` in `raster-core::coordinate_index`, so the two can't define the tree
differently.

### What moves

- `coordinate_index_root` of every non-empty store, so `index_root_before/after` in every
  storage-touching `StepRecord`, so every trace leaf from the first write onward, so every
  `commit.bin`. A `commit.bin` made before the change can't be audited after it, as with any
  trace-format change. It must be regenerated, not migrated.
- The empty index's root: `EMPTY`, where today it is the 256-level empty hash.
  `assert_opens_at_genesis` compares against `coordinate_index_root(&BTreeMap::new())`, so it
  follows automatically.
- `CoordinateIndexMembershipProof`, `CoordinateIndexNonMembershipProof` and `StorageWriteWitness`
  change shape. So do `TransitionInput` and the storage guest's input, and with them the
  transition and storage guests' image ids.
- **Not** `program_commitment`, `Raster.lock` or the tile image ids, provided tiles don't link the
  index code. Check with `cmp` on a rebuilt tile, as before. Not the control-flow guest's logic,
  though it shares `raster-core`.

## Soundness

**Claim:** a read or write is accepted exactly when it would be against the full-depth index of
the same key-value set. Equivalently: membership proves `(k, v)` is in the set, non-membership
proves `k` is not, and `root_after_insert` is the compact root of the set plus `(k, v)`.

- **Binding.** The compact root is a canonical function of the set (§The tree). A membership proof
  that folds to it either names the leaf really at that position, or exhibits a SHA-256 collision
  among domain-separated nodes. The leaf commits to its full key and the verifier supplies `k`, so
  the leaf is `k`'s. Domain separation and the non-hash `EMPTY` rule out a leaf passing for an
  internal or empty node, and the reverse.
- **Non-membership.** The terminal is what sits at `k`'s path at depth `d`. If `Empty`, no key has
  that prefix, so `k` is absent. If `Leaf(k')` with `k' ≠ k`, it is the only key with that prefix,
  so `k` is absent.
- **Insert.** Only the terminal's subtree changes, and its new canonical form is determined by
  what it held and `(k, v)`. That is the case split in `root_after_insert`. The siblings above
  are the same nodes before and after, so folding the new subtree with them gives the new canonical
  root. The guest compares that with the record's `index_root_after`. A record claiming any other
  root is refused, as it is today.
- **Depth is not an attack surface.** An adversary choosing coordinates to make keys share long
  prefixes would need SHA-256 partial preimages, and even then would only make proofs longer, at
  most 256 levels, today's cost. Correctness doesn't depend on depth.

**Test obligations:**
- a property test that `IncrementalCoordinateIndex` agrees with a direct recursive implementation
  of `node(d, p)`, root and proofs, over random sets and insertion orders, keeping today's
  `legacy_*` reference-implementation pattern,
- for each proof type, a tampered sibling, a wrong side, a too-deep path, a too-short path, a
  terminal with the wrong prefix, and `Leaf(k)` presented as non-membership of `k` are each
  refused,
- `root_after_insert` equals the batch root of the set plus `(k, v)`, for both terminal kinds and
  for collisions at every depth from 0 to 255 (constructed keys, not hashed coordinates).

## Cost

**Estimated** from measured per-level costs. Step 0 replaces the estimate's depth with the real
one.

- **Depth.** `prompt-prepare` runs 1,275 tiles, and not every step writes (recur iterations
  don't), so it holds on the order of 10³ objects: an expected depth of about 10–11 and a worst of
  about 20. `hello-tiles` holds fewer, about 7–9 deep. Against 256, that is 23–35 times fewer
  levels. Step 0 counts them.
- **Per level.** About 1,130 cycles today, mostly `Vec` allocation around two SHA blocks. With
  `[u8; 32]` buffers it should be nearer 300–500. Not measured.
- **Storage guest.** Index proofs ≈16.5M and ≈10.4M today. A read's proof becomes about
  11 × ~500 ≈ 6k cycles instead of 289k. A write's two proofs become one proof plus a small
  subtree build, about 12k instead of 578k. That is under 0.5M per window on both programs. The
  frame decode loses most of its 8 KB per proof (4.6M and 3.2M today). **Estimate: the storage
  guest goes from 24.2M to about 5–7M user cycles on `hello-tiles` and from 15.8M to about 3–5M on
  `prompt-prepare`**, smaller than the transition guest. With today's segment counts, that is a
  window of about 20–25 and 15–18 segments instead of 40 and 28.
- **Transition guest.** Its genesis, program-output and returned-state reads are a few proofs a
  window. That is a small saving on top.
- **Host.** An insert goes from 256 hashes plus 256 map writes to about `log₂ N` of each:
  `storage-write-cost` predicted about 10× on top of 2a for this redefinition. The prover's replay
  of the store for witnesses gets the same.

## Migration

0. **Measure the depths.** A host-side test or `show` subcommand that builds the compact index
   from a recorded trace's writes (`hello-tiles`, `prompt-prepare`, `chain-example`) and prints
   the object count and the leaf-depth histogram. No commitment changes, and it decides whether
   §Cost's depths hold.
1. **The compact index, side by side.** Add `leaf`, `internal`, `fold_up`, `root_after_insert`,
   the new verifiers and a `CompactCoordinateIndex` in `raster-core::coordinate_index`, next to
   the current one and used by nothing, with all §Soundness tests. It can be reviewed on its own
   terms.
2. **Switch.** The recorder, the prover's replay, both guests' verifiers, `StorageWriteWitness`
   without the membership-after proof, and `verify_storage_write_witness` rewritten around
   `root_after_insert`. One change, because the recorder's roots and the guests' checks have to
   agree. Delete the full-depth index and its `legacy_*` tests.
3. **Verify end to end.** Regenerate commitments and run `run --commit` / `--audit` on
   `hello-tiles` and `chain-example` (honest audits must pass), and fraud proofs at `hello-tiles`
   27, 47, 58 and `prompt-prepare` 796. Run the storage-guest tamper tests, rebased on the new
   witness shape. Confirm with `cmp` that tile ELFs did not change.
4. **Measure**, as `storage-prover` §Where the storage guest's cycles go did: the storage guest
   per category, the window per mode, and `storage-write-cost`'s `append_index_ns` on the host.
5. **Update** `storage-write-cost` (Term 2 done, by 2b), the specs that describe the index
   (`docs/specs/core/4-verify/02-what-must-run-next-algorithm.md`), and the proposals index.

## Open questions

1. **`EMPTY` as `[0; 32]` or as a domain hash?** A constant that is not a hash output is simplest
   and costs no hashing. A domain hash is the more common choice in implementations. Either is
   sound given the domain-separated node kinds. Leaning `[0; 32]`.
2. **Carry `EMPTY` siblings, or a bitmap?** A bitmap of which siblings are empty saves 32 bytes
   per empty level. At depth about 11 there are only a few empty levels, so leaning towards
   carrying them, for the simpler proof.
3. **Hash the depth into internal nodes?** Today's tree does (`combine_node_hash(depth, …)`). It
   isn't needed for soundness here, because position comes from the verifier's walk, and leaving
   it out matches the published constructions. It costs nothing either way, so this is taste.
   Leaning no.

## Deferred

- **Multiproofs within a segment.** With 16 or 32 steps' reads against nearby roots, shared upper
  nodes could be hashed once. At about 11 levels a proof, it saves little and complicates the
  witness, so revisit only if step 4 shows the index still matters.
- **A 16-ary tree.** Fewer levels, but each node hashes 16 children,
  and SHA-256 cost follows bytes hashed, so binary is better for a guest.
