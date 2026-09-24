# Proposal: `incremental-draft-materialization` — seal a draft instead of rebuilding it, and let the wrapping sequence close it

Status: Proposed 2026-09-21. **Revised 2026-09-24** — §Update-at-coordinate is now a design
rather than a cost note: a draft *resides* at the coordinate of the recur unit that opens it, from
the site's `Start` step to its seal. That supplies the creation step §The lifetime model flags as
missing, and retires the `[DRAFT_NAMESPACE, n]` namespace along with the replica gap
[`authenticated-chain-draft-output`](../issues/authenticated-chain-draft-output.md) reports.

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

> **Re-measured 2026-09-21 after `storage-write-cost` Term 1 landed.** The first measurement
> concluded "the premise of this proposal is wrong in its weighting — the double hash is 7–16%".
> Two things have changed that conclusion, one numerical and one methodological. The numerical
> one is recorded here; **the methodological one matters more and is in the next subsection.**

Medians over `hello-tiles`' five finalizes, idle machine, `--features profiling`:

| | before Term 1 | after Term 1 |
| --- | --- | --- |
| program total | 3.6 ms | **1.17 ms** |
| finalize, per call | ~400 µs | **~102 µs** |
| ↳ materialize | 2% | 6% |
| ↳ store | 98% | 94% |
| ↳↳ `encode_raster_value` (**this proposal's target**) | 7–16% of store | **22% of store** (21.4 µs) |
| ↳↳ storage `append` | 83–93% of store | **74% of store** (71.6 µs) |
| ↳↳↳ roots | 32–55% of append | **4.7%** (3.4 µs) |
| ↳↳↳ coordinate index | 44–68% of append | **93%** (66.4 µs) |
| ↳↳↳ frontier append | 0.3% | 1.1% (0.8 µs) |

The double hash did not get faster; everything around it got faster, so its share roughly tripled.
Nothing here is a regression — see `storage-write-cost` for the two fixes that produced it.

### The measurement basis was not representative — measured 2026-09-22

Every number in the section above, and in the original measurement, comes from drafts of **two**
elements. `hello-tiles` builds `CollectiveGreeting { title, lines }` with two
`push_draft_greeting_line` calls (`examples/hello-tiles/src/main.rs:92,97`).

This proposal's entire subject is an `O(N)` term, and at `N = 2` an `O(N)` term and an `O(1)` term
are indistinguishable. So the fixture could not show the effect, and the first version of this
document drew a sequencing conclusion from it anyway (*"land it after `storage-write-cost`, it is
only 7–16%"*). That conclusion was wrong.

`large_draft_finalize_scaling` (`raster-runtime/src/storage.rs`, `#[ignore]`d — run with
`cargo test -p raster-runtime --release --lib large_draft -- --ignored --nocapture`) walks N over a
`List<String>` draft. Release build, idle machine, medians over four runs:

| N | push total | materialize | **`encode_raster_value`** | storage append | finalize | ns/element |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 0.01 ms | 0.01 ms | 0.02 ms | 0.12 ms | 0.13 ms | 125 496 |
| 16 | 0.02 | 0.00 | 0.04 | 0.14 | 0.15 | 9 290 |
| 64 | 0.11 | 0.02 | 0.14 | 0.23 | 0.25 | 3 861 |
| 256 | 0.38 | 0.06 | 0.52 | 0.48 | 0.54 | 2 099 |
| 1 024 | 1.74 | 0.22 | 1.54 | 1.55 | 1.78 | 1 735 |
| 4 096 | 6.1–6.4 | 0.8–1.0 | 5.8–7.5 | 5.4–5.8 | 6.2–6.7 | 1 520–1 627 |
| **16 384** | **27.2** | **3.6–4.4** | **23.0–23.9** | **21.7–22.9** | **25.3–26.6** | **1 541–1 624** |

(`encode` is called *by* `append`; it is timed separately as well, so the two columns overlap.
Where `encode` reads slightly above `append`, that is cold-vs-warm noise on the repeated call.)

**What this establishes.**

1. **Finalize is `O(N)` with a converged cost of ~1.6 µs per element.** At 100 K elements that is
   ~0.16 s; at 1 M, ~1.6 s.
2. **`encode_raster_value` is ~90% of it at scale** (23.4 of 26.0 ms at N = 16 384), against 22%
   at `hello-tiles`' N = 2. This proposal's target does not merely survive re-measurement, it is
   the whole cost once a draft is large.
3. **The fixed storage-write cost stops mattering.** The whole `append` overhead measured at
   N = 1 is 0.12 ms; at N = 16 384 that is **0.45%** of finalize. The 256-deep coordinate index —
   93% of an append on `hello-tiles`, and [`storage-write-cost`](./storage-write-cost.md) Term 2 —
   is a *small-draft* cost.
4. **The crossover is around 80 elements.** Fixed cost 0.12 ms over per-element encode ~1.46 µs.
   Below it, Term 2 dominates; above it, this proposal does. `hello-tiles` sits at N = 2, two
   orders of magnitude on the wrong side of the line from any `raster-inference`-scale draft.
5. **The two passes over a draft cost about the same.** Push totals 27.2 ms and finalize 26.0 ms at
   N = 16 384 — so the element-level work is done twice, at near-parity, and the whole draft
   lifecycle is ~53 ms. That is the duplication this proposal removes, now measured rather than
   asserted.

**What this does *not* establish — stated because it would be easy to overclaim.** The table shows
*where the time is*, not *how much of it Stage 1 removes*. `encode_raster_value` does three things
per element — re-hash it, build its payload bytes, build its index node — and only the re-hash is
strictly duplicated work; the payload and node are built exactly once today, just late. Stage 1
moves them earlier rather than deleting them, so push grows as finalize shrinks. Remedy (a) also
leaves an `O(#nodes)` offset fixup behind. **The honest expectation is that Stage 1 removes the
duplicate hash and the rebuild traversals, not the full 1.46 µs/element** — and the split between
those is the measurement to take next, before promising a number.

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

  > **This needs a draft *creation* step, which Stage 2 as written does not emit.** "Every draft
  > opened in the frame" is not a fact the guest can evaluate: `create_draft`
  > (`raster-runtime/src/storage.rs:1042`) publishes no trace step, so the guest never learns a
  > draft was opened. `docs/proposals/README.md` already records this as the blocker on
  > `carried-state-channel` folding `active_drafts` in — *"an absent map entry is legitimate today
  > and `checks/drafts.rs:69` is permissive for that reason"*.
  >
  > So the seal step supplies only the **close** half of the lifetime rule. Stage 2's scope must
  > include a creation step as well, or its provability claim does not land and it delivers only
  > the ergonomic half.
  >
  > **Answered 2026-09-24** by §Update-at-coordinate's residence model: open the draft at the recur
  > site's `Start`, which is already a trace step at `[s]`. That is the creation step, with no new
  > step kind, and it makes empty sweeps well-defined at the same time. This also makes Stage 2 and
  > [`carried-state-channel`](./carried-state-channel.md) share a prerequisite rather than being
  > independent: that proposal's §Problem table lists draft roots as *"host-supplied, unchecked"*
  > at a window open, with the same `if let Some(..)` as the cause, and its defect 2 —
  > *"absence is not a claim"* — is why removal-on-close is not by itself a check.
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

## Update-at-coordinate — the residence model

**Revised 2026-09-24.** The full-append form below stays ruled out on cost. The surviving
variant — log per modification, index at seal — now has a *lifetime* to go with it, and together
they make a coherent design rather than a cost note. What changed is that the seal acquired a
natural moment and the objection that blocked the model turned out not to hold.

### The object resides at its coordinate

A draft takes the coordinate of the recur unit that opens it, at the moment it opens, and stays
there for life. Extensions update that object in place. Nothing is stored at
`[DRAFT_NAMESPACE, n]`, and there is no promotion step at the end — the object was always where it
will end up.

| | today | residence model |
| --- | --- | --- |
| while open | `THREAD_DRAFT_STORAGE`, keyed by `Anchor`, unaddressable | at its coordinate in `ObjectStore` |
| per modification | frontier moves; nothing enters storage | one **log** append (0.8 µs) |
| coordinate index | one insert at `finalize` | one update at the unit's **seal** |
| at close | materialize whole value, `store_finalized_draft` picks a coordinate | nothing to move |

### Why "a half-built object is observable" is not an objection

It was the reason this direction stayed a cost note rather than a design. It does not survive
examination:

- A recur driver is a loop **inside** `call_recur!`. No user code runs between iterations.
- The only body that does run is the recur tile, which receives the draft as a **handle**, not as
  an `AuthRef` to its coordinate.
- Bindings come from the CFS, and the CFS names *items*. Nothing inside a sweep can name the
  site's own output: `PriorItemOutput` addresses prior siblings, and a site is not prior to its own
  body.

So there is no observer inside the unit. And the index settles it independently of that argument:
with the index advancing **once, at the unit's end**, the addressable view never shows an
intermediate state at all, because every witness proves against the index and the frontier. The
log still records each modification, which is where the security argument lives.

### Create at the site's `Start`, not at the first iteration

`RecurTileStart` and `RecurSequenceStart` are already real trace steps at `[s]`. Opening the draft
there rather than on first use gives three things at once:

- **The creation step this proposal's §The lifetime model flags as missing.** That note says Stage
  2 *"needs a draft creation step, which Stage 2 as written does not emit"*, and that without it
  the provability claim does not land. A site `Start` supplies it with no new step kind.
- **Empty sweeps work.** A zero-iteration sweep has no first invocation, yet `finalize_empty_draft`
  exists precisely for that case. Opening at `Start` keeps it a draft that was opened and closed
  having received nothing, rather than an object that never existed.
- **The anchored start the draft chain lacks.** With creation committed, `checks/drafts.rs`'s
  permissive `if let Some(..)` can become a requirement, and `active_drafts` gains the terminator
  it has never had. See the comparison in
  [`../issues/fraud-evidence-storage-unavailable.md`](../issues/fraud-evidence-storage-unavailable.md)
  — storage roots are re-derivable from an anchored chain of proven writes; draft roots are not,
  which is why `root_before` has to ride in the tile's *input* today instead of being checked
  against a value the guest computes.

### What this still needs

- **A write witness for an update.** `verify_storage_write_witness` proves the coordinate was
  **absent** beforehand (`"Coordinate-index non-membership proof is invalid before write"`). A
  second write to a resident coordinate finds membership and fails, so an update needs its own
  witness kind: *old commitment → new commitment*. The previous-value pin it needs comes free from
  `active_drafts`, as below.
- **Two `assert!`s retire**, with tests naming them: `rejects_duplicate_coordinate_writes` on the
  authenticated store and `object_store_rejects_duplicate_coordinate_writes` on the child's. The
  second is load-bearing — its own comment says *"the running program depends on this one."*
- **A coordinate becomes time-dependent.** Under `finalize = false` a second recur extends what the
  first left, so "the object at `C`" has successive values. Reads stay verifiable because a binding
  records the `commitment` it read and proves membership at that step's `root_before` — but this is
  a semantic the model must state, and today does not.
- **Not adopted: "only recur units create drafts."** Considered and set aside. It would forbid
  `hello-tiles`' draft built from plain `call!` tiles, and the
  `call!(begin_layer_output, new!(ActivationSequence), ..)` shape
  [`recur-deferred-finalize`](./recur-deferred-finalize.md) was written around — where creation sits
  outside any recur and two different recurs extend it. Inferring creation from whether `output` is
  `new!(T)` is the same inference that proposal rejected: *"Inferring intent from the output
  expression would silently change it."* Residence and creation-by-recur are separable; this
  section adopts only the first.

### The original cost measurement, unchanged


An earlier direction was to let a draft occupy its coordinate from the first append and update it
per element, so `SequenceEnd` could simply name an object already in storage. The storage
structures permit it — `objects`, `entries` and `node_hashes` all use `insert`, which overwrites,
and the log already records one entry per modification; only three `assert!`s forbid it. The
previous-value pin an update witness needs comes free from the guest's `active_drafts`, since
`draft_root_from_field_roots` and the raster root are both `struct_commitments_root` over the same
child roots.

It is nonetheless **ruled out on cost**: the per-element price is one full `append`. Measured at
194–363 µs before Term 1 and at **71.6 µs after**, that is 7.2 s per 100 K elements — down from
20–36 s, and still far too slow.

The variant that survives is to append to the **log** per modification (**0.8 µs**) and update the
**index** once at the seal. That keeps every modification witnessed in the frontier — which is
where the security argument actually lives — at ~0.08 s per 100 K elements.

Term 1 landing has made this variant *more* attractive rather than less, and clarified exactly what
it is: the coordinate index is now **93% of an append** and the log append is 1.1%, so "log per
element, index at seal" is precisely "skip the 93%". The ~90x ratio between the two is stable under
the Term 2a fix as well (see `storage-write-cost`), since that fix reduces the index constant but
leaves the log append untouched.

## Costs

- Draft runtime state grows: per append field, a payload buffer and a node arena alongside the
  values it already keeps. Peak memory is unchanged in order (the values dominate) but the constant
  rises.
- Remedy (b) is a `.rindex` format break — regenerate artifacts, `paged-bytes` sets the precedent.
- The lifetime change moves `finalize` out of the sequence grammar: every program with a draft
  re-locks, exactly as `draft-provenance` notes for its own smaller change.
- The residence model retires two duplicate-write `assert!`s and adds an update witness kind. It
  also moves every draft object's coordinate — a draft closed inside a recur site is charged to the
  site today via `current_recur_site_coordinates()`, and would be opened there instead — so traces
  and commitments move for every program that builds a draft.

## Verification

- `append_frontier_root_matches_list_root_from_hashes` already pins frontier ≡ list root; the new
  invariant is the sibling: **a sealed draft's payload, index and root are byte-identical to
  `raster_payload_for_value` on the same value**, checked over the same 1..1024 growth.
- An element-hash counter asserting each element is hashed **once** across create→seal.
- `hello-tiles` and `examples/chain-example` through the full ladder — the latter is
  `authenticated-chain-draft-output`'s reproducer and should start passing. As of 2026-09-24
  `hello-tiles` reproduces it too, on the **fraud** path rather than a chain run
  (`build_storage_selection_witnesses`), so both should be checked: honest commit, honest audit,
  injected fraud detected, **and the receipt built** — that last step is the one nothing has
  reached yet.
- Under the residence model, two assertions that do not exist today: a draft's coordinate is
  allocated at the site `Start` step and never changes, and the coordinate index advances exactly
  once per sweep regardless of element count — the second is what makes "no intermediate state is
  addressable" a checked property rather than an argument.

## Not in this proposal

Draft→draft transformation (mapping every element of one draft into another) is
[`draft-map`](./draft-map.md). It needs a coverage rule this proposal does not supply.
