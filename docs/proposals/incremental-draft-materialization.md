# Proposal: `incremental-draft-materialization` — seal a draft instead of rebuilding it, and let the wrapping sequence close it

Status: Proposed 2026-09-21.

Related:
- [`incremental-draft-witness`](./incremental-draft-witness.md) (implemented 2026-08-15) — **the
  half that is already done.** It made the *digest* incremental: a push is `O(log N)` and
  `recompose_root` is `O(#fields)` "with no element ever touched". This proposal is the same move
  on the *payload and index*, which that one left alone.
- [`recur-deferred-finalize`](./recur-deferred-finalize.md) (implemented 2026-08-28) — `finalize =
  false`, whose own summary is that deferring the close *"moves no attestation; it moves only the
  materialization"*. If materialization stops being a lump, that flag's reason to exist goes with
  it. Its two open questions are answered here.
- [`draft-provenance`](./draft-provenance.md) (proposed) — `finalize(draft)` severs provenance
  because it is an `Expr::Call` the resolver does not recognize. This proposal removes the call
  rather than teaching the resolver about it.
- [`authenticated-chain-draft-output`](../issues/authenticated-chain-draft-output.md) — the open
  issue that finalize writes to storage with **no trace step**, so the recorder's replica never
  performs the write. Closing at a sequence boundary gives it one.

## What is already incremental, and what is not

Measured against the code, not assumed.

| per push | cost | where |
| --- | --- | --- |
| element subtree root | `O(element)`, once | `draft_value_root(&tree)` |
| list root | `O(log N)` | `AppendFrontier::push` |
| draft root | `O(#fields)` on demand | `recompose_root` |
| element *payload bytes* | **not computed** | — |
| element *index node* | **not computed** | — |

`AppendFrontier` is a real incremental Merkle frontier — `(len, last leaf, ommers)` — and its root
is exactly the whole-list root, checked at every length up to 1024:

```rust
frontier.push(leaves[len - 1]);
assert_eq!(frontier.root(), Some(list_root_from_hashes(&leaves[..len], len as u64)));
```

So the draft already knows, incrementally, the root that a full Merkleization would produce.

### The finding that motivated this — and what measuring it showed

`finalize` throws the frontier's work away and starts over:

```rust
// Materializing the whole object here is correct and stays: it is O(N)
// once, which was never the problem.
let tree = build_draft_tree(&state.schema, &state.field_values(), require_complete)?;
typed_value_from_tree::<S>(&tree)
```

then `store_value_at_coordinates` runs `postcard::to_allocvec` and
`raster_payload_for_value`, whose `encode_raster_value` re-derives every element root and every
Merkle level — work `draft_value_root` already did per push and `AppendFrontier` already folded.
**A draft of N elements hashes all N of them twice.**

That is true, and it is **not where the time goes.** Measured 2026-09-21 with the
`draft_materialize_ns` / `draft_store_ns` counters added for this purpose (`--features profiling`,
`ProfileStreamEvent::DraftFinalize`):

| | `hello-tiles` (5 finalizes) | `prompt-prepare` (10 finalizes) |
| --- | --- | --- |
| program total | 3.6 ms | 18.3 ms |
| **finalize** | **2.0 ms — 55% of program** | **2.6 ms — 14%** |
| materialize (`build_draft_tree` + `typed_value_from_tree`) | 0.043 ms — 2% | 0.148 ms — 6% |
| store | 1.96 ms — 98% | 2.35 ms — 91% |
| ↳ postcard | 0.007 ms — 0.4% of store | 0.006 ms — 0.3% |
| ↳ **`encode_raster_value`** (the double hash) | **0.14 ms — 7%** | **0.38 ms — 16%** |
| ↳ **storage `append`** | **1.82 ms — 93%** | **1.94 ms — 83%** |

And `append` splits again:

| `append`, per call | `hello-tiles` 363 µs | `prompt-prepare` 194 µs |
| --- | --- | --- |
| `current_root()` ×2 + frontier clones | 116 µs — 32% | 107 µs — 55% |
| coordinate-index update (`INDEX_BITS = 256`) | 245 µs — 68% | 86 µs — 44% |
| frontier append — *the actual record of the modification* | **1.0 µs — 0.3%** | **0.7 µs — 0.4%** |

**So the premise of this proposal is wrong in its weighting.** Removing the double hash removes
7–16% of the store half. The remaining 83–93% is `append`, and inside it the two dominant terms —
recomputing storage roots and a 256-bit-deep index update — are **paid by every storage write in
every program**, not by drafts. They are not this proposal's to fix; see
[`storage-write-cost`](./storage-write-cost.md).

This proposal stays worth doing: it is semantically free and removes a real duplicate pass. But it
should be sequenced **after** the two terms above, and it should not be justified on the double
hash being the bottleneck, because it is not.

## Mechanism — build the payload and index as the draft grows

Keep, per append field, alongside the frontier:

- the element's encoded payload bytes, appended to a per-field buffer;
- the element's `RasterNode`, pushed to a per-field node arena;
- the list node's `merkle_levels`, updated along the right spine (`O(log N)`).

`finalize` then **seals**: concatenate the field buffers, write the header, emit the index. No
element is re-encoded and no element is re-hashed.

### Why the encoding permits it

Two facts make appending layout-safe, both verified:

- **Lengths are fixed-width.** `parse_u64` reads 8 bytes LE. A list payload is
  `0x02 ‖ len:u64` and each element is `len:u64 ‖ child` — so appending rewrites `len` **in place**
  and adds bytes at the end. Element offsets never shift. (Had lengths been varint, a list crossing
  a width boundary would shift every element after it.)
- **The node arena is push-only.** `RasterIndex.nodes: Vec<RasterNode>` with children referenced by
  index, so a new element node appends and no existing index moves.

### The two things that do not permit it

**1. `RasterNode.offset` is absolute.** `prepare_raster_children` threads a file offset down from
the root, so every node's offset is measured from the start of the file. Anything that grows
earlier in the file invalidates every later offset.

**2. Struct fields are laid out sequentially.**

```rust
TreeValue::Struct(fields) => {
    let mut child_offset = offset + 1 + 8;
    for (name, child) in fields {
        ...
        child_offset += 8 + name_len + 8 + child_payload.len() as u64;
    }
}
```

Fields occupy consecutive regions, and a draft's fields are a `BTreeMap` — **sorted by name**. For
`CollectiveGreeting { title, lines }` the order is `lines, title`, so appending to `lines` shifts
`title`. A draft with one append field is only append-safe if that field happens to sort last.

### Two remedies, and the trade between them

- **(a) Per-field buffers, concatenate at seal.** No format change. Seal is one `memcpy` per field
  plus an `O(#nodes)` offset fixup. Removes both re-hashing and re-encoding; leaves a linear pass
  that touches no hash function. Cheap to build, and the fixup is the only thing that stays `O(N)`.
- **(b) Relative offsets (`rindex04`).** Store each node's offset relative to its parent's region,
  so nothing to fix up and seal is `O(#fields)`. A format break — but the format is already
  versioned (`rindex03`, with `rindex02` still recognized as legacy), and `paged-bytes` broke it
  once before.

(a) is the smaller change and captures most of the win; (b) is what makes seal genuinely `O(1)` in
the element count. **They compose** — (a) can land first and (b) later, since (b) only removes the
fixup (a) introduces.

## The lifetime model this unlocks

With seal cheap, *where* a draft closes stops being a cost decision and becomes a semantic one.
(Measurement qualifies this: a seal costs one storage `append`, ~200–360 µs, dominated by terms
`storage-write-cost` addresses. Cheap *relative to N*, not cheap absolutely.)

**Rule.** A draft is closed by the sequence that wraps its creation. A producer — a tile, a recur
tile, a recur sequence — may hand back an open draft; the enclosing sequence closes it at its
boundary, or passes it outward, in which case ownership transfers to *its* caller and the same rule
applies one level up. `finalize` disappears from the sequence grammar.

### What it fixes

- **`draft-provenance`'s severance.** With no `finalize(d)` call expression there is nothing for
  `expr_root_ident` to fail to recognize. The value leaving the sequence is the sequence's output,
  which the resolver already attributes. The proposal's `[8] concat_messages [Inline, Inline]` stops
  being reachable rather than being repaired.
- **`authenticated-chain-draft-output`.** Closing at a boundary means closing *at a step* —
  `SequenceEnd` already exists, is already replayed, and already carries `output_commitment`. The
  recorder's replica performs the write because the trace records it.
- **The guest's insert-only draft tracking.** `checks/drafts.rs` inserts into `active_drafts` and
  never removes; nothing verifies a draft was closed, closed once, or closed in the right frame.
  A boundary close is a place to hang that rule: **at `SequenceEnd`, every draft opened in the
  frame is closed or transferred out, and `active_drafts` loses the entry.** Without this the model
  is only nicer to write; with it, draft lifetime becomes proven rather than compiler-enforced.
- **`recur-deferred-finalize`'s open questions.** *"Should the CFS mark an open recur explicitly
  rather than implicitly by entry point?"* — yes, and it stops being special: every producer may
  return open. *"Should `RecurTileEnd` record the draft's post-root instead of `output: None`?"* —
  yes; that root is what the enclosing frame receives and later seals.

### What must change, and what is not yet settled

- **A rule is reversed, not relaxed.** `#[sequence]` currently *refuses* an open draft return:
  `ProtocolReturnKind::Draft(_) => panic!("must finalize Draft handles before returning")`. The
  reason is real — a sequence's output must be storage-backed, and an open draft is not a stored
  object. **Open: what does the inner sequence's `SequenceEnd.output_commitment` commit to?** The
  coherent answer is the draft *root*, which the ops chain already authenticates, but that changes
  what the field means for every consumer.
- **The coordinate moves.** `store_finalized_draft` picks coordinates from
  `current_recur_site_coordinates()`, so a draft closed inside a recur site is charged to the site.
  Closing at the wrapping sequence puts the object in that sequence's frame instead.
- **A created-but-unused draft must stay an error.** Under blanket auto-close, a draft nobody uses
  would be silently sealed, stored, committed and traced. Auto-close only what is returned or
  consumed; keep "created and dropped" a compile error, as today.
- **Set-once fields are still whole values in the witness.** `incremental-draft-witness` §5 was
  deliberately not done — the witness carries a full `SchemaNode` per step and a set-once field's
  whole value rather than its root. That is orthogonal, but it bounds how small a draft step can get.

## Update-at-coordinate — measured and ruled out

An earlier direction was to let a draft occupy its coordinate from the first append and update it
per element, so `SequenceEnd` could simply name an object already in storage. The storage
structures permit it — `objects`, `entries` and `node_hashes` all use `insert`, which overwrites,
and the log already records one entry per modification; only three `assert!`s forbid it. The
previous-value pin an update witness needs comes free from the guest's `active_drafts`, since
`draft_root_from_field_roots` and the raster root are both `struct_commitments_root` over the same
child roots.

It is nonetheless **ruled out on cost**: the per-element price is one full `append`, measured at
194–363 µs. At 100 K elements that is 20–36 seconds against a whole-program time of 18 ms.

The variant that survives is to append to the **log** per modification (0.7–1.0 µs) and update the
**index** once at the seal. That keeps every modification witnessed in the frontier — which is
where the security argument actually lives — at ~0.1 s per 100 K elements. It depends on
`storage-write-cost` landing first, since today the log append cannot be separated from the index
update.

## Costs

- Draft runtime state grows: per append field, a payload buffer and a node arena alongside the
  values it already keeps. Peak memory is unchanged in order (the values dominate) but the constant
  rises.
- Remedy (b) is a `.rindex` format break — regenerate artifacts, `paged-bytes` sets the precedent.
- The lifetime change moves `finalize` out of the sequence grammar: every program with a draft
  re-locks, exactly as `draft-provenance` notes for its own smaller change.

## Verification

- `append_frontier_root_matches_list_root_from_hashes` already pins frontier ≡ list root; the new
  invariant is the sibling: **a sealed draft's payload, index and root are byte-identical to
  `raster_payload_for_value` on the same value**, checked over the same 1..1024 growth.
- An element-hash counter asserting each element is hashed **once** across create→seal.
- `hello-tiles` and `examples/chain-example` through the full ladder — the latter is
  `authenticated-chain-draft-output`'s reproducer and should start passing.

## Not in this proposal

Draft→draft transformation (mapping every element of one draft into another) is
[`draft-map`](./draft-map.md). It needs a coverage rule this proposal does not supply.
