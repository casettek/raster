# Proposal: `incremental-draft-materialization` — seal a draft instead of rebuilding it, and let the wrapping sequence close it

Status: Proposed 2026-09-21. **Revised 2026-09-24** — §One storage rule replaces
§Update-at-coordinate's cost note, for `call_recur!` only. The organising claim is not about
drafts: there is **one rule** for how a produced value becomes a stored object — *at the coordinate
of the step that produced it, by a traced write* — and drafts are the only producer that does not
follow it. Of four schemes in the code today, one (`reserve_synthetic_coordinates` →
`[DRAFT_NAMESPACE, n]`) is not a coordinate space at all, and **both open reproductions of
[`authenticated-chain-draft-output`](../issues/authenticated-chain-draft-output.md) are that one
row**. Under the rule a recur site owns its object at `[s]`, `Draft` shrinks to a tile-local op
buffer that **never crosses a step boundary** — it exists only inside a recur site, and values, not
drafts, travel between steps — `new!`/`finalize`/`finalize = false` leave the language, and
`DRAFT_NAMESPACE` is deleted rather than tidied. A plain tile returns an ordinary value
(`-> CollectiveGreeting`) that a site then derives from, so every step boundary carries a real
object at a real coordinate. Extension by a later site is **derivation** — an ordinary first write whose
contents share structure — restricted to `push`, which makes continuation a whole-object property
*and* dissolves the layout blocker, since payload order and hash order are already decoupled.

**Revised 2026-09-27** — §The draft root rides in the site's recur-progress frame. A fraud window
may open at any step of a sweep, including its close, so the draft root a site carries between
steps must be anchored at every step, not only at the site's `Start`. It becomes one more field of
the `RecurProgressFrame` the site already opens at `Start`, advances per iteration and pops at its
close, so the `recur_progress_commitment` every step already records covers it. That adds no new
`StepRecord` field, no new seed and no storage write per iteration. `Transition.active_drafts`, its
`InitTransition` seed and the permissive `if let Some(..)` in `checks/drafts.rs` are deleted rather
than tightened. Same day, §A recur site gets its own step kinds: the site's `Start` stops borrowing
`SequenceStart` and its close stops borrowing `Exec`. They become two step kinds of their own,
`RecurStart` at `[s]` and `RecurEnd` at `[-s]`, shared by recur tiles and recur sequences, with the
family read from the CFS. This binds the site's inputs once and gives the close its own coordinate.

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
  > **Answered 2026-09-24** by §The derivation model: a recur site opens its object at the site's
  > `Start`, which is already a trace step at `[s]`. That is the creation step, with no new step
  > kind needed for it, and it makes empty sweeps well-defined at the same time. (§A recur site
  > gets its own step kinds later gives that step a kind of its own, `RecurStart`, for reasons
  > independent of drafts.) Under derivation the lifetime
  > rule also narrows usefully — a site owns exactly one object and completes it at its own close,
  > so "closed, closed once, in the right frame" becomes a property of the site rather than a rule
  > the wrapping sequence has to enforce. This also makes Stage 2 and
  > [`carried-state-channel`](./carried-state-channel.md) share a prerequisite rather than being
  > independent: that proposal's §Problem table lists draft roots as *"host-supplied, unchecked"*
  > at a window open, with the same `if let Some(..)` as the cause, and its defect 2 —
  > *"absence is not a claim"* — is why removal-on-close is not by itself a check.
  >
  > **How the creation step lands, 2026-09-27**: not as a storage write and not as a new
  > carrier. The site's `Start` opens the draft's entry in the site's `RecurProgressFrame`, and
  > `recur_progress_commitment` carries it to every later step of the sweep. See §The draft root
  > rides in the site's recur-progress frame.
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

  > **Dissolved for recur tiles by §The derivation model.** A site returns `AuthRef<S>`, so nothing
  > open crosses a boundary and `output_commitment` keeps meaning what it means everywhere else.
  > The question survives only for a recur *sequence*, which that revision leaves out of scope.
- **The coordinate moves.** `store_finalized_draft` picks coordinates from
  `current_recur_site_coordinates()`, so a draft closed inside a recur site is charged to the site.
  Closing at the wrapping sequence puts the object in that sequence's frame instead.
- **A created-but-unused draft must stay an error.** Under blanket auto-close, a draft nobody uses
  would be silently sealed, stored, committed and traced. Auto-close only what is returned or
  consumed; keep "created and dropped" a compile error, as today.
- **Set-once fields are still whole values in the witness.** `incremental-draft-witness` §5 was
  deliberately not done — the witness carries a full `SchemaNode` per step and a set-once field's
  whole value rather than its root. That is orthogonal, but it bounds how small a draft step can get.

## One storage rule — a value lives at its producer's coordinate

**Revised 2026-09-24**, replacing §Update-at-coordinate's cost note and an intermediate *residence*
sketch of the same day. Written for `call_recur!`; recur sequences are noted at the end.

The organising claim is not about drafts. It is that **there is one rule for how a produced value
becomes a stored object, and drafts are the only producer that does not follow it.**

> A value becomes a storage object at the coordinate of the step that produced it, by a traced
> write.

That is already true of tiles: `reserve_execution_coordinates` returns *"the step's own
coordinate"*, and objects and steps deliberately share the coordinate space — which is exactly what
lets `PriorItemOutput` resolve a sibling's output as `parent ++ [item_index]`.

### Today there are four schemes, and one is not a coordinate space at all

| producer | where its object lands | follows the rule? |
| --- | --- | --- |
| plain tile | its own CFS position, `reserve_execution_coordinates` | yes |
| draft closed **inside** a recur site | the site's position, `current_recur_site_coordinates()` | yes |
| draft closed **outside** a recur | `reserve_synthetic_coordinates` → `[…, DRAFT_NAMESPACE, n]` | **no** |
| `call_recur!(.., finalize = false)` | nowhere — no object exists | **no** |

The third row is a parallel namespace. `reserve_synthetic_coordinates` takes whatever frame is
current and pushes `DRAFT_NAMESPACE` plus a private counter: not a CFS position, not derivable from
the program's shape, and written by no step.

### The anomaly is the reported failure

Both open reproductions of
[`authenticated-chain-draft-output`](../issues/authenticated-chain-draft-output.md) are that row,
and nothing else:

```
recorder.rs:642 → Missing storage object at CfsCoordinates([-2147483648, 2])   (chain-example stage 3)
run.rs:765      → Missing storage object at CfsCoordinates([-2147483648, 2])   (hello-tiles, fraud path)
```

`-2147483648` is `DRAFT_NAMESPACE`. Both are drafts finalized *outside* a recur — a stage returning
a `Draft`, and `hello-tiles/src/main.rs:101`. The recorder rebuilds its replica from trace events,
so an object at a coordinate no CFS position names and no step writes **cannot exist there**. The
missing trace step and the parallel namespace are one defect, not two: give the write a CFS
position and the step that writes it is the event the recorder already replays.

### What the rule implies

- **A recur site owns one object**, at `[s]`. It opens at the site's `Start`, the iterations extend
  it, the site's close completes it, and the site returns `AuthRef<S>`. No seal ceremony — the
  close *is* the completion.
- **`Draft` shrinks to a tile-local op buffer**: operations a tile performs, applied to storage to
  build the site's object. It never escapes a tile body.
- **`new!`, `finalize` and `finalize = false` leave the language.** The flag existed only because
  closing was the default and a chain needed to opt out; a site that closes nothing but its own
  object gives it nothing to opt out of — which is what
  [`recur-deferred-finalize`](./recur-deferred-finalize.md) anticipated in saying deferral
  *"moves no attestation; it moves only the materialization."*
- **`DRAFT_NAMESPACE` is deleted**, not tidied. Under one rule no value needs it.

### The restriction: a draft never crosses a step boundary

This is the rule that closes the last gap, and it is worth stating as a prohibition rather than a
convention:

> **A `Draft<S>` exists only inside a recur site, between its `Start` and its `End`. No step
> boundary ever carries one.** Values cross between steps; ops live inside a tile.

`new!` is not the defect — it is a symptom. The defect is an **open draft crossing a step boundary
with no storage identity**, which is exactly what mints `[DRAFT_NAMESPACE, n]`: a value that exists
between steps but belongs to no step, so no CFS position names it and no write records it.

That is why moving creation into a tile's return type does not help. A tile declared
`-> Draft<CollectiveGreeting>` still emits no object at its own coordinate, still leaves its
`output_commitment` empty, and still gives a later site nothing to read a `StorageReadWitness`
against. Same hole, respelled. It also needs machinery that does not exist: `IntoDraft` converts
only things that are *already* drafts, so a value would have to be diffed into ops — twice,
identically, on the host and on riscv32 — and a struct literal can express a draft that
`apply_draft_ops` rejects outright, since `Set` on an append-only field is refused (*"does not
support set; use push"*).

**What replaces it.** A plain tile returns an ordinary value, and a recur site extends it by
derivation:

```rust
#[tile]
pub fn begin_greeting(title: String) -> CollectiveGreeting {
    CollectiveGreeting { title, lines: List::empty() }   // an ordinary value, object at [3]
}
```

```rust
let g = call!(begin_greeting, "Draft-built greeting".to_string());          // → object at [3]
let g = call_recur!(tile = add_line, input = lines, output = g, args = ()); // derives → [4]
```

No `Draft` in any signature outside a recur tile, no conversion, no new op kind — and every step
boundary carries a real object at a real coordinate. Under append-only derivation **extendability
is not a property of the type**; it is a property of what the next site does.

**The cost, stated plainly.** A chain of plain tiles each contributing to one object — the shape at
`examples/hello-tiles/src/main.rs:85` — is no longer expressible as drafts. Tile *n* would have to
take the value and return a new one, which is a copy: fine for a small struct, `O(N)` for a growing
list. So the rule that falls out is **plain-tile chains for small values; anything that grows is a
recur site.** That is the right pressure rather than an accident: the thing that grows is exactly
the thing needing incremental attestation, and a recur site is what supplies it.

### What the restriction changes

| area | change |
| --- | --- |
| `raster/src/input.rs` | `new_draft` and `finalize` removed from the surface; `Draft<S>` becomes unconstructable by user code. `IntoDraft` keeps only its `RecurSequenceOutput` impl (recur sequences, out of scope here) |
| `raster/src/lib.rs` | `new` and `finalize` leave the prelude |
| `raster-macros/src/lib.rs` | the `ProtocolReturnKind::Draft(_) => panic!` arms stop being a limitation and become **the rule**. A tile signature taking or returning `Draft<S>` is legal only for a recur tile's `output` slot |
| `raster-macros/src/recur.rs` | `finalize` key removed from `RecurCallInput`; the `*_open` driver set and `__raster_recur_auth_open_<tile>` entry point deleted; `output` becomes present/absent (create) or an expression (derive) |
| `raster-runtime/src/storage.rs` | `reserve_synthetic_coordinates` deleted with its `next_synthetic_index` counter; `store_finalized_draft`'s two-branch coordinate choice collapses to the site's coordinate |
| `raster-core/src/cfs.rs` | `DRAFT_NAMESPACE` deleted — no draft can be closed outside a site, so nothing needs the namespace |
| `checks/drafts.rs` | keeps the per-step `root_before → ops → root_after` check against the witness; loses `active_drafts` and the permissive `if let Some(..)` at `:69`. The expected `root_before` now comes from the site's frame |
| `raster-core/src/recur_progress.rs` | `RecurProgressFrame` gains the draft entry: opened by `push_site`, advanced by `advance_tile_iteration`, compared against `output_commitment` and popped by `close_site`. See §The draft root rides in the site's recur-progress frame |
| `raster-core/src/transition.rs` | `active_drafts` removed from `Transition` and `InitTransition`, together with `TrackedDraftState` |
| `raster-core/src/cfs.rs` (`RecurTileItem`) | `leaves_output_open` deleted; the item says whether the site **derives**, and from which input, so the opening root is not prover-chosen |
| programs | `new!`/`finalize`/`finalize = false` removed everywhere; seeding tiles either fold into the creating recur tile or become plain value-returning tiles that a site derives from |

**`active_drafts` becomes site-scoped, and then it is not needed at all.** An entry opens at a
site's `Start` and closes at its `End`, and no entry outlives its site. That is exactly the lifetime
of the site's `RecurProgressFrame`, so the entry moves into the frame and the map is deleted.
"Closed, closed once, in the right frame" stops being a rule the guest must reconstruct and becomes
a property of where the entries come from. A fraud window can still open mid-site, so an entry
may cross a *window* boundary. The creation record does **not** anchor that case, because the
window does not contain the `Start`. `recur_progress_commitment` anchors it; see §The draft root
rides in the site's recur-progress frame.

### What actually happens during a sweep — one write, not N

The rule above says *at the coordinate of the step that produced it*. For a recur site that step is
the **close**, not the iterations, and this is load-bearing enough to state mechanically because it
is nowhere else in these documents.

A mid-sweep iteration writes **nothing to storage**. The recorder's write is conditional on the
event carrying an output value:

```rust
let storage_write = output.as_ref().map(|output| {
    self.storage.append_serialized_bytes(&output.data, tile_coordinates.clone(), output.raster.clone())
});
```

An iteration that hands back an open draft carries no output, so no write occurs. The single write
happens at `RecurTileEnd` (`recorder.rs:1085`), the same step whose `output_commitment` is set from
`storage_write.entry.object_commitment`.

Two accumulators therefore advance at different rates, and conflating them is easy:

| per iteration | per site close |
| --- | --- |
| one step record | one step record |
| one `hash_trace_item` → trace frontier | — |
| one fingerprint entry | one fingerprint entry |
| **no** storage write | **one** storage write: object + log append + index insert |

A 1000-iteration sweep is 1001 trace steps and 1001 fingerprint entries, and **one** storage write.
The trace records the *process*; storage records the *result*. That is why every draft mutation is
fingerprint-bound without touching storage, and why
[`recur-deferred-finalize`](./recur-deferred-finalize.md) could say deferring the close
*"moves no attestation; it moves only the materialization."*

### Three representations, and why none is redundant

The same draft is tracked in three places, differently, and the reason each exists is worth having
written down:

| | holds | why it cannot be elsewhere |
| --- | --- | --- |
| **child process** | full state: `values` **and** `AppendFrontier`, in `THREAD_DRAFT_STORAGE` keyed by `Anchor` | it has to *compute*. The recorder is downstream on a one-way pipe, and under `--no-auth` there is no recorder at all |
| **recorder** | per-step `draft_transition_witness`, and — under §The draft root rides in the site's recur-progress frame — **one running root per live site**, in its `RecurProgressStack` | it holds *evidence*. Each witness is self-contained — a pre-state frontier plus ops — so nothing needs accumulating to *prove* a step. The running root exists only so the recorder can stamp `recur_progress_commitment`, and it is computed from the same witness |
| **transition guest** | the running root only: `active_drafts[id]` today, the site frame's `draft.root` under §The draft root rides in the site's recur-progress frame | it *verifies*: `root_before → ops → root_after`, one link per step. It never sees an element value |

Keyed by `Anchor`, not coordinates, the child's draft is **unaddressable** while open — which is
what guarantees no step can observe a half-built value, and why there is no `Selectable` impl for
`Draft`.

### Why `root_before` rides in the tile's input

A consequence of the above, and the reason the anchor below matters. Storage roots are
**re-derivable by the verifier**: the guest carries its own frontier, advances it only through
proven writes, and compares — `assert_eq!(storage_root_before, &current_root)`. The recorded value
is a *cross-check* on a fact the guest computed from an anchored chain.

A draft root is not re-derivable. There is no creation step to anchor from and no draft-write
witness to advance through, so the journal has to *state* `root_before` — which means it has to be
in the tile's input, since a replayed tile has no draft store and learns nothing except through its
input. Remove it and `verify_witness_root(pre_state, root_before)` would compare two host-supplied
values, consistent by construction.

Under this proposal that changes: with an anchored start and a checked terminator, `root_before`
becomes the same kind of redundant cross-check storage's already is. "Anchored" has to hold at
**every** step, not only at `Start`, because a window may begin anywhere in a sweep. The storage
root is anchored per step because each `StepRecord` records `storage.root_before`, and the draft
root will be anchored per step because each `StepRecord` records `recur_progress_commitment`.
`root_before` stays in the tile's input, because the replayed tile still has to state which
pre-state its ops apply to. It is no longer the only statement of it.

### Extension is derivation, and append-only makes it checkable

A second site adding to a first site's object is **not** a merge and needs no new storage
primitive:

> `[s2]`'s object *is* `[s1]`'s object plus appends. Both are ordinary first writes at their own
> coordinates; neither is mutated.

```
call_recur!(.. output ..)            → creates [s1], returns AuthRef<S>
call_recur!(.. output = s1_ref ..)   → reads [s1] (proven), continues its frontier, writes [s2]
```

Merkle proofs are indifferent to structural sharing — a selection into `[s2]` is a self-contained
path in `[s2]`'s tree. And continuation is *inherent* rather than separately proven, because
`AppendFrontier` is `(len, last leaf, ommers)`: no frontier but `[s1]`'s roots to `[s1]`'s
commitment, and appending to it cannot rewrite the prefix.

**Restrict *derivation* to `push` — not creation.** A site may `set` and `push` freely on the
object **it created**, at any point in its own sweep. Only a **derived** site — one extending
another site's object — is push-only. Two things follow, and both are larger than they look:

1. *Continuation becomes a whole-object property.* With `set` permitted, a derived object could
   differ from its base in a set-once field, so "`[s2]` is `[s1]` plus appends" would hold only
   field by field. Append-only makes it true of the object.
2. *The layout blocker stops binding.* §The two things that do not permit it shows appends shift
   later fields because a draft's fields are a `BTreeMap` laid out in name order — *"only
   append-safe if that field happens to sort last."* But the **root** does not use layout order:
   `draft_root_from_field_roots` folds child roots through `struct_commitments_root` in *schema*
   order. Layout and hash order are already decoupled, so laying append-only fields **last in the
   payload** makes an append shift nothing, whatever the names sort to.

The line is drawn at **creator vs deriver**, not at *first iteration vs later*. An earlier
draft of this section said set-once fields must be written at `is_first()`; that is wrong and would
forbid a real program. `raster-inference`'s recur outputs are mostly *mixed* structs — 5 of 8 carry
set-once fields beside their lists:

| type | list fields | set-once fields |
| --- | --- | --- |
| `MergedPieces`, `PromptTokenization` | one each | — |
| `KvSequence` | `keys`, `queries`, `errors` | — |
| `PleLayerInputs` | `rows`, `errors` | `layer_idx` |
| `PrefillLogits` | `logits`, `errors` | `decode_position` |
| `ActivationSequence` | `rows`, `errors`, `kv` | `start_position` |
| `DecodeEdge` | `generated_token_ids` | `has_selected`, `decode_position`, `token_id`, `value` |
| `GeneratedOutput` | `generated_token_ids` | `generated_token_count`, `generated_text`, `stop_reason` |

Most of those scalars are sweep *inputs* and could be written at `is_first()`. `GeneratedOutput`'s
are not: `output-finalize/src/lib.rs:143-148` sets `generated_token_count`, `generated_text` and
`stop_reason` from the accumulated `state` **after** the sweep. "Sweep, accumulate, then write
summary scalars" is the natural shape of a finalize stage, and an `is_first()` rule would forbid it.

Creator-vs-deriver keeps every one of those programs legal while preserving what mattered: a
derived site touches only the tail, so continuation stays a whole-object property and the payload
never shifts. `DecodeEdge`'s chain is already push-only downstream —
`append_selected_token` (`decode-select-token/src/lib.rs:96`) does nothing but
`generated_token_ids().push(..)`.

### What this fixes beyond the reported failure

**The draft chain gains an anchor.** A chain's first `root_before` rests today on a host-supplied
`active_drafts` entry, which is why `checks/drafts.rs:69` must stay a permissive `if let Some(..)`.
Under the one storage rule it starts from the site's `Start`, and the site's recur-progress frame
carries it to every later step:

| | today | one rule |
| --- | --- | --- |
| chain start | host-supplied `active_drafts` entry | the site `Start` opens the frame's entry: the empty root, or `[s1]`'s commitment bound through the `Start`'s input binding |
| first `root_before` | asserted, unchecked | must equal the frame's opening root |
| window opening mid-site | host-supplied `active_drafts` entry, unchecked | seed reproduced against the first window step's `recur_progress_commitment` |
| chain end | the step exists and carries the value; **nothing compares them** | `root_after == step.output_commitment`, and the frame is popped |

Read as one sentence: *from an object storage proves exists, applying ops the tile's replay image
attests, you arrive at the commitment storage proves was written.* The join is free because
`draft_root_from_field_roots` **is** `struct_commitments_root` — a draft root and an object's
structural root are the same function of the same child roots.

**Both halves are smaller than they look, because both steps already exist.** `RecurTileStart` and
`RecurTileEnd` are real traced steps at `[s]`, and the close already carries the object's
commitment — `exec_step` sets `output_commitment` from `storage_write.entry.object_commitment`. What
is missing is not a step but a *check*: `verify_draft_transition` returns immediately for any step
without `requires_replay_proof()`, and a site close is `ExecTarget::RecurTile`, so at the exact step
holding the commitment the guest does nothing with `active_drafts` — no comparison, no removal.
(`advance_recur_progress`'s `close_site` fires there, but it closes the recur *progress* frame, not
the draft.) The draft change is two assertions at a step that already runs, not a new step kind or
a new witness. §A recur site gets its own step kinds does change the site's step kinds — the close
becomes `RecurEnd` at `[-s]` — but for reasons of its own; the two assertions do not depend on it. This is also the answer to
[`recur-deferred-finalize`](./recur-deferred-finalize.md)'s open question *"Should `RecurTileEnd`
record the draft's post-root?"* — it already records something equal to it.

**A recur's output is immediately readable.** A site returns a storage reference, so `select!`
reaches it in the same frame — which `hello-tiles` does twice and which no open-handle model can
offer without an explicit close.

**Nothing is retired.** Every write stays a first write at an unoccupied coordinate, so the
duplicate-write `assert!`s stand — including the one whose test says *"the running program depends
on this one"* — and no update-witness kind is needed. `verify_storage_write_witness`'s
non-membership-before proof is unaffected.

### The draft root rides in the site's recur-progress frame

**Revised 2026-09-27.** The two close assertions above compare against the root the guest has
carried to the close. They are exactly as sound as that root's anchor at the window's first step.

**The requirement.** A fraud window may begin at any step: at iteration *k* of a sweep, or at the
site's close. Mid-site anchoring is required, not optional. Today the anchor at a window's first
step is the `InitTransition.active_drafts` entry, which the host supplies and nothing checks.

**Why the creation record is not a storage write.** Today the site's `Start` is recorded as
`StepKind::SequenceStart` (`recorder.rs:470-516`). `[s]` is a scope, so the site's `Start` and its
close share the coordinate the way a nested sequence's boundary steps do, and
`record_matches_item` binds it to the `RecurTile` item the same way. It carries the site's inputs,
including the authenticated `L` from the `0x0A` metadata. It produces nothing, and boundary steps
carry no `StorageRoots`, so the creation record cannot be a write at `[s]`. It does not need to be:
`Start` already opens the site's `RecurProgressFrame` (`push_site`), and that frame is committed on
every step. The same holds after §A recur site gets its own step kinds: `RecurStart` also carries
no `StorageRoots`.

**The design.** Add the draft entry to the frame:

```rust
pub struct RecurProgressFrame {
    // existing: site, kind, chunk, source_len, next_iteration_index,
    //           last_control, state_commitment, state_is_output
    /// The object this site builds, as it stands after the last iteration.
    /// `None` exactly when the CFS says the site owns no object.
    pub draft: Option<SiteDraft>,
}

pub struct SiteDraft {
    pub schema_hash: [u8; 32],
    pub root: DraftRoot,
}
```

| step | frame operation | check |
| --- | --- | --- |
| site `Start` | `push_site` opens `draft` | creating site: `root` = the empty root of `S`. Deriving site: `root` = the base object's commitment, bound the way the site's `Start` binds all its inputs, by its input source witness against the producing record's `output_commitment` |
| iteration | `advance_tile_iteration` advances `draft` | the replay journal carries a `DraftReplayTransition` **iff** `draft` is `Some`, with `schema_hash` and `root_before` equal to the frame's. `root` becomes the `root_after` that `apply_draft_ops` derives |
| site close | `close_site` compares, then pops the frame | `draft.root == step.output_commitment`. The close's storage write witness already proves `([s], output_commitment)` was inserted |
| every step | — | `progress.commitment() == step.recur_progress_commitment`, unchanged |

In one sentence: the object that storage proves was written at `[s]` has exactly the root that the
replay-proven ops reach from the site's opening root.

**Why this anchors a window that opens mid-site.** Nothing new is needed at the window boundary.
[`window-seed-reconstruction`](./window-seed-reconstruction.md) already seeds a window with the
whole `RecurProgressStack` (`TransitionInput.window_start_recur_progress`). The guest advances that
seed through the window's first step, and it must reproduce the step's recorded
`recur_progress_commitment`. A seed with a forged draft root advances to a different stack and
fails. This holds for a window that opens at the close too, since the root compared against
`output_commitment` is one the seed could not forge. The commitment covers the whole stack, so
omitting the entry changes the hash: absence is a claim.
[`recur-state-chaining`](./recur-state-chaining.md) already made this move for loop-carried state,
adding `state_commitment` as *"one more frame field rather than a new carrier"*. So the pattern is
implemented, and the seed plumbing already carries new frame fields.

**What must come from the CFS, not the trace.** Two facts decide the opening root, and the prover
must not choose either of them:
- *Whether the site owns an object.* `RecurTileItem.state_is_output` already says this: a
  state-only site returns its state, and every other site returns an object. Reading it from the
  first iteration's journal instead would make omitting the entry free. That is the same reason
  `state_is_output` was added.
- *Whether the site derives, and from which input.* Without this, a deriving site could be
  presented as a creating one: it would open at the empty root and silently drop its base.

**Parity with the recorder.** Every frame field must be computable by the recorder, which is what
made revision 1 of `recur-progress-commitment` unimplementable. The draft root qualifies. The
recorder already receives each iteration's `draft_transition_witness` (pre-state frontier plus
ops), so it computes `root_after` with the same `apply_draft_ops` the guest runs. One
implementation in `raster-core`, two callers, as with `state_commitment`.

**Zero-iteration sweeps** need no special case. The entry opens with its opening root, no
iteration changes it, and the close compares that same root. For a creating site the stored object
must then be the empty object of `S`. That is what `finalize_draft_value(.., require_complete =
false)` materializes today: unset set-once fields become `Unit`, which is the value
`absent_field_root` hashes. For a deriving site, the stored object must equal its base.

**What it deletes.** `Transition.active_drafts`, `InitTransition.active_drafts`,
`TrackedDraftState` and the `if let Some(..)` at `checks/drafts.rs:69`. No draft exists outside a
site, so the map keyed by the anchor-derived draft id holds nothing the frame does not.

**Rejected: anchoring through storage.** The alternative is to put each iteration's draft root into
storage and anchor the window through the storage frontier, which is already anchored per step.
Three forms were considered:

| form | per-iteration cost | what it needs | verdict |
| --- | --- | --- | --- |
| update the object at `[s]` every iteration | one full `append`: 71.6 µs, 7.2 s per 100 K elements | an update-witness kind; the three duplicate-write `assert!`s relaxed | ruled out on cost, §The original cost measurement |
| append a log entry `([s], root)` per iteration, update the index once at the close | 0.8 µs | a storage **update gate**: a write path legal only for an iteration of site `s` at `[s]`, which the guest can check from the record plus the CFS. Also an update witness, recorder replay, and a window seed read from the frontier's last leaf, which is valid only because the sweep is the sole storage writer during a tile sweep | works, but it rests on an invariant nothing checks, and it adds a witness kind and a write path |
| **the root in the site's frame** | one more field in a hash every step already computes | nothing new at the window boundary | **chosen** |

The gate is feasible and could be narrow, but anchoring is its only use: no step reads the object
mid-sweep. The frame gives the same anchor while storage stays write-once, with one write per site
at its close.

### A recur site gets its own step kinds

**Added 2026-09-27.** The two sections above hang the draft's creation on the site's `Start` and
its completion on the site's close. Neither step has a kind of its own today. The `Start` borrows
`SequenceStart`, the close borrows `Exec`, and both borrowings leak.

**How it got this way.** A recur site originally had one trace event, `RecurTileExec`, published
*after* the loop and recorded as `Exec(RecurTile)`: the site was modelled as one large call, with
inputs, output and storage write in a single record. `recur-progress-commitment` rev 2 needed `L`
authenticated before iteration 0, so its §3.2 split the event. The trailing event was *renamed*
`RecurTileEnd` and kept its `Exec` kind, and `RecurTileStart` was *appended* and recorded as the
closest existing kind, `SequenceStart`. So the close is `Exec` for historical reasons, and the
`Start` is `SequenceStart` because it was the nearest fit.

**Why those choices were reasonable at the time.**

| step | reason | where |
| --- | --- | --- |
| `Start` = `SequenceStart` | same commitments: `input_commitment` and `input_source_commitment`, with no output and no storage. *"`Start` commits to what is about to run"* | `recur-progress-commitment` §3.2 |
| | ordering came almost free: a scope coordinate already yields `{[s], [s][1], [s+1]}`, so marking a bare site coordinate as a scope was a two-line change | §3.2.1, `CfsCursor::get_sequence` |
| | name binding came free: boundary steps bind by `sequence_id == item.id`, so one arm was added | `checks/cfs.rs:129` |
| | the input machinery already handles it, including the `0x0A` payload exception for the recur source | `verify_step_record_inputs`, `binding_requires_payload` |
| | no new `StepKind` variant, so no guest `match` changes (inferred; not stated in the proposal) | — |
| close = `Exec` | it writes the site's output at `[s]`, and only `Exec` carries `StorageRoots` and a write witness | `recorder.rs:1043` |
| | `ExecStep`'s three targets share one shape, so no field can be added to one and forgotten on another | `trace.rs:338` |

**What leaks.**

1. **`sequence_id` is wrong at the `Start`.** A boundary step names the sequence it *enters*, so
   `declared_sequence_id` maps `RecurTile(item) → item.id` (`checks/cfs.rs:383`). But a recur tile
   pushes no frame, and its iterations and its close name the *enclosing* sequence. The `Start`
   puts a tile id in a sequence-id slot, and the two halves of one site name different things.
2. **The `Start` looks like a frame it is not.** The sequence-scope witness lookup accepts any
   `SequenceStart` at the parent coordinates (`checks/cfs.rs:572`), so a recur tile's `Start` at
   `[s]` would pass as the scope frame of `[s][i]`. This is latent only because recur iteration
   steps return from input binding early (`checks/cfs.rs:211`).
3. **Inputs are committed twice.** The macro publishes the same `__raster_input` in both
   `RecurTileStart` and `RecurTileEnd` (`raster-macros/src/recur.rs:822, 960`), and the recorder
   passes it into the close's `ExecStep`. Both steps bind the inputs, and both need input source
   witnesses. §3.2 intended the opposite: *"The trailing event stops being an input carrier at
   all."*
4. **Open and close share `[s]`.** After the close, `[s][1]` is still an ordering-legal successor,
   which only `close_site` rejects. The witness store is keyed by coordinates, so the close's entry
   overwrites the `Start`'s. This is the defect #8 fixed for recur-*sequence* iterations by moving
   their `End` to `[s][-i]`; the site's close never got that fix.
5. **`Exec` promises something the close does not do.** `ExecStep` means *"ran something and
   committed to what it consumed and produced"*. The close runs nothing replayable:
   `requires_replay_proof()` is false for `RecurTile`. The object it writes is checked against
   nothing, which is the missing draft assertion. With `finalize = false` it writes nothing, yet it
   is still an `Exec` with an empty `output_commitment`.
6. **One idea has three shapes.**

   | construct | open | close |
   | --- | --- | --- |
   | nested sequence | `SequenceStart` at `[s]` | `SequenceEnd` at `[s]` |
   | recur sequence iteration | `SequenceStart` at `[s][i]` | `SequenceEnd` at `[s][-i]` |
   | recur site, tile or sequence | `SequenceStart` at `[s]` | `Exec(RecurTile \| RecurSequence)` at `[s]` |

**The change.** Two step kinds, shared by both recur site families:

```rust
pub enum StepKind {
    ProgramStart(ProgramStartStep),
    ProgramEnd(ProgramEndStep),
    SequenceStart { input_commitment, input_source_commitment },
    SequenceEnd { output_commitment },
    Exec(ExecStep),                 // a tile ran: a plain call or one recur tile iteration
    RecurStart(RecurStartStep),     // new, at [s], either family
    RecurEnd(RecurEndStep),         // new, at [-s], either family
}

pub struct RecurStartStep {
    pub site_id: String,            // the CFS item id at [s]
    pub input_commitment: Vec<u8>,
    pub input_source_commitment: Vec<u8>,
}

pub struct RecurEndStep {
    pub site_id: String,
    pub output_commitment: Vec<u8>, // the object written at [s]
    pub storage: StorageRoots,      // exactly one write
}
```

- **`ExecTarget::RecurTile` and `ExecTarget::RecurSequence` are both deleted.** They only ever
  named a site's close. A recur tile's iterations are already recorded as `ExecTarget::Tile` —
  *"`RecurTile` names the site only"* (`trace.rs:784`) — so `ExecTarget` is left with one variant.
  Whether to keep the one-variant enum or replace `target` with a `TileId` is an implementation
  choice.
- **The trace events keep their four names.** Different macro drivers emit `RecurTileStart` and
  `RecurSequenceStart`, and `RecurTileEnd` and `RecurSequenceEnd`. The recorder maps both `Start`
  events to `RecurStart` and both `End` events to `RecurEnd`.
- **The names keep the vocabulary's rule**: a name without `Iteration` is the site, and a name
  with it is one iteration.

**Why two kinds and not four** (`RecurTileStart`/`End` and `RecurSequenceStart`/`End` as step
kinds were considered first):

1. *The guest already takes the family from the CFS, not from the record.* `advance_recur_progress`
   decides tile or sequence by matching the item at `[s]`. Everything else that differs by family
   also comes from the CFS item or the frame: `chunk` exists only on `RecurTileItem`,
   `state_is_output` and the draft entry are read from the item, and `close_site`'s terminal rules
   (5 and 7 for a tile, S4 for a sequence) branch on `frame.kind`.
2. *A duplicated fact has to be checked, or it is an attack surface.* With four kinds,
   `record_matches_item` must reject a `RecurTileStart` at a `RecurSequence` item. With two, that
   disagreement cannot be written down: the record says a site opens here, and the CFS says which
   kind of site.
3. *The payloads are identical.* Four variants would differ only in name.
4. *Fewer match arms*, in the accessors, `record_matches_item`, `verify_sequence_id`, the store
   check and the witness builders.

What that gives up: the event→step-kind mapping is no longer one-to-one, and a trace dump no longer
shows the family without the CFS. `site_id` still names the item, and panic messages can print the
family after resolving it.

| trace event | recorded today as | recorded after as | coordinates |
| --- | --- | --- | --- |
| `RecurTileStart` | `SequenceStart`, `sequence_id` = the tile's id | `RecurStart` | `[s]` |
| `RecurTileIterationExec` | `Exec(Tile)` | unchanged | `[s][i]` |
| `RecurTileEnd` | `Exec(RecurTile)`, re-binding the inputs | `RecurEnd`, no inputs | `[s]` → **`[-s]`** |
| `RecurSequenceStart` | `SequenceStart` | `RecurStart` | `[s]` |
| `RecurSequenceIterationStart` / `End` | `SequenceStart` / `SequenceEnd` | unchanged | `[s][i]` / `[s][-i]` |
| `RecurSequenceEnd` | `Exec(RecurSequence)`, re-binding the inputs | `RecurEnd`, no inputs | `[s]` → **`[-s]`** |

| today's problem | how the new kinds remove it |
| --- | --- |
| 1. `sequence_id` wrong at the `Start` | the site's id moves into `site_id`; `sequence_id` names the enclosing frame on both halves, like every step that is not a sequence boundary. `declared_sequence_id` loses its `RecurTile` arm |
| 2. `Start` mistaken for a frame | the scope lookup matches `SequenceStart` only, and a site no longer emits one |
| 3. inputs committed twice | `RecurEndStep` has no input fields, so the inputs are bound once, at `RecurStart` |
| 4. shared `[s]` | `RecurEnd` sits at `[-s]`, with its own successor set and its own witness-store entry, by the rule #8 introduced. A stray iteration after it is rejected by ordering, not only by `close_site` |
| 5. `Exec` at a step that runs nothing | `Exec` goes back to meaning "a tile ran", always with a replay proof |
| 6. three shapes | the pairing is visible in the kind: sequences use `SequenceStart`/`SequenceEnd`, recur sites use `RecurStart`/`RecurEnd` |

**How it carries the draft design.** The frame operations of §The draft root rides in the site's
recur-progress frame are dispatched on the kind, with the family taken from the item at `[s]`, rather
than on a borrowed kind that only the item disambiguates:

| kind | binds inputs | storage | frame | draft (tile sites) |
| --- | --- | --- | --- | --- |
| `RecurStart` at a `RecurTile` item | yes: the CFS sources, with the `0x0A` payload for `input` | none | `push_site(Tile, chunk, L, state_is_output)` | open the entry: empty root of `S`, or the base object's commitment |
| `Exec(Tile)` at `[s][i]` | through the replay journal, as today | none | `advance_tile_iteration` | `root_before` must equal the entry; the entry becomes `root_after` |
| `RecurEnd` at a `RecurTile` item | **no** | **exactly one** write, at `[s]` | `close_site` | `entry.root == output_commitment`, then pop |
| `RecurStart` / `RecurEnd` at a `RecurSequence` item | as for a tile site | as for a tile site | `push_site(Sequence, …)` / `close_site` | none; recur sequences are out of scope |

One rule becomes easy to state with a kind of its own: `RecurEnd` **always** writes exactly one
object. Under §One storage rule every site produces one, `finalize = false` is gone, and a
state-only site stores its state. So `storage` is never "unchanged", and the guest requires a
write rather than accepting its absence. §Still open's question of where a creating site's empty
root comes from is unchanged: it is needed at `RecurStart`, and still needs `S`'s schema there.

**What the move to `[-s]` forces.**

- **The record moves; the object does not.** The object stays at `[s]`, the site's coordinate, as
  §One storage rule requires, and the recorder already appends at `site_coordinates`. Later
  siblings are unaffected: they already check that their input's *storage* coordinates equal `[s]`
  for a `RecurTile` source (`checks/cfs.rs:748`). But `checks/store.rs:302` builds the expected
  write entry from `step_record.coordinates()`, so for `RecurEnd` it must use `opened([-s]) =
  [s]`, via the helper #8 added.
- **Everything keyed by the site must open the coordinate.** Besides the write entry, two lookups
  compare a record's coordinates against `[s]`:
  - the guest's `close_site(coordinates)` in `advance_recur_progress` compares against
    `frame.site`, so a `RecurEnd` must pass `opened(coordinates)`. The item lookup already
    resolves a close: `try_get_item_exact` opens the coordinate first;
  - the prover's producer lookup for a `PriorItemOutput` source finds the record with
    `record.coordinates == [s]` (`raster-prover/src/trace.rs:868`), filtered by
    `record_produces_item`. For a recur site item it must look for `RecurEnd` at `[-s]`, and
    `record_produces_item` swaps its two `Exec` site arms for `(RecurEnd, RecurTile |
    RecurSequence)`.
- **A negative last coordinate stays unambiguous.** `X ++ [-k]` is a recur-sequence iteration close
  when the item at `X` is a `RecurSequence`, and a site close otherwise. A recur site is never a
  direct child of a recur sequence item, whose children are iterations, so the two cannot collide.
- **The successor sets change in five places, and together they make `RecurEnd` mandatory.**

  | after | today | after the change |
  | --- | --- | --- |
  | entering a site (`expand_recur_entry_coordinates`) | `[s]` or `[s][1]` | `[s]` only — a site is entered through `RecurStart` |
  | `RecurStart` at `[s]` (the scope branch) | `[s]`, `[s][1]`, `[s+1]` or the parent's close | `[s][1]` or `[-s]` |
  | a recur tile iteration `[s][i]` (`cfs.rs:239`) | `[s][i+1]` or `[s]` | `[s][i+1]` or `[-s]` |
  | a recur sequence iteration close `[s][-i]` (`cfs.rs:217`) | `[s][i+1]` or `[s]` | `[s][i+1]` or `[-s]` |
  | `RecurEnd` at `[-s]` | — | `[s+1]` or the parent's close: a new arm |

  Today the ordering chain accepts a site with no `End` (`RecurStart` followed directly by
  `[s+1]`), and nothing at `ProgramEnd` requires the recur-progress stack to be empty, so the frame
  is simply never closed. That is harmless for storage, because no `End` means no object, but it
  means the close assertion of §The draft root rides in the site's recur-progress frame is not
  forced to run. With the sets above, the only way out of a site is through `RecurEnd`. The
  widening `recur-progress-commitment` §3.2.1 accepted, where `[s][1]` stays legal after the
  close, also disappears. `closing_coordinates_of` (`cfs.rs:578`) handles only recur-sequence
  iterations today and must also accept a recur site item; the scope branch already asks it for a
  scope's close.

**What moves.**

| area | change |
| --- | --- |
| `raster-core/src/trace.rs` | `RecurStart` and `RecurEnd` added to `StepKind`, with `RecurStartStep` and `RecurEndStep`; `ExecTarget::RecurTile` and `ExecTarget::RecurSequence` removed, and the comments that explain them (`trace.rs:457`, `:784`) rewritten; the accessors `input_commitment()`, `input_source_commitment()`, `output_commitment()` and `storage_roots()` gain arms — inputs only on `RecurStart`, output and storage only on `RecurEnd` |
| `raster-core/src/cfs.rs` | `closing_coordinates_of` accepts a recur site item; `try_get_next_coordinates` and `expand_recur_entry_coordinates` change the successor sets above |
| `raster-runtime/src/tracing/recorder.rs` | the `RecurTileStart`/`RecurSequenceStart` event arm records `RecurStart` with `site_id`, and `sequence_id` naming the enclosing sequence; the `RecurTileEnd`/`RecurSequenceEnd` event arms record `RecurEnd` at `[-s]`, write at `[s]`, and stop reading `fn_call_record.input` |
| `raster-macros/src/recur.rs` | the `RecurTileEnd` and `RecurSequenceEnd` events publish `input: None` |
| `checks/cfs.rs` | `record_matches_item` gains one arm, `(RecurStart \| RecurEnd, RecurTile(item) \| RecurSequence(item)) => site_id == item.id`, with any other item rejected, and loses `(SequenceStart, RecurTile)` and the two `Exec` site arms; `declared_sequence_id` loses its `RecurTile` arm; `advance_recur_progress` dispatches on the kind, takes the family from the item and passes `opened(coordinates)` to `close_site`; step-kind names in panic messages gain the two |
| `checks/store.rs` | `RecurEnd` requires exactly one write, at `opened(coordinates)` |
| `raster-cli/src/commands/run.rs`, `raster-prover/src/trace.rs` | witness builders learn the two kinds; `RecurEnd` gets no input source witness; the producer lookup and `record_produces_item` change as above |
| `raster-cli/src/commands/run.rs` (`step_coordinate_label`), `raster-cli/src/commands/fraud.rs` (`exec_target_name`, `exec_target_kind`) | lose the `RecurTile`/`RecurSequence` targets; a site's label comes from `RecurStart`/`RecurEnd`, and the family from the CFS where the printout wants it |

**Cost.** A trace-format break: `StepKind` gains two variants, `ExecTarget` loses two, and site
closes move coordinate, so every trace containing a recur site changes its fingerprint. Batch it
with the break §One storage rule already causes. The ordinary nested `SequenceEnd` still shares
`[s]` with its `SequenceStart`; moving it to `[-s]` is the same change and could ride along, but
nothing here needs it.

### What it needs

- **A creation record at the site's `Start`, and two assertions at its close.** Both steps already
  exist; neither carries a draft fact today. The close already holds the commitment
  (`output_commitment`), so what is missing is the comparison and the removal, not a step. Both
  go into the site's `RecurProgressFrame`: `push_site` opens the draft entry, and `close_site`
  compares and pops it. `advance_tile_iteration` chains it in between.
- **`RecurStart`/`RecurEnd` step kinds** for both recur site families, in place of the borrowed
  `SequenceStart` and `Exec`, with the close at `[-s]`. See §A recur site gets its own step kinds.
- **Two CFS facts on `RecurTileItem`**: whether the site owns an object (`!state_is_output`,
  already present) and whether it derives, and from which input (new).
- **Persist the append frontier** with the object, per append-only field. Without it a derived site
  must refold every base element to obtain a right edge — `O(N)` per extension, which defeats the
  sharing. It is `(len, leaf, ommers)`: **≤ 20 digests at 10⁶ elements**. Safe to store because it
  is *derived*, not authoritative — `verify_witness_root` already requires it to root to
  `root_before`, and `root_before` must equal the read-proven base commitment.
- **Lay append-only fields last in the payload**, independent of name order.
- **`rindex04` relative offsets** become an *optimisation* rather than a prerequisite under
  append-only derivation, since nothing earlier in the payload grows. They still matter for
  avoiding an `O(N)` re-assembly when a derived object is materialized contiguously.

### Still open

- ~~Who owns a draft no recur creates.~~ **Closed 2026-09-27** by §The restriction: a draft never
  crosses a step boundary. No draft exists outside a recur site, so the question does not arise.
  `hello-tiles/src/main.rs:85` is rewritten — a plain tile returns an ordinary value and a site
  derives from it — at the cost that a chain of plain tiles contributing to one *growing* object is
  no longer expressible.
- **Recur sequences.** This revision is written for `call_recur!`. The rule should extend
  unchanged, since a recur sequence site already lands its output at `[s]`, but the
  `SequenceEnd.output_commitment` question in §What must change is untouched here. The
  recur-progress frame already serves both site kinds, but a recur sequence has no replay journal
  to advance the frame's draft entry from, so it inherits the open question behind
  `recur-state-chaining`'s unpinned terminal `state_out`.
- **Whether intermediate objects should stay addressable.** A chain leaves `[s1]` readable at its
  own commitment forever. That is sound — every read names the commitment it read — but it grows
  the coordinate index by one per extension.
- **Where a creating site's empty root comes from at `Start`.** It is the root of the empty
  object of `S`, so it needs `S`'s schema, and today the schema first appears in iteration 0's
  witness (`pre_state.schema`). There are two options. The CFS item can record the output schema
  hash while the `Start` supplies the `SchemaNode` to hash against it. Or the frame can open as
  "empty, schema `h`" and let iteration 0 *adopt* its root after checking that its pre-state
  witness is empty, which is how `state_commitment` adopts at iteration 0. The second option
  still needs the schema at the close when there are zero iterations, so the first is the simpler
  rule.

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

**Superseded for anchoring, 2026-09-27.** The surviving variant's remaining purpose was to anchor
a window that opens mid-sweep through the storage frontier. §The draft root rides in the site's
recur-progress frame does that for one frame field per step, with no storage write and no update
gate. The measurements stand; the variant is no longer needed.

## Costs

- Draft runtime state grows: per append field, a payload buffer and a node arena alongside the
  values it already keeps. Peak memory is unchanged in order (the values dominate) but the constant
  rises.
- Remedy (b) is a `.rindex` format break — regenerate artifacts, `paged-bytes` sets the precedent.
- The lifetime change moves `finalize` out of the sequence grammar: every program with a draft
  re-locks, exactly as `draft-provenance` notes for its own smaller change.
- §One storage rule retires no assertion and adds no witness kind, but it does move every draft
  object's coordinate — a draft closed inside a recur site is charged to the site today via
  `current_recur_site_coordinates()` and would be *opened* there instead, and one closed outside a
  recur moves out of `[DRAFT_NAMESPACE, n]` entirely — so traces and commitments move for every
  program that builds a draft. It adds a persisted append frontier per append-only field
  (≤ 20 digests at 10⁶ elements) and a payload layout rule (append fields last); `rindex04` becomes
  an optimisation rather than a prerequisite.
- Derivation is restricted to `push`; **creation is not**. A site sets and pushes freely on its own
  object. `begin_decode_edge`-style seeding tiles fold into the creating recur tile, and a
  finalize stage like `output-finalize` still writes its summary scalars after its own sweep. Only
  a *derived* site is push-only, which is the narrowest restriction that keeps continuation a
  whole-object property.
- Every program with a draft is rewritten, not merely re-locked: `new!`, `finalize` and
  `finalize = false` all leave the language. Counted across `examples/`, `crates/raster/tests/` and
  `raster-inference`: **14** uses are `output = new!(T)` and become a bare `output`; **8** are handed
  to a `call!` tile, of which 7 are test scaffolding and 4 are real program shapes needing
  restructuring.
- The draft entry in `RecurProgressFrame` changes `recur_progress_commitment` for every step
  inside a sweep that builds an object: a trace-format break. It should be batched with the
  break §One storage rule already causes. At run time the recorder applies each iteration's ops
  once more to derive `root_after`. That costs `O(ops · log N)` per iteration, the same work the
  guest does for the step.
- The recur site step kinds add two `StepKind` variants and remove `ExecTarget::RecurTile` and `ExecTarget::RecurSequence`, which
  moves the fingerprint of every trace containing a recur site, recur sequences included. It should
  be batched with the same break.

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
- Under the derivation model, three assertions that do not exist today: a site's object coordinate
  is allocated at its `Start` step; a site's close returns a reference to that coordinate and no
  other; and a derived object's first `root_before` equals the root of the base object that its
  `Start`'s input binding proves. The last one is the anchor, and it is what lets
  `checks/drafts.rs:69`'s permissive `if let Some(..)` be deleted.
- Window-open attacks against the frame's draft entry, one per window start: a window opening at
  iteration *k* with a seed whose draft root differs from the honest one is rejected at its first
  step. So is a seed that omits the entry, and a window opening at the close whose seed root
  matches a root the tile legitimately produced but not the one this sweep ended on (the spliced
  chain `carried-state-channel` §Verification names).
- The close: a site whose stored object differs from its frame root is rejected, including a
  zero-iteration creating site whose object is not the empty object of `S`.
- Site step kinds: a site's inputs are bound at its `Start` only, and an `End` carrying input
  fields is not constructible; an iteration step after `RecurEnd` at `[-s]` is rejected by
  ordering, not only by `close_site`; a site `End` without exactly one write at `[s]` is
  rejected; a sequence-scope witness naming a `RecurStart` as its frame is rejected; a
  `RecurEnd`'s `sequence_id` naming anything but the enclosing sequence is rejected; a `RecurStart`
  or `RecurEnd` at a coordinate whose item is not a recur site is rejected.
- Site ordering: a trace that leaves a site without its `RecurEnd` (`RecurStart` then `[s+1]`), or
  enters it without its `RecurStart` (straight to `[s][1]`), is rejected by the ordering check; a
  zero-iteration site `RecurStart` → `RecurEnd` is accepted; a later sibling's `PriorItemOutput`
  input resolves to the `RecurEnd` record at `[-s]` and to the object at `[s]`.
- Recorder/guest parity: over a fixture with a creating site, a deriving site and a state-only
  site, the recorder's stamped `recur_progress_commitment` equals the guest's at every step.
- A sharing test: deriving `[s2]` from `[s1]` + k appends hashes **k** elements, not `len([s1]) + k`
  — the sibling of the element-hash counter above, and the thing `rindex04` exists to make true of
  the payload as well as the root.

## Not in this proposal

Draft→draft transformation (mapping every element of one draft into another) is
[`draft-map`](./draft-map.md). It needs a coverage rule this proposal does not supply.
