# Proposal: `draft-map` — transform a draft into a draft, with coverage proven

Status: Proposed 2026-09-21. **Design not chosen** — this document states the problem and the
constraint that makes it hard, not a mechanism.

Related:
- [`incremental-draft-materialization`](./incremental-draft-materialization.md) — split this out of
  that proposal deliberately. Everything there is about *cost and lifetime* and needs no new
  soundness rule; this needs one, and mixing them would let a genuinely new verification obligation
  ride in on a performance change.
- [`selection-unbound-from-execution`](../issues/selection-unbound-from-execution.md) — **top
  priority, and the same missing join.** A verified selection proof and a verified execution with
  nothing relating them. A map has exactly that shape.
- [`lazy-list-recur`](./lazy-list-recur.md) — supplies the model for the rule that is missing here:
  a sweep is held to `count == L` against an *authenticated* `L`, and rules 1–7 pin contiguity and
  position. A map needs the analogue.

## The shape

Today a draft grows by `push`: elements arrive one at a time from tile calls, and each append is
attested by the `TileExec` step that produced it (`checks/drafts.rs` chains `root_before → ops →
root_after` per step). What has no spelling is:

> take every element of draft `A`, apply a tile to it, and append the result to draft `B`.

Written with existing machinery this is a recur over `A`'s elements with `B` as the recur output.
That works, and is the reason this is a proposal rather than a gap: **the appends are already
covered**, one `TileExec` at a time, by the same per-step draft transition check.

## What is missing

The appends being individually attested does not make the *map* attested. Three facts have no
check:

1. **Every element of `A` was consumed.** Nothing relates `B`'s length to `A`'s. A sweep that
   stops early produces a shorter `B` with a perfectly valid proof over it.
2. **Each element was consumed once, in order.** Nothing pins `B[i]` to `A[i]`.
3. **`A` is the draft it claims to be.** If `A` is still open, its identity is a root the ops chain
   carries; if sealed, a storage object. The map must name which, and bind to it.

(1) is the same fact `lazy-list-recur` spends rules 1–7 and S1–S5 on, and its §6 warning applies
verbatim: the journal is *"a binding, not an authority"*. A per-element count the step reports is a
value the prover chose.

## Why it cannot simply reuse the recur rules

A recur's bound `L` comes from an **authenticated** `0x0A` metadata selection over a *stored* list
— `authenticated_source_len`, which refuses anything but `SelectionPayloadKind::List`. That is what
makes `L` index-trusted rather than prover-chosen, and it is why the forged `len = 0` sweep is
refused.

An open draft has no stored list and no `0x0A` selection. Its length lives in an `AppendFrontier`
inside the runtime. So either:

- **the source draft must be sealed first**, which makes the map an ordinary recur over a stored
  list and this proposal mostly disappears — at the cost of a full materialization between the two
  drafts, which is the thing `incremental-draft-materialization` exists to avoid; or
- **the frontier's `len` must become authenticated evidence** the guest can check, in the way the
  `0x0A` record is. That is the interesting direction and the one with no design yet.

## Open questions

- Does `B` need to record which `A` it maps, or is it enough that the tile steps chain? (If the
  chain is enough, what stops two interleaved maps from being attributed to one source?)
- Can `AppendFrontier` be made to serve as the authenticated bound, or does a map always require
  its source sealed? The second is sound and cheap to specify; the first is what makes drafts
  composable without paying materialization between stages.
- Is a *filter* (fewer outputs than inputs) in scope? It breaks the `len(B) == len(A)` equality
  that would otherwise be the cheapest coverage rule, and needs per-element evidence of the skip.

## Not in this proposal

Cost, lifetime and where a draft closes: [`incremental-draft-materialization`](./incremental-draft-materialization.md).
