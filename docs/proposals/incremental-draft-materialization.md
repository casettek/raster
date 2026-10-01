# Proposal: `incremental-draft-materialization` — seal a draft instead of rebuilding it, and complete it at its recur site's close

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
(Corrected 2026-09-27: that holds for a struct with one growing list; with several, `rindex04`
relative offsets are what dissolve it, and they are now adopted — §Continuation on the draft
buffer.)

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
  on the *payload and index*, which that one left alone. It is a **precondition, and it is
  satisfied**: the frontier-based witness (`DraftWitnessField::Append` carrying an
  `AppendFrontier`) is what lets a draft step, and a deriving site's first step, carry `O(log N)`
  instead of every element. Its §5 (set-once fields as roots rather than whole values) was not done
  and is **not** required here; it would only shrink the first-iteration witness of a deriving site.
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
- Issues filed 2026-09-28 for soundness gaps found while deciding this proposal, each with its
  direction picked here: [`program-output-unbound`](../issues/program-output-unbound.md) (D5c),
  [`recur-carried-state-unbound`](../issues/recur-carried-state-unbound.md) (D5b, D5c) and
  [`replay-draft-schema-unbound`](../issues/replay-draft-schema-unbound.md) (D1).
- [`tile-output-commitment-unbound`](../issues/tile-output-commitment-unbound.md) (filed
  2026-09-28) — **to be resolved together with this proposal.** The object a tile step stores is
  never tied to the output its replay produced: `verify_io_witness` skips execution steps,
  `verify_storage_transition` ignores the output bytes, and nothing requires a tile with output to
  write. Measured with a guest probe. This proposal leans on stored commitments being what their
  producing tile computed — a deriving site opens at its base's commitment, a state-only site's
  close compares against its stored result — and it already breaks the same tile journal (D1, D2),
  needs the same shared encoder (D5b) and edits the same store check (D3).

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

> **At a recur site's close it is three times** (found 2026-09-27 by reading the code, not yet
> measured). In authenticated mode the recur driver builds the `RecurTileEnd` event's payload
> separately from the store: after the loop it calls `resolve_storage_value` on the stored
> result, which deserializes the whole object, and then `raster_trace_payload`, which runs
> `encode_raster_value` a second time on the same value (`raster-macros/src/recur.rs`, just
> before `RecurTileEnd` is published). So the element hashes are computed once per push, once by
> `store_value_at_coordinates` → `raster_payload_for_value` (`storage.rs:1429`), and once more
> for the trace. The seal has to hand its one payload to **both** consumers — the child's object
> store and the End event's `FnOutput` — or removing one encoding leaves the other in place.

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

The draft is then **sealed**: concatenate the field buffers, write the header, emit the index. No
element is re-encoded and no element is re-hashed. Today that moment is `finalize`; under §One
storage rule it is the recur site's close, where the seal runs in the child before `RecurTileEnd`
is published (§Where the seal runs).

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

Fields occupy consecutive regions in **declaration order**: the payload is encoded from the typed
value (`encode_raster_value` → `tree_value_from_serialize`), and `TreeStructSerializer::serialize_field`
pushes fields in the order serde visits them. Appending to a list therefore shifts every field
declared after it, and a struct is append-safe only if its one growing list is declared last.
`CollectiveGreeting { title, lines }` declares `lines` last, so it happens to be safe. (Corrected
2026-09-28: an earlier version said name order, from the draft's `BTreeMap`, which feeds only
materialization, not layout.)

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

> **Decided 2026-09-27: (b) is adopted, not optional.** Continuation shares a base object's bytes
> and index nodes with the object derived from it (§Continuation on the draft buffer).
>
> Sharing does **not** strictly need relative offsets. With absolute offsets it still works if
> every node addresses its own buffer: a base node keeps its offset into `[s1]`'s bytes, and the
> buffer is implied by the node-id range. (An earlier version of this note said absolute offsets
> would force rewriting every base node; that holds only if nodes address one shared logical
> space.) Relative offsets are chosen for three things absolute offsets do not give:
>
> 1. **One logical address space.** A node's position is `pos(parent) + rel(child)`, computed on
>    descent, and a single piece table maps logical positions to buffers. No node carries a buffer
>    tag, and a chain of derivations adds pieces, not addressing modes.
> 2. **Cheap export.** A derived object becomes a standalone contiguous `.rindex` artifact — a
>    program output, an `--input` for the next program — by concatenating its pieces. The index is
>    valid as it stands; with absolute offsets every node after the first grown region would be
>    rewritten.
> 3. **A seal with no fixup** for a creating site: per-field buffers concatenate without the
>    `O(#nodes)` offset pass remedy (a) leaves.

## The lifetime model this unlocks

> **Superseded 2026-09-24 by §One storage rule.** The rule below — the sequence wrapping a draft's
> creation closes it — was replaced: a draft exists only inside a recur site, and the site
> completes its own object at its own close. The seal survives unchanged; only the moment it runs
> moved. This section is kept because §What it fixes records how the creation-step question was
> found and answered.

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
  yes; that root is what the enclosing frame receives and later seals. (Under §One storage rule
  the site seals its own object at its close, and the root check lands at `RecurEnd`.)

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
  > **Dissolved for recur sequences too, 2026-09-28** (§Recur sequences): an iteration whose body
  > returns the draft publishes no output; the draft's progress lives in the frame.
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
  it, the site's close completes it, and the site returns `AuthRef<S>`. No user-visible close call —
  the site's close *is* the completion, and the seal of §Mechanism runs inside it.
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

> **Amended 2026-10-01 (batch B): a draft may also live inside one plain tile.** The replacement
> below — "a plain tile returns an ordinary value" — cannot compile: `bounded-collections` makes a
> struct with a `List` field non-`Materializable`, so no tile may return it. Instead a plain tile
> may create a draft with `Draft::<S>::new()`, populate it, and **return** it; the tile's close
> completes it into an `S` stored at the tile's own coordinate, and the caller receives that
> object. Nothing open crosses the boundary — the rule above still holds for every step boundary.
> What makes a `List`-bearing tile output acceptable is the **draft budget**
> (`DRAFT_STEP_BUDGET`, 64 KiB of `set`/`push` payload per tile run, times the consumed elements
> of a chunked iteration), enforced by the shared `Draft` code in the native run and the replay.
> A recur site continues such an object by derivation (`output = base`), as below.

> **Amended 2026-09-28 for recur sequences (D5a): the boundary is the *site's*, not every step's.**
> A recur sequence's body is ordinary sequence code that hands the site's draft to plain tiles
> (`append_activation_row(output: Draft<ActivationSequence>, …) -> Draft<…>` in `raster-inference`'s
> `input-embedding`). Inside the site's scope, `[s]` to `[-s]`, the draft may pass between the
> body's steps; it never leaves the scope. See §Recur sequences.

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
| `raster-macros/src/lib.rs` (tile wrapper) | a recur-iteration tile — one whose return carries the site's draft or its carried state — publishes `output: None`, and its replay emits `output_bytes = []` — see §What actually happens during a sweep |
| `raster-runtime/src/storage.rs` (draft identity) | the draft's `Anchor` becomes `anchor_for_schema([s], S::schema_hash())`, the site's own coordinate — see §Draft identity |
| `raster-core/src/draft.rs`, `raster/src/input.rs` | `draft_id` removed from `DraftReplayHandle` and `DraftReplayTransition`; the replayed `Draft` gets a constant anchor — see §Draft identity |
| `raster-runtime/src/storage.rs` (draft buffer) | `THREAD_DRAFT_STORAGE` → a per-site `DraftBuffer`, holding the encoded per-field buffers of §Mechanism alongside the frontiers and op log; consumed once by the seal — see §The draft buffer |
| `checks/store.rs` | an `Exec` step without a storage write must have an empty `output_commitment` |
| `raster-core/src/cfs.rs` (`SequenceDef`), `raster-compiler` (flow resolver) | `returns: Option<InputBinding>`, resolved from the body's returned expression; an unresolvable return is a build error; `produces_output` becomes `returns.is_some()` — see §Sequence return binding |
| `raster-core/src/trace.rs` (`SequenceEnd`), recorder | `SequenceEnd` records `output: Option<StorageData>` and `output_commitment = selected_hash` |
| `checks/entrypoint.rs`, `checks/cfs.rs` | `ProgramEnd` and `SequenceEnd` checked against `returns`; a consumer of a sequence item cites exactly its returned binding |
| `raster-core/src/recur_progress.rs` | `state_commitment` becomes the value's raster root; the frame opens its state at `push_site` from a stored seed's binding; `close_site` checks a state-only site's result — see §Carried-state commitment |
| `raster-macros` (tile replay, sequence step) | the replay computes `state_in`/`state_out` as raster roots; a recur sequence iteration binds its state as a reference, not inline bytes |
| `checks/cfs.rs` (`assert_carried_state_matches_input`, `fold_sequence_iteration_state`) | a sequence's `state_in` is the `Start`'s state reference; its `state_out` is the End's returned binding |
| `checks/drafts.rs` | keeps the per-step `root_before → ops → root_after` check against the witness; loses `active_drafts` and the permissive `if let Some(..)` at `:69`. The expected `root_before` now comes from the site's frame |
| `raster-core/src/recur_progress.rs` | `RecurProgressFrame` gains the draft entry: opened by `push_site`, advanced by `advance_tile_iteration`, compared against `output_commitment` and popped by `close_site`. See §The draft root rides in the site's recur-progress frame |
| `raster-core/src/transition.rs` | `active_drafts` removed from `Transition` and `InitTransition`, together with `TrackedDraftState` |
| `raster-core/src/cfs.rs` (`RecurTileItem`) | `leaves_output_open` deleted; `output: Option<RecurOutputDecl { schema_hash, empty_root, derives_from }>` added, so the opening root is not prover-chosen — see §What must come from the CFS |
| `raster-compiler` (`CfsBuilder`) | fills `RecurOutputDecl.schema_hash` and `empty_root` with `schema_walk` over the site's output type |
| `raster/src/input.rs` (`restore_draft_from_replay_handle`) | asserts `handle.schema_hash == S::schema_hash()` instead of overwriting it |
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

Under this proposal a mid-sweep iteration writes **nothing to storage**. The recorder's write is
conditional on the event carrying an output value:

```rust
let storage_write = output.as_ref().map(|output| {
    self.storage.append_serialized_bytes(&output.data, tile_coordinates.clone(), output.raster.clone())
});
```

> **Corrected 2026-09-27: today every iteration does write.** An earlier version of this section
> said an iteration handing back an open draft *"carries no output, so no write occurs."* That is
> not what the code does. The tile wrapper attaches an `FnOutput` to **every** tile event, whatever
> it returns (`raster-macros/src/lib.rs`, the native wrapper). A draft-returning tile's result is
> the `Draft` handle, and `Draft` serializes as a fixed marker, `{ kind: "raster::Draft", schema:
> <type name>, reusable: false }` (`raster/src/input.rs:317`). So the recorder appends that marker
> as an object at `[s][i]` on every iteration. Checked on a real trace: all 7
> `RecurTileIterationExec` frames in the latest `hello-tiles` `trace.bin` carry it. A sweep of N
> iterations is **N + 1** authenticated appends today. At the measured 71.6 µs per append (93% of
> it the coordinate index), that is ~7 s per 100 K iterations — the cost §The original cost
> measurement rules out for per-iteration updates, spent on an object that carries no information.
>
> **What makes "no write per iteration" true.** The host cannot simply drop the output. For every
> tile step the transition guest checks `replay_journal.output_bytes == recorded output witness`
> (`checks/io.rs`), and the replayed tile serializes its `Draft` result as the same marker, since
> `Draft`'s `Serialize` impl is not target-specific. Both sides have to change together:
>
> - **The replay wrapper** emits `output_bytes = []` for a tile whose return carries the site's
>   draft — `Draft`, `RecurControl<Draft>`, `(RecurState, Draft)` and `RecurControl<(RecurState,
>   Draft)>`. Nothing is lost: the draft's effect is the journal's `draft_transition`, the control
>   is `recur.control`, and the carried state is `recur.state`.
> - **The native wrapper** publishes `output: None` for the same return kinds, so the recorder's
>   `output.as_ref().map(..)` writes nothing and `exec_step` leaves `output_commitment` empty.
>
> The guest then accepts the step with no other change: the I/O check compares an empty journal
> output with an absent witness (`unwrap_or(&[])`), and `verify_storage_transition` takes its
> no-write branch, which requires the storage and index roots to be unchanged.
>
> **One guest check to add.** That no-write branch does not constrain `output_commitment`, and the
> I/O check skips execution steps. Today the branch is rare — every tile event carries an output —
> so the gap is small; once every recur iteration takes it, an unconstrained 32-byte field on every
> iteration record is free entropy for manufacturing a divergence. Rule: **an `Exec` step without a
> storage write must have an empty `output_commitment`.**
>
> That rule is necessary, not sufficient. It neither requires a write when the replay produced
> output nor ties a write's commitment to that output, so a step with a forged commitment, or with
> its write dropped, still verifies —
> [`tile-output-commitment-unbound`](../issues/tile-output-commitment-unbound.md). What closes both
> lands with this rule: a tile step writes exactly when its journal's `output_bytes` is non-empty,
> and its write's commitment is bound to that output. How the binding is made is the issue's open
> choice (§Directions there).
>
> **State-only iterations are included** (§Still open, D3 — decided 2026-09-28). A tile returning
> `RecurState<T>` or `RecurControl<RecurState<T>>` also writes its returned state at `[s][i]`
> today, and nothing reads it: the next iteration receives its state *inline* in its input
> (`FnInputValue::Inline`), and the chain is enforced on commitments — the replay computes
> `state_in` from that input and the frame requires it to equal the previous `state_out`. Such a
> tile likewise publishes `output: None` and replays `output_bytes = []`. Recur *sequences* are
> unaffected: their iterations close with `SequenceEnd`, which writes nothing.

The single write happens at the site's close (`RecurTileEnd`, `recorder.rs:1043`), the same step
whose `output_commitment` is set from `storage_write.entry.object_commitment`.

Two accumulators therefore advance at different rates, and conflating them is easy:

| per iteration | per site close |
| --- | --- |
| one step record | one step record |
| one `hash_trace_item` → trace frontier | — |
| one fingerprint entry | one fingerprint entry |
| **no** storage write (today: one, the draft marker — see above) | **one** storage write: object + log append + index insert |

Under this proposal a 1000-iteration sweep is 1001 trace steps and 1001 fingerprint entries, and
**one** storage write (1001 today).
The trace records the *process*; storage records the *result*. That is why every draft mutation is
fingerprint-bound without touching storage, and why
[`recur-deferred-finalize`](./recur-deferred-finalize.md) could say deferring the close
*"moves no attestation; it moves only the materialization."*

### Where the seal runs

The seal of §Mechanism is a child-process operation. It is not a step, not a trace event and not
something the guest checks directly. Under this rule it has exactly one place to run: the site's
close, just before the child publishes `RecurTileEnd`.

| moment | child process | recorder | guest |
| --- | --- | --- | --- |
| `RecurTileStart` → `RecurStart` at `[s]` | creates the draft with empty per-field buffers; a deriving site loads the base object's persisted frontier and buffers | opens the frame and its draft entry | opens the entry: empty root, or the base's commitment |
| each iteration → `Exec(Tile)` at `[s][i]` | push: frontier, payload bytes, node arena and Merkle spine updated incrementally | records the witness; advances the frame's root | replays the ops: `root_before` equals the entry, and the entry becomes `root_after` |
| site close → `RecurTileEnd` → `RecurEnd` at `[-s]` | **seal**: join the buffers into object bytes, index and root; the same payload goes to the child's store and into the End event's `FnOutput` | appends the bytes at `[s]` and sets `output_commitment` | `entry.root == output_commitment`, then pops the frame |

(`RecurStart`, `RecurEnd` and `[-s]` are §A recur site gets its own step kinds; the seal is
indifferent to them and would run at the same moment with today's kinds.)

Two consequences:

- **The close assertion checks the seal.** A seal whose bytes differ from a full encoding produces
  a commitment that does not match the replay-derived root, so the guest rejects even an honest
  trace. The byte-identity invariant in §Verification is therefore enforced at `RecurEnd`, not
  only by tests — a seal bug shows up as a rejected honest trace, never as an accepted wrong one.
- **One payload, two consumers.** The seal's output must feed both the store and the trace event.
  Today the trace side re-encodes independently (§The finding that motivated this), so a seal that
  replaces only the store's encoding would leave one full re-hash per close in place.
- **The recorder checks it first.** `close_site` is shared `raster-core` code, and the recorder
  calls it at the site's close as the guest does. With the draft entry in the frame, the
  comparison `draft.root == output_commitment` therefore also runs **at record time**, before
  anything is committed: a seal whose root disagrees with the replayed ops panics in the recorder
  rather than producing a trace no guest accepts. What it covers is the *root*: the recorder takes
  an object's commitment from the payload's `root_hash` (`internal_object_commitment`) and does not
  rehash the bytes, so bytes that disagree with their own root surface only when something reads
  them with a selection proof.

### Recur sequences (D5a)

**Decided 2026-09-28.** A recur sequence builds its draft in its body's plain tiles, and those
tiles are replayed: each `Exec(Tile)` at `[s][i][j]` carries a draft transition in its journal.
So a recur sequence needs no journal of its own to advance the frame's draft entry — the body's
tiles are that journal.

- **Scope rule.** A draft never crosses a *site* boundary (§The restriction, amended). Inside a
  recur sequence's scope the body may hand the site's draft to plain tiles; a nested ordinary
  sequence may not take it, as today.
- **Frame rule.** Any `Exec(Tile)` whose journal carries a draft transition advances the draft
  entry of the **innermost live site frame** whose coordinates contain the step. That covers a
  recur tile's iterations and a recur sequence's body tiles with one rule. A nested recur tile
  inside the body has its own frame; once it closes, the innermost frame is the sequence's again.
- **Opening and closing** are the recur tile's: `RecurStart` opens the entry from
  `RecurSequenceItem.output` (D1's declaration, on both recur item kinds), and `RecurEnd` checks
  `draft.root == output_commitment` and pops.
- **No output for a draft-returning step** (D3, generalized): any tile whose return is the site's
  draft publishes no output, and so does a recur sequence iteration whose body returns the draft.
  This dissolves §What must change's open question — what `SequenceEnd.output_commitment` commits
  to for an open draft: nothing, because the draft's progress lives in the frame.
- **Seeding tiles become values.** `begin_ple_layer(new!(PleLayerInputs), …)` and
  `begin_layer_output(new!(ActivationSequence), …)` become plain tiles returning a value, and the
  site derives from it; both only set scalars, so a push-only deriving site suffices.
  `prefill-range`'s two writers on one draft (`carry_cached_key` with `finalize = false`, then
  `attend_token`) become a chain of two derivations.
- **Non-goal: a nested recur tile appending to the enclosing site's draft.** Possible today with
  `output = output, finalize = false` inside a body; used nowhere in the tests, `examples/` or
  `raster-inference`. A nested site owns its own result. If it is ever needed, it is a *borrowing*
  site: one that owns no object, advances the owning frame's draft entry, and whose `RecurEnd`
  writes nothing.

Full event sequence, for `main = [ per_row ]` with `per_row` a recur sequence over L = 2 rows whose
body runs a nested recur tile `mac` over K = 2 weights and then `append_row(output, acc)`:

```
ProgramStart                                          []
├─ RecurSequenceStart ─► RecurStart     [1]           opens SEQUENCE site per_row (draft entry opens)
│  ├─ RecurSequenceIterationStart ─► SequenceStart  [1,1]    iteration 1
│  │  ├─ RecurTileStart ─► RecurStart   [1,1,1]       opens TILE site mac (state only)
│  │  │  ├─ RecurTileIterationExec ─► Exec(Tile)  [1,1,1,1]   no write (D3)
│  │  │  └─ RecurTileIterationExec ─► Exec(Tile)  [1,1,1,2]   no write (D3)
│  │  ├─ RecurTileEnd   ─► RecurEnd     [1,1,-1]      closes mac → writes Acc at [1,1,1]
│  │  └─ TileExec (append_row) ─► Exec(Tile)  [1,1,2] no write; advances per_row's draft entry
│  ├─ RecurSequenceIterationEnd ─► SequenceEnd  [1,-1]       no output
│  ├─ RecurSequenceIterationStart ─► SequenceStart  [1,2]    iteration 2 — same shape at [1,2,…]
│  │  …                                                       writes Acc at [1,2,1]
│  └─ RecurSequenceIterationEnd ─► SequenceEnd  [1,-2]
├─ RecurSequenceEnd   ─► RecurEnd       [-1]          seal; writes the object at [1];
│                                                      draft.root == output_commitment
ProgramEnd                                            []
```

The step kind `RecurStart`/`RecurEnd` is the same for both families; the CFS item at the coordinate
says which (`[1]` is a `RecurSequence` item, `[1,1,1]` a `RecurTile` item). Three writes in all —
each iteration's `Acc`, which `append_row` reads, and the site's object — against
L·(K + 2) + 1 = 9 today.

### Sequence return binding (D5c)

**Decided 2026-09-28.** A reference names a stored object; the guest can only check it as
precisely as the CFS describes where it came from:

| a step's argument comes from | what the CFS records | what the guest checks | precise? |
| --- | --- | --- | --- |
| a tile or recur tile, item `[j]` | "item `j`'s output" | coordinates **equal** `[j]` — a tile has one output, at its own coordinate | yes |
| a sequence, item `[j]` | "item `j`'s output" | coordinates merely **inside** `[j]` (the `Sequence \| RecurSequence` arm of the prior-item-output check in `checks/cfs.rs`) | no |
| `main`'s return, at `ProgramEnd` | only `produces_output` | the object is stored and the selection is valid | no |

A sequence writes many objects under `[j]`, one per tile it calls, and which one it returns is
decided by its body — which the CFS does not record: `SequenceDef` has no return binding, and the
flow resolver resolves call *arguments* (`resolve_argument`) but never a body's returned
expression. So a dishonest trace can substitute any object the sequence wrote:

```rust
#[sequence]
fn inner(list: …) -> AuthRef<Out> {
    let a = call!(prepare, list);                    // writes A at [2,1]
    let b = call_recur!(tile = t, input = list, …);  // writes B at [2,2]
    b                                                // returns B
}
#[sequence]
fn main(list: …) -> … {
    let r = call!(inner, list);    // item [2]
    call!(consume, r)              // item [3]: should read B
}
```

A trace in which `consume` cites A at `[2,1]` passes every check — inside `[2]`, stored, validly
selected, replay input equal to recorded input — and claims `consume(A)`, a computation the program
never performs. `ProgramEnd` is looser still. **Measured**: a throwaway guest probe gave
`verify_program_end` an output object at `[7]`, a coordinate naming no CFS item, and it returned
`Established`. `program-end.md` §7's argument that a forged output "diverges from the fingerprint,
which is fraud-provable" needs the guest to reject the forged step, which it does not. A recur
sequence's carried state is the same gap in inline form: an iteration's returned state is its
`SequenceEnd` output bytes, checked only against their own hash, so the chain of `state_out`s is
consistent with itself but not with what the body computed.

**The design.**

- **The CFS records each sequence's return — its source and its selector path**, resolved from
  the body's returned expression by the same `resolve_argument` used for arguments: a prior item's
  output, a sequence parameter, an entry argument. The source alone is not enough. An
  `InputBinding` carries no path — the CFS binds provenance, not shape — so a return recorded as a
  bare `InputBinding` pins *which object* and leaves *which part of it* free: for `main` ending in
  `select!(u64, stats.count)`, a `ProgramEnd` selecting `stats.sum` would verify. The return
  therefore records the `select!` path as well (static by the grammar; a data-sourced index is
  refused, having nowhere to carry its citation), and the guest compares it with the recorded
  selector. `produces_output` becomes `returns.is_some()` for `main`. The grammar makes this a
  single binding: the only return form is *"last expression = a binding or call result"*, and
  control flow is forbidden in sequence bodies (`.claude/skills/raster/SKILL.md`, the sequence
  grammar table). A returned expression the resolver cannot resolve is a **build error**, never
  `Inline` — `sequence-grammar-closure`'s rule applied to returns. (Until `finalize` leaves the
  language, `5e0bf83` makes it a build *warning* for `main` instead, so a program returning
  `finalize(draft)` still builds and runs unauthenticated; its `ProgramEnd` fails closed.)
- **Storage-backed returns are resolved statically, through the CFS** (revised 2026-09-28; this
  replaces a runtime check at each `SequenceEnd`). A sequence body has no control flow, so which
  step's output it returns is fixed at build time. `returns` is recorded *relative* to the
  sequence — an item index, never a coordinate, because one definition is called from many places
  — and the guest follows it from any consumer down to the step that wrote the object
  (`CfsCursor::resolve_value`): `PriorItemOutput(k)` names item `F ++ [k+1]`, where a tile, recur
  tile or recur sequence wrote its value exactly; a nested sequence hands on its own `returns`,
  prepending its path; a returned parameter continues at the caller's argument; an entry argument
  is the entry object at `[]`. No trace or witness field is involved, so the check also holds in a
  fraud window that contains no `SequenceEnd`.
- **`ProgramEnd` and every consumer of a sequence item cite exactly the resolved object**,
  replacing the "inside `[j]`" prefix check. A re-return is one more hop: `main` → `inner` → `b`
  resolves to exactly `[2,2]`. Measured on `hello-tiles`: `main` returns through three nested
  sequences to `exclaim`'s object at `[21,7,4,1]`, and every one of the trace's 32 prior-item
  arguments resolves to the coordinates the run recorded.
- **`SequenceEnd` records a returned binding only for inline values** — a recur sequence's carried
  state (D5b), the one return the static walk cannot follow because it is not a stored object.
- **A nested sequence may not return an inline value**, the rule `main` already has. A recur
  sequence's returned state is a body tile's output (`advance_word_cursor` in
  `crates/raster/tests/recur_draft.rs`), so it resolves to a prior item's output and is bound.
- **A recur site needs no return binding**: its result is its own object at `[s]` (drafts), or its
  state (D5b).

**Order.** `ProgramEnd` first, as a standalone fix — `main`'s `returns` and one check in
`verify_program_end`. It is a soundness gap in shipped code and the smallest piece, like D1's
replay assertion. Nested sequences and consumers follow with the step-kind change, since
`SequenceEnd` moves to `[-s]` in the same break.

**Implemented so far — 2026-09-28: storage-backed returns, static.** In three steps: `main`'s
return source (`5e0bf83`); its selector path, with the selection proof made unconditional — a
zero-length selection used to skip it, leaving the output commitment free (`923aa9f`); then
`returns` for every value-returning sequence (a recur-sequence body excepted — its site is the
leaf) and the static walk, used by `ProgramEnd` (fails closed on any unfollowable chain) and by
every prior-item argument. Not yet:

- **A path through a parameter is only a suffix.** Argument bindings carry no path, so once the
  walk crosses a sequence parameter only the trailing segments are known (`path_complete =
  false`); prior-item arguments get no path check at all. Giving bindings a path is a separate
  change, and also closes [`sequence-scope-forbids-narrowing`](../issues/sequence-scope-forbids-narrowing.md).
- **Two chains are not followed**: a nested return the CFS could not bind, and a recur-sequence
  body's parameter. `ProgramEnd` fails closed on both; a prior-item argument falls back to the
  old "inside the source item" rule, so no honest program regresses.
- **Inline returns** (recur-sequence carried state) — D5b.
- **The write itself.** The walk pins *where* the object was written, which is as good as the
  write — [`tile-output-commitment-unbound`](../issues/tile-output-commitment-unbound.md).

Each step moved the `program_commitment` of every program whose CFS it changed (the CFS is
postcard-encoded) — the first two every program with an output, the third only programs with
nested sequences (`hello-tiles` among the examples). Details in
[`program-output-unbound`](../issues/program-output-unbound.md).

### Draft and carried state are different things

They meet in the same frame and at the same `RecurEnd`, which makes them easy to conflate. They
should not share a mechanism, a commitment or a check.

| | draft | carried state |
| --- | --- | --- |
| what it is | the site's **output object** under construction | a **value threaded between iterations** |
| how it changes | by ops — `set` and `push` — inside tiles | replaced whole: each iteration returns the next value |
| where it lives between steps | the child's `DraftBuffer`, never in the trace | recur tile: inline in the next iteration's input. Recur sequence: a **reference** to the previous iteration's returned object (D5b) |
| frame field | `draft: Option<SiteDraft { schema_hash, root }>` | `state_commitment: Option<Hash32>` |
| per-step link | `root_before == frame root`; replay proves `root_after` from ops | `state_in == frame commitment`; `state_out` becomes it |
| commitment | the object's structural root, the same function as its raster commitment | the state value's **object commitment**, the raster root of `T` (D5b; today `H("recur-carried-state" ‖ postcard)`) |
| what the site returns | the object, at `[s]` | the final state, at `[s]`, for a state-only site; discarded for a state+output site |
| terminal check | `draft.root == output_commitment` at `RecurEnd` | a state-only site: `frame.state_commitment == output_commitment` at `RecurEnd` (D5b) — a separate field compared by a separate rule |

A state+output site has both, independently: the draft is checked by the draft rule; the state is
chained per iteration and, since the site discards it, has no terminal check to make.

### Carried-state commitment (D5b)

**Decided 2026-09-28** (resolves D3′). A carried state is committed by the state value's **object
commitment** — the raster root of `T`, over the inner `T`, not `RecurState<T>`, whose raster
encoding adds the field name `inner`. That is the function storage already uses for any stored
object, which is what makes a state comparable with the result a site stores.

| | recur tile | recur sequence |
| --- | --- | --- |
| `state_in` | computed in the replay from the typed input value | the commitment of the **reference** the iteration's `Start` binds: the previous iteration's returned object, or the seed |
| `state_out` | computed in the replay from the returned value | the commitment of the iteration's returned binding, which §Sequence return binding binds to a body tile's output — no hashing, no type needed in the guest |
| frame opens at `RecurStart` | from the seed's binding when the seed is stored — authenticated by `RecurStart`'s storage read; adopted from iteration 0 only for an inline literal seed | the same |
| terminal check, state-only site | `frame.state_commitment == output_commitment` at `RecurEnd` | the same |
| state+output site | chained per iteration; discarded, so no terminal check | the same |

Recur sequence state therefore passes between iterations by reference rather than as inline bytes.
The `{6}`/`{7}` substitution of D3′ is rejected at `RecurEnd`, for tiles and sequences alike.

**What it also closes.** Both of `recur-state-chaining`'s stated non-goals, in part. A stored seed
is now pinned — `attend_token`'s `scores` chain in `raster-inference`, each site seeded by the
previous site's result, and `scan_all_words`'s `begin_word_cursor` — because the frame opens from
the seed's binding instead of adopting what iteration 0 reports; an inline literal seed stays
unpinned. And a state+output recur sequence's final `state_out` is no longer free: it is a body
tile's bound output.

**Rejected.** Keeping `H(postcard)`: the terminal check is impossible for recur sequences, since the
guest cannot turn postcard bytes into a raster root without knowing `T`. The selection hash of the
state's raster payload: computable without the type, but the stored result is committed by its
raster root, so the close would have to carry the stored object's payload to compare.

**Requires.** The replay's raster root must equal storage's for every state type. The probe in
§Verification showed it for a struct of a `String` and a `List<String>` only; a type-coverage test
(enums, maps, integers, `Bytes<N>`, nesting) must pass before relying on it. On a mismatch, the fix
is one shared encoder in `raster-core`, used by both the replay and storage.

### Draft identity

A draft is keyed by an `Anchor` in `THREAD_DRAFT_STORAGE`, and the replay journal's
`DraftReplayTransition.draft_id` carries it. Today the anchor is
`anchor_for_schema(coordinates, schema_hash)` over the **synthetic** coordinate
`reserve_synthetic_coordinates` mints, `[…, DRAFT_NAMESPACE, n]`. Deleting the namespace deletes
that input, so the anchor needs a new one.

**Rule: `anchor = anchor_for_schema([s], S::schema_hash())`**, the site's own coordinate. It is
unique, because a site owns exactly one draft and no two live sites share `[s]`, and it needs no
counter. It is a **host-side** key only: it names the site's `DraftBuffer`, and it does not enter
the trace.

**`draft_id` leaves the trace — decided 2026-09-28** (§Still open, D2). Its only reader in the
guest was the `active_drafts` map key (`checks/drafts.rs:69,84`), which the frame design deletes: an
iteration at `[s][i]` finds its draft through the site frame, and a tile step carries at most one
draft, since a `Draft` appears only in a recur tile's output slot. Everywhere else it was passed
through: the replay tile read it from the handle in its input (`Draft::new(handle.draft_id, …)`)
and echoed it into the journal, and the native witness copied the host anchor
(`raster-runtime/src/storage.rs:1254`). Left in place it would be a host-chosen value reaching a
trace leaf that nothing checks — the free-field entropy `verify_sequence_id` is written to exclude.

So `draft_id` is removed from `DraftReplayHandle`, from `DraftReplayTransition` (the replay
journal's and the native witness's), and with `active_drafts` from the guest. The replayed `Draft`
is constructed with a constant anchor, which it never uses — the replay has no draft buffer. The
alternative, checking `draft_id == anchor_for_schema(frame.site, output.schema_hash)`, was rejected:
it would move `anchor_for_schema` into `raster-core` only to validate a field nothing reads. The
change moves the tile guests' journal format and image ids, which D1's schema assertion and the
step-kind change move anyway.

### The draft buffer — why a draft is not an entry in the object store

The child process keeps two stores, and it is fair to ask why a draft does not simply live at `[s]`
in the first one and get updated per iteration.

| | `THREAD_STORAGE: ObjectStore` | `THREAD_DRAFT_STORAGE` |
| --- | --- | --- |
| entry | `StoredObject { reference: (coordinates, commitment), backing: bytes + raster payload }` | `DraftRuntimeState { schema, current_root, fields, ops }` |
| form | **encoded and final**: postcard bytes plus the raster index and root, built in one pass by `encode_raster_value` | **working form**: a set-once field as a value tree with its root; an append field as `values: Vec<DraftValue>` plus an `AppendFrontier` |
| mutability | write-once: `put` asserts no earlier write at the coordinate | changed on every `set`/`push` |
| key | coordinate, read through a `StorageRef` that pins the commitment | `Anchor`, which no step can select |
| authenticated structures | none since `storage-role-split`: log, index and roots live only in the recorder | none |

**What the draft buffer is for.** It is not only a root tracker. It does four things `ObjectStore`
cannot:

1. **It accumulates values until materialization.** Under `--no-auth`, where roots are skipped,
   this is all it does.
2. **It tracks the root cheaply.** A push hashes the new element once and moves the frontier —
   `O(log N)`, plus `O(#fields)` to recompose. The root is needed per op (the handle's
   `expected_root`), in the tile's input (`root_before`), and in the witness (the pre-state must
   root to `root_before`).
3. **It produces the per-step witness.** `ops` and the pre-state snapshot (schema, set values,
   frontiers) are what give the guest `root_before → ops → root_after` for each iteration.
   `ObjectStore` has no op log.
4. **It keeps the half-built value unreachable.** Keyed by `Anchor`, it cannot be selected by any
   step; the object at `[s]` appears only at the close.

**Why not update the object at `[s]` per iteration instead.** The answer differs by layer:

| layer | per-iteration update would cost | verdict |
| --- | --- | --- |
| child `ObjectStore` | a full re-encode per push. No index or root is involved on this side, but the stored form is the encoded payload, and today it is not appendable — `RasterNode.offset` is absolute and struct fields are laid out sequentially (§The two things that do not permit it). So each push reruns `encode_raster_value` over the whole object: `O(N)` per push, `O(N²)` per sweep. At the measured ~1.5 µs per element, a 16 384-element sweep would spend about N²/2 × 1.5 µs ≈ **200 s**, against ~25 ms for build-then-seal (an estimate from the measured rate, not a run) | ruled out |
| recorder `AuthenticatedObjectStore` | a traced write per iteration: log append plus index insert, 71.6 µs, 93% of it the index; an update-witness kind; a storage gate | ruled out, §The draft root rides in the site's recur-progress frame |
| transition guest | nothing either way: it verifies ops against roots and first sees the object in the close's write witness | — |

**The seal makes the question a matter of placement.** Once §Mechanism is in place, each append
field keeps its *encoded* payload bytes, node arena and Merkle spine up to date as elements are
pushed. The draft buffer then *is* the object under construction, in appendable form, and the seal
only joins the buffers and hands them to `ObjectStore` at `[s]`. At that point it could in principle
be a mutable `ObjectStore` entry. It stays separate because `ObjectStore`'s contract is that a
`StorageRef` — coordinates plus commitment — pins immutable bytes. A mutable entry would change its
commitment on every push, breaking that contract for the length of the sweep, and it would need the
duplicate-write assertion relaxed and a new rule that nothing may read `[s]` yet. A separate buffer
keeps the invariant for free, and the recorder never has to mirror it.

**Rename.** `THREAD_DRAFT_STORAGE` becomes a per-site **`DraftBuffer`**: keyed by the site's anchor
(§Draft identity); holding the values or encoded buffers, the frontiers and the op log; created at
the site's `Start`; handed to `ObjectStore` exactly once, by the seal at the site's close. "Storage"
suggests an addressable store alongside `ObjectStore`, which is exactly what it must not be.

### Three representations, and why none is redundant

The same draft is tracked in three places, differently, and the reason each exists is worth having
written down:

| | holds | why it cannot be elsewhere |
| --- | --- | --- |
| **child process** | full state: `values` **and** `AppendFrontier`, in `THREAD_DRAFT_STORAGE` keyed by `Anchor` | it has to *compute*. The recorder is downstream and one-way: it runs in the `cargo raster run` process and replays `trace.bin` after the program has exited (`run.rs`, `child.wait()` before `load_trace_from_file`). Under `--no-auth` there is no recorder at all |
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
2. *The layout blocker stops binding* — through relative offsets (`rindex04`, adopted in §Two
   remedies), not through field order. With offsets relative to the parent's region, an append
   shifts no node's recorded offset for any number of lists; four of the eight `raster-inference`
   recur outputs in the table below have two or more (`KvSequence`, `PleLayerInputs`,
   `PrefillLogits`, `ActivationSequence`).

   > **Corrected 2026-09-28.** An earlier version argued that layout and hash order are decoupled,
   > so append-only fields could simply be laid out last. Both premises are wrong. Fields are laid
   > out in declaration order (§The two things that do not permit it), and the raster root folds a
   > struct's children in that same order (`assemble_subtree` → `struct_commitments_root`), so
   > reordering the layout would change the object's commitment. The draft root's schema order
   > matches it — measured, §Verification.

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
derived site touches only the tail of each list, so continuation stays a whole-object property, and
with relative offsets no base node is rewritten. `DecodeEdge`'s chain is already push-only downstream —
`append_selected_token` (`decode-select-token/src/lib.rs:96`) does nothing but
`generated_token_ids().push(..)`.

### Continuation on the draft buffer

**Added 2026-09-27.** How a deriving site runs, with the pieces defined elsewhere in this
proposal — `RecurStart`/`RecurEnd`, the `DraftBuffer`, the frame's draft entry and the seal:

```rust
let base = call!(begin_greeting, title);                                        // object at [3]
let g = call_recur!(tile = add_line, input = lines, output = base, args = ());  // object at [4]
```

| step | user side (`DraftBuffer`) | recorder | guest |
| --- | --- | --- | --- |
| `RecurStart` at `[s2]` | create the buffer, anchor `anchor_for_schema([s2], S)`, **initialized from the base**: read `[s1]` from `ObjectStore`; take each list's frontier and each field's root from its raster index; decode the set-once fields only | open the frame's draft entry: root = base commitment, `derived = true` (from the CFS) | the same, with the base commitment authenticated by `RecurStart`'s storage read of `[s1]` (a CFS binding alone pins its coordinates, not its commitment) |
| iteration | `push` only: hash the new element, move the frontier, append its encoded bytes and nodes to that list's tail | `root_after = apply_draft_ops(pre_state, ops)`; the frame root advances | `root_before == frame root`; ops apply; **no `Set`** |
| `RecurEnd` at `[-s2]` | seal: the delta — each list's tail, updated headers and right spine — plus a reference to `[s1]` | one write at `[s2]`, as a **derived** object sharing `[s1]`'s bytes and nodes; `frame root == output_commitment` | the same comparison, then pop |

**How continuation is proven — one chain, no new witness.** The base's commitment is bound at
`RecurStart`. At the first iteration, `verify_witness_root(pre_state, root_before)` shows the
pre-state (frontiers and set-field values) roots to exactly that commitment, so the prefix cannot
be altered. Only `Push` ops follow. The chain's final root equals the commitment written at
`[s2]`. Together: `[s2]` is `[s1]` plus appends. The frame's opening root is the whole join.

**What it requires.**

1. **The frontier comes from the base's index, not from a persisted copy.** The raster index
   already stores every Merkle level of every list (`RasterNodeKind::List { merkle_levels }`,
   built by `merkle_levels_from_hashes`). The frontier — length, last leaf, the complete left
   subtrees — is readable from those levels in `O(log N)`. This replaces the earlier requirement
   to persist a frontier with the object, which a base produced by a plain tile (`begin_greeting`)
   would never have had. The condition is that the base is raster-encoded, as a recur source
   already must be.
2. **The seal lands before or with derivation.** Today's buffer keeps decoded `values` and finalize
   re-encodes everything. A deriving site on today's buffer would decode all N base elements at
   `RecurStart` and re-hash them at the close — correct, but `O(N)` per extension, which is what
   derivation exists to avoid. With the encoded per-field buffers of §Mechanism, the base's bytes
   and nodes are adopted as they are.
3. **Push-only is enforced explicitly.** The set-once rule does not cover it: a base whose set-once
   field was never set stores `Unit`, and `root(Unit)` equals the absent-field root
   (`absent_field_root`), so a prover can present that field as absent in the witness and a
   deriver's `Set` then passes `apply_draft_ops`. The frame's draft entry carries `derived`, taken
   from the CFS like the site's other facts, and `advance_tile_iteration` rejects any `Set` op when
   it is set.
4. **The first iteration's witness carries the base's set-once values.** `incremental-draft-witness`
   §5 was not done, so the pre-state holds a set-once field's value, not its root. The buffer
   decodes the base's set-once fields — never its lists — at `RecurStart`. The cost is bounded by
   the size of those scalars.
5. **Relative offsets (`rindex04`).** Adopted, per §Two remedies: one logical address space,
   export by concatenation, and a fixup-free seal. §How a derived object maps onto buffers
   below relies on the first.

**The close carries a delta, not the whole object.** Today `RecurTileEnd` carries the site's full
output — `FnOutput { postcard bytes, raster payload }` — and the recorder stores a full copy at the
site's coordinate. That is the only way the object reaches the recorder, which rebuilds its storage
from trace events alone. For a derived object it is redundant: the recorder already holds `[s1]`,
written by the step that produced it. So a deriving site's `RecurTileEnd` carries:

- a reference to the base, `(coordinates [s1], commitment)`;
- per list field, the appended elements' bytes and index nodes, and the updated right spine;
- the updated list headers (`len`) and the new root — `O(#fields)`.

The recorder resolves `[s1]` in its own replica and stores `[s2]` as a new backing kind,
`ObjectBacking::Derived { base: StorageRef, tails }`, beside today's `Owned` and `Referenced`. The
child's `ObjectStore` uses the same backing and shares the base's bytes instead of copying them.
The commitment written is the stated root; the close check `frame root == output_commitment` ties
it to the replayed ops, and the recorder can recompute it in `O(k + log N)` from the base's levels
and the tail's element roots. The guest is unaffected: a write witness proves `(coordinates,
commitment)`, never bytes. Selection proofs into `[s2]` come from the derived backing's index —
the base's levels plus the updated spine — and must equal those of the same object encoded
contiguously.

Cost per derivation of k elements onto N: **hashing `O(k + log N)`, trace bytes `O(k)`, recorder
memory `O(k)`**, against `O(N + k)` for all three with a full-object output. A chain of m
derivations carries `O(Σk)` bytes instead of about `m · N`.

A plain site's output, and any non-derived object, still travels whole: those bytes are new data,
and the recorder has no other way to learn them.

#### How a derived object maps onto buffers

A design sketch, fitted to today's format: a node's payload is `tag(1) ‖ count(8) ‖ children`; a
struct field is `name_len(8) ‖ name ‖ payload_len(8) ‖ payload`; a list element is
`len(8) ‖ payload` (`prepare_raster_children`); all lengths are fixed-width. A read selects a node
and slices `(offset, len)` out of the payload. A list node stores its element ids **and every
Merkle level** inline (`RasterNodeKind::List { elements, merkle_levels }`), so a list node is itself
`O(N)`.

**Bytes — a piece table.** The derived object's logical payload is an ordered list of pieces,
`(logical_start, source, source_offset, len)`, each sourced from the base's bytes or the delta's
tail buffer. For `CollectiveGreeting { lines, title }` (laid out in name order), with k lines
appended to `[s1]`:

```
P1 Base[s1] | S tag,count | "lines" name_len,name |     unchanged
P2 Tail     | payload_len′ | L tag,count′ |               16 new header bytes
P3 Base[s1] | e1 … en1 |                                  base elements
P4 Tail     | en1+1 … en1+k |                             appended elements
P5 Base[s1] | "title" field |                             unchanged
```

There are `O(#fields)` pieces: per grown list a header piece, a base region and a tail region;
adjacent unchanged base bytes merge. A read binary-searches the pieces. A range inside one piece is
a borrowed slice with no copy — and selecting element i or a set-once field always is, since an
element never straddles the base/tail boundary. Only a whole grown list, the whole object, or a
range across the boundary spans pieces; hashing such a span streams the pieces into sha256 in
order, without copying, and decoding gathers them.

**Nodes — an overlay index.** With relative offsets, each node of the derived object is one of:

| kind | nodes | cost |
| --- | --- | --- |
| shared | every base node in an unchanged region: base elements and their subtrees, unchanged fields' subtrees | none — reused by id; their relative offsets still hold because the list header stays 9 bytes |
| new | the appended elements and their subtrees | `O(k)` |
| rewritten | the path from the root to each grown list (struct root, grown list nodes), and sibling fields after a grown list, whose offset relative to the struct moved; their children are relative to them and do not change | `O(#fields + depth)` |

The base arena keeps its ids, and the overlay appends new ones — the push-only arena property
§Why the encoding permits it already relies on.

**Lists — a continuation, not a copy.** A grown list's node becomes `{ base list node, extra
element ids, per-level tails }`. Element i's id comes from the base below `n₁` and from the extras
after. At level h the first `p_h = ⌊n₁ / 2^h⌋` nodes covered complete subtrees in the base and are
unchanged, so level h is `base.levels[h][..p_h] ++ tail[h]`; all the tails together are
`O(k + log N)` hashes — the same work as the draft buffer's frontier push. A proof sibling at
level h comes from the base prefix below `p_h` and from the tail otherwise, `O(1)` per level, so
selection proofs keep their shape.

**Chains — flattened at derivation.** `[s3]` derived from `[s2]` gets pieces that point directly at
the original buffers (`[s1]`'s bytes, `[s2]`'s tail, `[s3]`'s tail), never at `[s2]`'s piece table,
so a read costs the same at any chain depth. A grown list gains one piece per derivation, so after
m derivations a lookup is `O(log m)`; compacting a list past a piece threshold is optional.

**The recorder rebuilds, it does not trust.** It reconstructs the piece table from the base's index
and the overlay rather than accepting one from the child, and can recompute the root from the
overlay in `O(#fields + k + log N)`. A derived object keeps no postcard form: whole-object reads
already use the raster bytes when present (`OwnedObject::resolve_whole`).

**The invariant.** Flattening the pieces yields a payload **byte-identical** to encoding the same
value contiguously, and every node reached by a selector path has the same logical position,
length and root as in that encoding (node ids may differ). Selection proofs therefore cannot tell
a derived object from a contiguous one.

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
including `L` from the `0x0A` metadata. It produces nothing, so the creation record cannot be a
write at `[s]`. It does not need to be: `Start` already opens the site's `RecurProgressFrame`
(`push_site`), and that frame is committed on every step. The same holds after §A recur site gets
its own step kinds: `RecurStart` **never writes**.

> **Corrected 2026-09-30 — no write, but a verified read.** This paragraph used to say `L` was
> authenticated and that `RecurStart` carries no `StorageRoots`. The first was false in shipped
> code: a boundary step has no storage roots, so `checks::store` neither reads its object nor folds
> its witnesses, and `L` was read from unverified bytes. Rule 8
> ([`tile-io-structural-roots`](./tile-io-structural-roots.md) §Step 1) now catches a wrong `L` at
> the first iteration, but a `Start` naming a fabricated **empty** list at the right coordinates,
> followed by zero iterations, passed rule 7. The fix gives the site `Start` **read-only**
> `StorageRoots` (`root_before == root_after`), like `ProgramEnd`: the record pins the roots, so
> the existing store path verifies the read and the `0x0A` fold. It lands first on
> `SequenceStart` as `storage: Option<StorageRoots>`, set only at a recur site `Start`, and moves
> into `RecurStartStep.storage` when §A recur site gets its own step kinds lands. This proposal
> needs that read more than today's code does: the two further storage-backed facts `RecurStart`
> gains here — a deriving site's base commitment (§Continuation on the draft buffer) and a stored
> seed's commitment (§Carried-state commitment (D5b)) — are unauthenticated without it, since a CFS binding
> pins coordinates only. **Landed 2026-09-30** on `SequenceStart` (guest-tested, and verified on
> real `hello-tiles` fraud windows), and moved into `RecurStartStep.storage` the same day with
> batch A.

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
| site `Start` | `push_site` opens `draft` | creating site: `root` = `output.empty_root` from the CFS. Deriving site: `root` = the base object's commitment, bound the way the site's `Start` binds all its inputs, by its input source witness against the producing record's `output_commitment` |
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
- *The output's schema hash and empty root* (§Still open, D1 — decided 2026-09-28). The empty root
  depends only on `S`, and a zero-iteration close has nothing else to compare against.

All three live in one field, computed when the CFS is built:

```rust
pub struct RecurTileItem {
    // … id, sources, chunk, state_is_output …   (`leaves_output_open` deleted)
    /// `None` exactly for a state-only site.
    pub output: Option<RecurOutputDecl>,
}

pub struct RecurOutputDecl {
    pub schema_hash: [u8; 32],     // schema_walk over the output type, hashed
    pub empty_root: [u8; 32],      // draft_root_from_field_roots(schema, {})
    pub derives_from: Option<InputBinding>,  // `None` for a creating site
}
```

`CfsBuilder` fills `schema_hash` and `empty_root` with `raster-compiler::schema_walk`, the walker
that already fills `InterfaceDecl.schema_hash` for `main`'s interface, so both enter
`program_commitment` through the CFS. The guest therefore needs no `SchemaNode` at `RecurStart`.
Two consequences:

- **The replay tile must bind the schema.** Today `restore_draft_from_replay_handle` sets
  `S::schema_hash()` and then overwrites it with the host-supplied `handle.schema_hash`
  (`raster/src/input.rs:636`), so a journal's `schema_hash` is host-chosen. It becomes an
  assertion, `handle.schema_hash == S::schema_hash()`, which makes every iteration's schema hash
  replay-proven. The guest then checks the journal's hash against `output.schema_hash`. This is a
  gap today, independent of this proposal, and worth fixing first.
- **The walker must agree with the derive** for every recur output type — the same reliance
  `InterfaceDecl` already has. A disagreement cannot pass silently: an honest run fails, at the
  first iteration (journal hash against the CFS hash) or at a zero-iteration close (stored object
  against `empty_root`).

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
   all."* The macro records why it did not happen: *"`End` keeps its input too for now: every
   downstream check on the site's `Exec` record reads it, so duplicating keeps this addition
   strictly additive"* (`raster-macros/src/recur.rs`, above `RecurTileStart`). A deliberate
   temporary duplication, then, which the new step kinds retire.
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
    pub storage: StorageRoots,      // read-only: before == after; authenticates L, base, seed
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
| `RecurStart` at a `RecurTile` item | yes: the CFS sources, with the `0x0A` payload for `input` | **read only**: its storage inputs are read and folded against unchanged roots — this is what authenticates `L`, a base and a stored seed | `push_site(Tile, chunk, L, state_is_output)` | open the entry: empty root of `S`, or the base object's commitment |
| `Exec(Tile)` at `[s][i]` | through the replay journal, as today | none | `advance_tile_iteration` | `root_before` must equal the entry; the entry becomes `root_after` |
| `RecurEnd` at a `RecurTile` item | **no** | **exactly one** write, at `[s]` | `close_site` | `entry.root == output_commitment`, then pop |
| `RecurStart` / `RecurEnd` at a `RecurSequence` item | as for a tile site | as for a tile site | `push_site(Sequence, …)` / `close_site` | none; recur sequences are out of scope |

One rule becomes easy to state with a kind of its own: `RecurEnd` **always** writes exactly one
object. Under §One storage rule every site produces one, `finalize = false` is gone, and a
state-only site stores its state. So `storage` is never "unchanged", and the guest requires a
write rather than accepting its absence. Where a creating site's empty root comes from is not
changed by this: it is needed at `RecurStart`, and still needs `S`'s schema there (§Still open, D1).

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

- **Ordinary nested sequences close at `[-s]` too** (§Still open, D4 — decided 2026-09-28). With
  a sequence's `SequenceStart` and `SequenceEnd` sharing `[s]`, the successor relation — keyed on
  the coordinate alone — is the union of both halves' successors. Measured with a throwaway probe
  on `main = [a, child, d]`, `child = [b, c]`:

  | after | successors today |
  | --- | --- |
  | `[2]` (child's `SequenceStart`) | `[2]` (End), `[2,1]` (first step), **`[3]` (next sibling)** |
  | `[2,2]` (child's last step) | `[2]` (End), **`[2,1]` (restart the body)**, **`[3]` (next sibling)** |

  So the ordering check accepts a trace that skips a sequence's End, and one that restarts its
  body after the last step — the latter stopped only incidentally, by the duplicate-write assert on
  the body's first tile. Nothing else requires the End: a consumer of the child's output is checked
  only for storage coordinates inside `[2]`. With the End at `[-s]`, every scope — sequence, recur
  sequence iteration, recur site — opens at `i` and closes at `-i`, and each set is exact:
  `SequenceStart` → `{[s][1], [-s]}`, the last child → `{[-s]}`, `SequenceEnd` → `{[s+1]}` or the
  parent's close. The union branch in `try_get_next_coordinates` goes away rather than surviving
  for nested sequences alone.

**What moves.**

| area | change |
| --- | --- |
| `raster-core/src/trace.rs` | `RecurStart` and `RecurEnd` added to `StepKind`, with `RecurStartStep` and `RecurEndStep`; `ExecTarget::RecurTile` and `ExecTarget::RecurSequence` removed, and the comments that explain them (`trace.rs:457`, `:784`) rewritten; the accessors `input_commitment()`, `input_source_commitment()`, `output_commitment()` and `storage_roots()` gain arms — inputs only on `RecurStart`, output and storage only on `RecurEnd` |
| `raster-core/src/cfs.rs` | `closing_coordinates_of` accepts recur site and sequence items; `try_get_next_coordinates` (including the climb out of a sequence's last child, which returns the parent's close) and `expand_recur_entry_coordinates` change the successor sets above |
| `raster-runtime/src/tracing/recorder.rs` (`SequenceEnd`) | a nested sequence's `SequenceEnd` records at `[-s]`, with its own witness-store entry |
| `raster-prover/src/trace.rs` (producer lookup) | a `Sequence` item's producer is its `SequenceEnd` at `[-s]` |
| `raster-runtime/src/tracing/recorder.rs` | the `RecurTileStart`/`RecurSequenceStart` event arm records `RecurStart` with `site_id`, and `sequence_id` naming the enclosing sequence; the `RecurTileEnd`/`RecurSequenceEnd` event arms record `RecurEnd` at `[-s]`, write at `[s]`, and stop reading `fn_call_record.input` |
| `raster-macros/src/recur.rs` | the `RecurTileEnd` and `RecurSequenceEnd` events publish `input: None` |
| `checks/cfs.rs` | `record_matches_item` gains one arm, `(RecurStart \| RecurEnd, RecurTile(item) \| RecurSequence(item)) => site_id == item.id`, with any other item rejected, and loses `(SequenceStart, RecurTile)` and the two `Exec` site arms; `declared_sequence_id` loses its `RecurTile` arm; `advance_recur_progress` dispatches on the kind, takes the family from the item and passes `opened(coordinates)` to `close_site`; step-kind names in panic messages gain the two |
| `checks/store.rs` | `RecurEnd` requires exactly one write, at `opened(coordinates)` |
| `raster-cli/src/commands/run.rs`, `raster-prover/src/trace.rs` | witness builders learn the two kinds; `RecurEnd` gets no input source witness; the producer lookup and `record_produces_item` change as above |
| `raster-cli/src/commands/run.rs` (`step_coordinate_label`), `raster-cli/src/commands/fraud.rs` (`exec_target_name`, `exec_target_kind`) | lose the `RecurTile`/`RecurSequence` targets; a site's label comes from `RecurStart`/`RecurEnd`, and the family from the CFS where the printout wants it |

**Cost.** A trace-format break: `StepKind` gains two variants, `ExecTarget` loses two, and site
closes move coordinate, so every trace containing a recur site changes its fingerprint. Batch it
with the break §One storage rule already causes. Nested sequences' `SequenceEnd` moves to `[-s]`
in the same break (D4), so every trace with a nested sequence changes too.

### What it needs

- **A creation record at the site's `Start`, and two assertions at its close.** Both steps already
  exist; neither carries a draft fact today. The close already holds the commitment
  (`output_commitment`), so what is missing is the comparison and the removal, not a step. Both
  go into the site's `RecurProgressFrame`: `push_site` opens the draft entry, and `close_site`
  compares and pops it. `advance_tile_iteration` chains it in between.
- **`RecurStart`/`RecurEnd` step kinds** for both recur site families, in place of the borrowed
  `SequenceStart` and `Exec`, with the close at `[-s]`. See §A recur site gets its own step kinds.
- **No output for draft-returning iterations**, on the host and in the replay, plus the guest rule
  that an `Exec` step without a write has an empty `output_commitment`. See §What actually happens
  during a sweep.
- **Draft identity from the site**: `anchor_for_schema([s], S::schema_hash())` as the host-side
  buffer key, and `draft_id` removed from the handle, the journal and the witness. See §Draft
  identity.
- **`THREAD_DRAFT_STORAGE` renamed to a per-site `DraftBuffer`**, created at the site's `Start` and
  consumed once by the seal. See §The draft buffer.
- **`RecurTileItem.output: Option<RecurOutputDecl>`** in the CFS: whether the site owns an
  object, its schema hash and empty root (computed by `schema_walk`), and whether it derives and
  from which input.
- **The replay tile's schema assertion**, `handle.schema_hash == S::schema_hash()`.
- **Tile output bound to the stored object**, resolved together with this proposal:
  [`tile-output-commitment-unbound`](../issues/tile-output-commitment-unbound.md). A deriving
  site's base and a state-only site's stored result are only as sound as that join.
- ~~**Persist the append frontier** with the object.~~ **Superseded 2026-09-27**: the raster
  index already stores every list's Merkle levels, so a deriving site reads the frontier from the
  base's index in `O(log N)`. See §Continuation on the draft buffer.
- ~~**Lay append-only fields last in the payload.**~~ **Superseded 2026-09-27**: sufficient for one
  growing list only; relative offsets cover any number.
- **Derivation after (or with) the seal**, the `derived` flag with its push-only guest check, and
  decoding only the base's set-once fields at `RecurStart`. See §Continuation on the draft buffer.
- **A delta output for a deriving site's close**, and `ObjectBacking::Derived` in both stores.
- **One sealed payload for both consumers** at the site's close: the child's store and the
  `RecurTileEnd` event's `FnOutput`. The driver's separate `resolve_storage_value` +
  `raster_trace_payload` pass goes away.
- **`rindex04` relative offsets — adopted.** An earlier revision made them an optimisation, on
  the premise that nothing earlier in the payload grows; that holds for one growing list only.
  Sharing could be built on absolute offsets with per-buffer addressing, but relative offsets give
  one logical address space, export by concatenation and a fixup-free seal. See §Two remedies.
- **The derived-object mapping**: piece table, overlay index, list continuation, flattening at
  derivation. See §How a derived object maps onto buffers.

### Still open

Closed:

- ~~Who owns a draft no recur creates.~~ **Closed 2026-09-27** by §The restriction: a draft never
  crosses a step boundary, so no draft exists outside a recur site. `hello-tiles/src/main.rs:85` is
  rewritten — a plain tile returns an ordinary value and a site derives from it — at the cost that a
  chain of plain tiles contributing to one *growing* object is no longer expressible.
- ~~Whether intermediate objects should stay addressable.~~ **Answered 2026-09-27: yes.** A derived
  object's backing refers to its base, so the base must stay (§Continuation on the draft buffer).

**Decisions**, in the order they should be taken — each later one leans on the earlier:

- ~~**D1. The site's output schema: where it comes from, and how it is bound.**~~ **Decided
  2026-09-28: the CFS declares it** — `RecurTileItem.output` carries `schema_hash` and `empty_root`,
  computed by `schema_walk`, and the replay tile asserts its schema instead of adopting the host's
  (§What must come from the CFS). Rejected: taking the schema from iteration 0, which leaves a
  zero-iteration close nothing to compare against; and carrying a `SchemaNode` at `RecurStart`,
  whose hash would still need a bound value to check against. The question as it stood: A creating site's
  frame opens at the empty root of `S`, and a zero-iteration close compares against that root, so
  the guest needs `S`'s schema hash and empty root at `RecurStart`. Two facts constrain the
  answer. The CFS is built from the source AST (`raster-compiler`'s `CfsBuilder`), but
  `raster-compiler::schema_walk` already computes a `SchemaNode` from source for `main`'s interface
  (`InterfaceDecl.schema_hash`, pinned in `program_commitment`). And **the replay tile does not bind
  the schema today**: `restore_draft_from_replay_handle` overwrites the statically known
  `S::schema_hash()` with the host-supplied `handle.schema_hash`, so the journal's `schema_hash`,
  and the witness schema checked against it, are host-chosen at the first link of a chain.
- ~~**D2. The journal's `draft_id`: check it or remove it.**~~ **Decided 2026-09-28: removed**
  from `DraftReplayHandle` and `DraftReplayTransition`; the anchor stays a host-side buffer key
  (§Draft identity).
- ~~**D3. State-only iterations.**~~ **Decided 2026-09-28: included.** A `RecurState<T>` return
  publishes no output and replays empty `output_bytes`, as a draft return does; nothing reads the
  `[s][i]` object (§What actually happens during a sweep).
- ~~**D3′. A state-only site's stored result is not tied to its final carried state.**~~
  **Resolved 2026-09-28 by D5b** (§Carried-state commitment). As found:
  2026-09-28 while deciding D3; it exists today, independently of D3. A state-only site stores its
  final state `T` at `[s]` (`run_recur_list_state` → `bind_infallible_call` →
  `store_execution_output_value`, at `current_recur_site_coordinates()`), and the recorder writes
  it with `output_commitment = raster_root(T)`. The chain ends at `frame.state_commitment =
  H("recur-carried-state" ‖ postcard(T))`, replay-proven for tiles. No step compares the two —
  `close_site` receives no output, and the I/O check skips execution steps — and they cannot be
  compared directly, being different hash functions over the value. So the chain can prove one
  final state while `[s]` holds another, and every later reader consumes the stored one without
  complaint: summing `[1, 2, 3]` proves `{6}` while `{7}` is written. `recur-state-chaining` hit the
  same encoding mismatch and moved its terminal check onto the iteration's postcard output, for
  recur sequences only; for tiles the last hop was left open.

  The fix is a terminal check **of the state's own**, kept separate from the draft's (§Draft and
  carried state are different things): at `RecurEnd` of a state-only site, the stored result must
  be the chain's final state. What remains to choose is the commitment that makes the two
  comparable — for instance `raster_root(T)` over the inner `T` (not `RecurState<T>`, whose raster
  encoding adds the field name `inner`), computed in the replay; or the selection hash of the
  state's raster payload, which the guest can compute from payload bytes without knowing `T`.
  A state+output site needs no terminal state check: it discards its state. Open points: the draft root has
  been shown equal to the raster root only for a struct of a `String` and a `List<String>` (not yet
  enums, maps or integers); and recur sequences bind `state_in` from inline postcard bytes in the
  guest, which cannot compute a raster root without the type, so their half belongs with D5.

  **Deferred to D5b (2026-09-28).** The carried-state commitment function is shared by recur tiles
  and recur sequences, so it is decided once, with recur sequences, rather than changed for tiles
  now and again later.
- ~~**D4. Ordinary nested `SequenceEnd` at `[-s]`.**~~ **Decided 2026-09-28: included**, with the
  step-kind change. Measured: sharing `[s]` lets the ordering check accept a skipped End and a
  restarted body (§What the move to `[-s]` forces).
- **D5. Recur sequences**, split in three:
  - ~~**D5a. Drafts in recur sequences.**~~ **Decided 2026-09-28** — §Recur sequences: the draft
    never crosses a *site* boundary; body tiles advance the innermost frame's draft entry; draft
    returns publish no output; seeding tiles become values; a nested recur tile appending to the
    outer draft is a non-goal.
  - ~~**D5c. Binding a sequence's returned value to its body.**~~ **Decided 2026-09-28** — §Sequence
    return binding: the CFS records each sequence's return, source and selector path;
    `SequenceEnd` and `ProgramEnd` are checked against it; consumers cite exactly the returned
    binding; `ProgramEnd` first. **Implemented for storage-backed returns** (2026-09-28), resolved
    statically through the CFS rather than checked at each `SequenceEnd`; paths through a
    parameter and inline returns remain (§Sequence return binding). As found:
    2026-09-28, and broader than recur sequences. `SequenceDef` has no return binding — only
    `main`'s `produces_output` flag — and the flow resolver resolves call *arguments*
    (`resolve_argument`) but never a body's returned expression. Three consequences:
    - a nested `SequenceEnd` is checked only against its own output witness bytes, so an
      *inline* returned value — a recur sequence's carried state — is bound to nothing the body
      computed: the chain of `state_out`s is consistent with itself, not with the body;
    - a consumer of a nested sequence's output is checked only for storage coordinates **inside**
      `[s]` (`checks/cfs.rs`, the `Sequence | RecurSequence` arm of the prior-item-output check),
      so it may cite any object the sequence wrote, not the one it returned;
    - `main`'s output is checked for presence and selection, not identity. **Measured**: a
      throwaway guest probe gave `verify_program_end` an output object at `[7]` — a coordinate
      naming no CFS item — and it returned `Established`. `program-end.md` §7 argues a forged
      output "diverges from the fingerprint, which is fraud-provable"; that needs the guest to
      reject the forged `ProgramEnd`, which it does not.
  - ~~**D5b. The carried-state commitment.**~~ **Decided 2026-09-28: the value's object
    commitment**, the raster root of `T` (§Carried-state commitment). Recur sequence state passes by
    reference; a state-only site's `RecurEnd` checks `frame.state_commitment == output_commitment`;
    a stored seed opens the frame at `RecurStart`. Resolves D3′.

**Left to implementation:**

- The delta output's wire format: a new `FnOutput` form, or a separate field.
- The exact `rindex04` format — in particular whether a field's relative offset lives in the child
  node or in the parent's field entry. The second leaves fields after a grown list unrewritten.
- The piece-compaction threshold for long derivation chains (optional).
- Whether a derivation's base may be a *selection* inside another object
  (`output = select!(x.inner)`) rather than a whole object at a coordinate. It must at least be
  raster-encoded.

**Not yet measured or tested:**

- The third full encoding at a recur close — found by reading the code, not timed.
- How much of the ~1.46 µs/element the seal removes: only the re-hash is purely duplicated; payload
  bytes and index nodes are still built, earlier (§The measurement basis was not representative).
- Figures derived from measured rates rather than runs: ~200 s for re-encoding per push at
  N = 16 384, and ~7 s per 100 K iterations for today's marker writes.
- The frontier read from `merkle_levels` equals the frontier built by pushing — the duplicate-last
  padding makes this worth a test before relying on it.
- Byte-identity of the seal and of the derived-object mapping.
- `schema_walk` agrees with the derive's `S::schema()` for every recur output type in `examples/`,
  `crates/raster/tests/` and `raster-inference`.
- The replay's raster root equals storage's for every state type: enums, maps, integers,
  `Bytes<N>`, nesting (§Carried-state commitment).
- A zero-iteration creating site's stored object equals the empty root of `S` (only the
  non-empty case is measured).

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

## Implementation order

Decided 2026-09-30: this proposal is implemented in full, together with
[`tile-io-structural-roots`](./tile-io-structural-roots.md) step 2. Batches are grouped by what
they break; the same table is in that proposal's §Order.

| batch | contents | breaks |
| --- | --- | --- |
| **A — trace shape** | `RecurStart`/`RecurEnd` step kinds, site closes at `[-s]`, `ExecTarget::RecurTile`/`RecurSequence` removed; D4 (nested `SequenceEnd` at `[-s]`); `RecurStartStep.storage` (read-only) takes over the interim `SequenceStart.storage` (landed 2026-09-30), which is removed again; site-ordering checks | trace shape, transition guest; **done 2026-09-30** — moves the image ids of tiles that link the edited code (see below) |
| **B — language and object ownership** (**done 2026-10-01**) | §One storage rule and §The restriction: `new!`/`finalize`/`finalize = false` removed, a site owns its object at `[s]` (creates, or derives with `output = base`), plain tiles return values, `DRAFT_NAMESPACE` deleted, host anchor `anchor_for_schema([s], S)`; D1b (`RecurOutputDecl` in the CFS via `schema_walk`); programs rewritten (`examples/`, `crates/raster/tests`, `raster-inference`) | CFS, programs, `program_commitment` |
| **C — tile journal** (one tile-image-id break) (**done 2026-10-01**) | D3 (recur iterations publish no output; an `Exec` with no write has an empty `output_commitment`); D2 (`draft_id` removed); D1a (replay tile asserts its schema); D5b (replayed state as raster roots, recur-sequence state by reference); **`tile-io-structural-roots` step 2** (`output_root`, `input_roots`); guest: the frame's draft entry replaces `active_drafts`, `RecurEnd` writes exactly one object | every tile image id, `program_commitment` |
| **D — materialization** | `DraftBuffer`, one seal for store and `RecurEnd` output, `rindex04` relative offsets, derivation sharing (`Derived` backing, delta output) | `.rindex` format; no guest or tile change |
| **E — verification** | both proposals' Verification lists; `tile-io-structural-roots` step 3; GPU proving; all locks, `raster-inference` included | — |

What implementing this proposal in full changes for `tile-io-structural-roots`:

- Step 2's write rule needs no exceptions: a plain tile returns a value (`output_root = Some`), a
  recur iteration never writes, and `RecurEnd`'s one write is bound by the frame (draft root or
  D5b state commitment), not by a journal.
- D2 is not deferred: with no draft crossing a step boundary, the frame's draft entry replaces
  `active_drafts`, the only reader of `draft_id`.
- D3 covers recur iterations only; plain `Draft`-returning tiles no longer exist.
- The draft-argument binding mismatch (CFS says storage, trace says an inline handle — the `[7]`
  and `[15]` fraud windows) disappears with batch B.

Open: whether batch B verifies under today's guest on its own. If not, B and C merge. Checked
first, on one rewritten recur site in `hello-tiles`.

**Batch A — done 2026-09-30.**

- **Step kinds.** `StepKind::RecurStart(RecurStartStep { site_id, input_commitment,
  input_source_commitment, storage })` at `[s]` and `StepKind::RecurEnd(RecurEndStep { site_id,
  output_commitment, storage })` at `[-s]`, for both families; `ExecTarget` is left with `Tile`
  only. The interim `SequenceStart.storage` is gone: the `L` read is `RecurStartStep.storage`. A
  site's steps name the **enclosing** sequence in `sequence_id`; the site is `site_id`. `RecurEnd`
  carries no inputs, the macros stop re-publishing them on `RecurTileEnd`/`RecurSequenceEnd`, and it
  writes the site's object at `opened([-s]) = [s]` (`checks::store`).
- **Closes at `[-s]`.** Every scope — nested sequence (D4), recur site, recur-sequence iteration —
  closes at its own coordinate, with its own witness-store entry and no input.
  `CfsCursor::try_get_next_coordinates` is rewritten as explicit per-kind rules instead of the
  union walk. It is **stricter than the table above** in one place: a scope with a body cannot
  close before its first child — `SequenceStart` offers `[s][1]` only, not `{[s][1], [-s]}`. It also
  closes a hole the table did not list: `ProgramStart` used to offer `[]`, i.e. `ProgramEnd`
  straight away, skipping the program.
- **Guest.** `record_matches_item` binds `RecurStart`/`RecurEnd` to a recur site item by
  `site_id`, `RecurStart` only at an open coordinate and `RecurEnd` only at a close; the borrowed
  `SequenceStart`-at-a-site and `Exec(RecurTile/RecurSequence)` arms are gone. `advance_recur_progress`
  opens the frame on `RecurStart` and closes it on `RecurEnd` (`close_site(opened)`). Steps without
  an input source commitment (every close) skip the scope-parent check on both host and guest.
- **Deferred to batch C, deliberately.** `RecurEnd` writes *at most* one object: `finalize = false`
  sites still write none until batch B. `RecurEnd` counts as an execution step (its output is not
  compared yet — the frame's draft entry does that in batch C).
- **Fraud tooling.** `RecurEnd` is an eligible `--fraud-step` target (`recur-site`), so a site's
  close can be corrupted and proven.
- **Tile image ids do move.** The table said "no tile ids". Wrong: a tile binary embeds code — and
  the file and line of every panic site — from the raster crates it links, so editing
  `raster-core/src/trace.rs` moved the five `hello-tiles` tiles that link it (the draft and recur
  tiles). Every batch that touches code compiled into tiles moves those tiles; the "one break"
  framing is about released program identities, not about development.
- **Verified.** `raster-core` 169 (successor tests rewritten; D4 and `main` ordering tests added),
  runtime 67, `raster` 26 + suites, guest 129 (site-kind and ordering tests added). Real
  `hello-tiles` fraud windows in dev mode: site starts `[11] [16] [18]`, site closes `[-11]` (the
  corrupted step itself), `[-16]`, `[-18]`, a window crossing nested-sequence closes (`29:4`) and
  one ending at `ProgramEnd` (`97:4`) all prove. `35:4` fails on the pre-existing
  [`sequence-scope-forbids-narrowing`](../issues/sequence-scope-forbids-narrowing.md)
  (`personal_greet_seq` selects into its parameter).

**Batch B — done 2026-10-01** (language and object ownership; this repository — `raster-inference`
is a separate follow-up).

- **Language.** `new!`, `finalize`, `finalize = false`, `new_draft` and the six `run_recur_*_open`
  drivers are gone. `call_recur!`/`call_recur_seq!` take bare `output` (the site **creates** its
  object) or `output = base` (it **derives** from a stored object). A site's entry point receives
  `SiteOutput<S>`; a derived base is traced as a **storage** binding of `RecurStart`, so batch A's
  read authenticates it.
- **Tile-local drafts** (the amendment to §The restriction). `Draft::<S>::new()` (refused outside a
  tile); a tile returning a draft it created — it takes no draft — is completed by the `#[tile]`
  wrapper into its `S` output, native and replay alike, via `draft_tree_from_fields` and the value
  decoder now shared in `raster_core::tree`. A tile that *receives* a draft (a recur-sequence body
  tile) returns it to its site as before.
- **Derivation.** `derive_site_draft` rebuilds the draft from the base's value — set-once fields
  written, list fields with their elements and frontier — and asserts its root equals the base's
  commitment. Push-only falls out: a second write to a set-once field is refused. `[k]` is never
  modified; `[s]` is a new object. (Rebuilding re-hashes the base's elements; batch D reads the
  frontier from the stored index instead.)
- **Draft identity.** `anchor_for_schema([s], S)`; drafts complete only at their site's
  coordinate. The synthetic namespace survives **only** for the fixture helper `store_value`, which
  no program path reaches — a deviation from "`DRAFT_NAMESPACE` is deleted", kept to avoid
  rewriting ~30 test fixtures.
- **D1b.** `RecurOutputDecl { schema_hash, empty_root, derives }` on both recur item kinds, filled
  by `CfsBuilder::fill_site_output_schemas` from the site's `RecurOutput<S>` /
  `RecurSequenceOutput<S>` parameter via `schema_walk`. Checked on `hello-tiles`: create sites
  `derives = false`, the recur sequence `derives = true`, the state-only site none. Not yet
  checked: `schema_walk`'s hash equals the derive's `S::schema_hash()` — needed by batch C.
- **Draft budget.** `DRAFT_STEP_BUDGET` = 64 KiB per consumed element, `DRAFT_OP_CHARGE` = 64 bytes
  per op, reset per tile run by the wrapper. Measured: `hello-tiles` peaks at 327 bytes per run;
  `raster-inference`'s widest per-element write is a 1536-wide `ActivationRow`, ~6 KiB.
- **Programs.** `hello-tiles`: the three-tile draft chain is one creating tile; the recur sequence
  derives from a tile-built base. `chain-example/phase3-report`: one creating tile builds the report
  (`authenticated-chain-draft-output`'s reproducer). Create sites use bare `output`.
- **Verified.** Suites green (core 169, compiler 44, runtime 67, `raster` incl. new tests for
  creating tiles, the budget, derivation and `Draft::new` outside a tile, guest 129). `hello-tiles`
  commit + honest audit. Dev-mode fraud windows over the creating tiles `[6]`, `[12]`, a consumer
  of `[6]`, create sites `[9]`, `[10]` and their closes, and the derive site `[13]` — **its windows
  were blocked before batch B** — all prove; a negative control fails at `[13]`'s `RecurStart`
  reading its base. Guest unchanged in this batch: a derived site's first `root_before` is still
  adopted (`active_drafts`); batch C anchors it.

**Batch C — done 2026-10-01** (tile journal; every tile image id and `program_commitment` move).

- **D2.** `draft_id` is gone from `DraftReplayHandle`, `DraftReplayTransition` and the native
  witness; the replay builds its `Draft` with a constant anchor it never reads.
- **D1a.** `restore_draft_from_replay_handle` asserts `handle.schema_hash == S::schema_hash()`
  instead of adopting the host's hash, so every journal's schema is replay-proven.
- **The frame's draft entry.** `RecurProgressFrame.draft: Option<SiteDraft { schema_hash, root
  }>`, opened at `RecurStart` from `RecurOutputDecl` (empty root, or the `"output"` base's
  commitment — a whole object only), advanced by `RecurProgressStack::advance_draft` on every
  tile step that carries a transition (the innermost site: a recur tile's iteration — where it is
  required — or a recur sequence's body tile), compared at `close_site` with the object written.
  `active_drafts`, `TrackedDraftState` and the guest's `if let Some(..)` are deleted; the guest's
  draft check now returns a `DraftStep` for the frame. The recorder runs the same three calls from
  the native witness (`DraftStep::from_native_witness`), so a seal or schema disagreement fails at
  record time. `RecurEnd` must write: `close_site` refuses an empty `output_commitment`.
- **D3.** A tile whose return carries its site's draft or state publishes no output natively and
  replays empty `output_bytes`; a recur-sequence iteration returning the draft publishes none
  either. Guest: a writing step kind that wrote nothing records an empty commitment, and only a
  writing step kind may carry a write witness.
- **D5b.** Carried state is committed by its raster root (`raster_core::tree::value_root`; for a
  recur tile over the inner `T`, `raster::recur_state_root`); the postcard `state_commitment` is
  deleted. A **stored** seed — a whole object, second argument of a site the CFS marks
  `carries_state` (new on both recur items) — is read at `RecurStart` and opens the chain
  (`RecurProgressStack::seed_state`); an inline seed is adopted from iteration 0, as before. A
  state-only site's `close_site` requires `state_commitment == output_commitment`. **Recur
  sequences pass state by reference**: `RecurSequenceState<T>` holds an `AuthRef<T>`; an
  iteration's `Start` records the state as a storage binding, checked to be the chain's current
  commitment; its `End` records the returned state's binding, which the guest checks is the object
  the CFS's `returns` for the body names inside the iteration (the compiler now records a recur
  body's returned *state* there, the first element of a `(state, output)` tuple — `ReturnExpr::
  Tuple`), reads from the current storage state (`TransitionInput.returned_state_read_witness`),
  and holds `state_out` to. The postcard iteration output and its terminal check are gone.
- **`tile-io-structural-roots` step 2** — see that proposal: `output_root`, `input_roots`.
- **Fixtures.** `hello-tiles` gains a stored seed for its state-only recur tile and a stateful recur
  sequence seeded by a stored object (`count_lines_sequence`), so the D5b paths run under
  `--commit`/`--audit`.
- **Verified.** Suites green: core, compiler, runtime, `raster` (incl. new tests for the replay's
  roots, D3 and by-reference state), transition guest, prover, CLI. `hello-tiles`
  commit + honest audit; dev-mode fraud windows prove over: a creating tile; draft iterations,
  which now write nothing; the create-site and derive-site closes (draft root check); the derived
  recur sequence's body tiles; state-only iterations and close from an inline and from a stored
  seed; an early-`Break` site; the stateful recur sequence's `RecurStart`, iteration `Start`, body
  tiles, iteration `End` and close. Negative controls (guest markers placed after each new check
  succeeds) fire on those windows: D3's empty-commitment rule, the draft-root close at `[-9]` and
  at the derive site `[-13]`, the state close at the state-only site, the iteration state binding
  and the returned-state read. The one failing window, at exec 3, is
  `sequence-scope-forbids-narrowing`, unchanged.
- **Limits, recorded.** A derive base, a stored seed, and a recur sequence's state must be whole
  objects: a binding carries the object commitment, not a selected value's root. A selected seed
  falls back to adoption for a recur tile and is refused for a recur sequence.

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
  program that builds a draft. `rindex04` relative offsets are adopted (a `.rindex` format break,
  above), and continuation adds a delta form of a deriving site's output and a `Derived` object
  backing in both stores.
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

**Measured 2026-09-28 — the close assertion holds for an honest run.** The design's central check,
`draft.root == output_commitment` at `RecurEnd`, needs a draft's root to equal the stored object's
raster commitment, and `struct_commitments_root` is order-sensitive. A throwaway probe on the
existing `collect_lines` fixture (`crates/raster/tests/recur_draft.rs`; `LineBundle { title, items }`,
`title` set, `items` pushed, declaration order differing from name order) compared the last
iteration's `root_after` from `apply_draft_ops`, the `RecurTileEnd` raster root and the returned
`StorageRef` commitment: all three were `540d47bc…2a6b`. The probe was removed afterwards. A
zero-iteration site was not probed.

- `append_frontier_root_matches_list_root_from_hashes` already pins frontier ≡ list root; the new
  invariant is the sibling: **a sealed draft's payload, index and root are byte-identical to
  `raster_payload_for_value` on the same value**, checked over the same 1..1024 growth.
- An element-hash counter asserting each element is hashed **once** across create→seal,
  counting the trace payload built for `RecurTileEnd` as well as the store — today that path hashes
  every element a third time.
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
- Iteration writes: a sweep of N iterations performs exactly one authenticated append, for a
  draft-building and for a state-only site alike; an
  iteration record carrying a non-empty `output_commitment` with no write is rejected; neither a
  tile's input nor its journal carries a draft id.
- Recorder-side close: a seal whose root differs from the frame's draft root panics at record
  time, not only in the guest.
- Schema binding: a replay handle whose `schema_hash` is not `S::schema_hash()` fails in the
  replay tile; a journal whose schema hash differs from `output.schema_hash` is rejected by the
  guest; a zero-iteration close whose stored object differs from `output.empty_root` is rejected.
- Carried state: a state-only site whose stored result differs from the chain's final state is
  rejected at `RecurEnd` (`{6}` proven, `{7}` stored), for a recur tile and a recur sequence; a
  site whose seed is a stored object opens its frame from that binding, and an iteration 0
  reporting a different `state_in` is rejected.
- Return binding: a `ProgramEnd` citing any object other than the one `main` returns is rejected
  (the measured probe, inverted — **in place since `5e0bf83`**, with the intermediate-object case);
  a `ProgramEnd` citing the right object under a different selector (`stats.sum` where `main`
  returns `select!(u64, stats.count)`, or the whole `stats`) is rejected, as is a zero-length
  selection claiming an output with no proof — **both in place**, with genuine proofs; an object a
  nested sequence wrote but did not return is rejected, for `ProgramEnd` and for an argument, and
  a value a sequence passes through is accepted outside the call — **in place**; a consumer citing A at `[2,1]` where the sequence returns B at
  `[2,2]` is rejected; a re-return through two sequences resolves to the original coordinate; a
  recur sequence iteration whose recorded state is not its body tile's output is rejected; a
  sequence whose returned expression the resolver cannot resolve fails to build.
- Sequence ordering: a trace that skips a nested sequence's `SequenceEnd` (`[s]` straight to
  `[s+1]`), or restarts its body after the last step, is rejected by the ordering check.
- Site ordering: a trace that leaves a site without its `RecurEnd` (`RecurStart` then `[s+1]`), or
  enters it without its `RecurStart` (straight to `[s][1]`), is rejected by the ordering check; a
  zero-iteration site `RecurStart` → `RecurEnd` is accepted; a later sibling's `PriorItemOutput`
  input resolves to the `RecurEnd` record at `[-s]` and to the object at `[s]`.
- Recorder/guest parity: over a fixture with a creating site, a deriving site and a state-only
  site, the recorder's stamped `recur_progress_commitment` equals the guest's at every step.
- A sharing test: deriving `[s2]` from `[s1]` + k appends hashes **k** elements, not `len([s1]) + k`
  — the sibling of the element-hash counter above, and the thing `rindex04` exists to make true of
  the payload as well as the root.
- Frontier from the index: for every length up to 1024, the frontier read from a list's stored
  `merkle_levels` equals the frontier built by pushing the same elements.
- Continuation: a deriving site's `Set` on a base's unset set-once field is rejected; a deriving
  site's `RecurTileEnd` carries `O(k)` bytes; `[s2]`'s derived backing yields selection proofs and a
  materialized payload byte-identical to the same object encoded contiguously; a delta whose
  stated root differs from the frame root panics in the recorder.
- Derived mapping: for a chain of derivations over a struct with several lists, flattening the
  pieces equals the contiguous encoding byte for byte, every selector path yields the same
  position, length and root, and proofs of element i equal the contiguous object's; exporting a
  derived object as `.rindex` by concatenation round-trips through `read_raster_artifact`.

## Not in this proposal

Draft→draft transformation (mapping every element of one draft into another) is
[`draft-map`](./draft-map.md). It needs a coverage rule this proposal does not supply.
