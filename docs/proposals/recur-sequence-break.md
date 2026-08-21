# Proposal: `recur-sequence-break` — early termination for `call_recur_seq!`, decided where decisions can be made

Status: proposed 2026-08-13

Related:
- [`recur-progress-commitment.md`](./recur-progress-commitment.md) — **blocking.** This extends
  its `RecurProgressStack` and reuses the trace `recur_control` bit its revision 2 introduces.
  Nothing here is implementable before it lands.
- [`lazy-list-recur.md`](./lazy-list-recur.md) — defines rules S1–S5, the recur-sequence
  completeness set. §S4 (`observed iteration count == L`) is the rule this proposal weakens, and
  §"Recur sequences: no termination fact exists" is the reasoning it overturns.
- [`sequence-grammar-closure.md`](./sequence-grammar-closure.md) — the grammar this adds a form
  to. The `RecurBreak` capability is a new parameter kind a sequence may declare.

## Problem

`#[sequence(kind = recur)]` cannot stop early. `raster-macros/src/recur.rs:317` rejects it at
macro expansion:

```text
`#[sequence(kind = recur)]` cannot return `RecurControl`;
early termination must be decided inside recur tiles
```

That message points at the real constraint but reads as a workaround, and it is not one — it is
a consequence of what a sequence *is*. `RecurSequenceInput<T>` (`raster/src/input.rs:104-112`)
carries `item: AuthRef<T>` with no value accessor, and its doc says so outright:

> Recursive sequences are orchestration-only: they may pass this handle to normal tiles, but they
> cannot inspect item values or iteration position.

**A recur-sequence body physically cannot compute a break condition.** There is no expression it
could write. So the question is not "may a sequence break" but "may a sequence *relay* a break a
tile decided", and today the answer is no.

### The gap, and why the obvious workaround does not close it

Two loop forms exist, and neither covers "scan until a match, doing multi-step work per item":

| form | multi-step body | early exit |
| --- | --- | --- |
| `call_recur!` | no — one tile per iteration | **yes** |
| `call_recur_seq!` | **yes** — orchestrates tiles per item | no |

The workaround is a `done` flag in loop-carried state, with later iterations short-circuiting to
no-ops. It produces the right *output*, and it does not close the gap, because the cost `Break`
exists to avoid is not the body's work — it is the **iteration itself**. Every remaining
iteration is still a sequence invocation with its own steps, step records, storage transitions and
replay units. At `L = 100_000` a search that should stop at item 12 pays for 99_988 provably
empty iterations.

### Why this is not simply "allow `RecurControl`"

Lifting the `:317` panic alone would be unsound, and `lazy-list-recur.md` already says why. Its
recur-sequence rules rest on the fact that a sequence has *no* control to confuse a nested one
with:

> A containment-based rule ("some tile inside the iteration said `Break`") would read that as the
> outer loop terminating legitimately — handing the truncation this whole section exists to catch
> a free pass. It cannot arise: the inner `Break` is attributed to `[2][1][3]` by its own
> coordinates, and the outer loop has no control to confuse it with.

Give recur sequences a control and that ambiguity is exactly what returns. Distinguishing "a tile
that may end *this* loop" from "a tile that happens to be somewhere inside it" is the whole of the
design below.

## Design

### 1. The shape: decided by a tile, relayed opaquely

```text
capability in  ->  tile decides  ->  tile's output IS the sequence's return  ->  driver observes
```

The sequence never inspects anything. It threads an opaque handle to its return position, the
same way `into_ref!` keeps an item opaque. The orchestration-only invariant survives untouched.

Concretely, against the existing shape at `crates/raster/tests/recur_draft.rs:309-317`:

```rust
#[sequence(kind = recur)]
fn scan_rows(
    input: RecurSequenceInput<Row>,
    output: RecurSequenceOutput<Found>,
    needle: String,
    stop: RecurBreak,                                  // the capability
) -> RecurControl<RecurSequenceOutput<Found>> {
    let hit = call!(match_row, input, needle);         // ordinary tile, no capability
    call!(record_or_stop, output, hit, stop)           // holds it; its output is the return
}
```

**The capability-holding tile's output is the sequence's return value.** That is not a style
convention — it is what makes the relay checkable, because `InputBinding::PriorItemOutput`
(`raster-core/src/cfs.rs:625`) already records that the sequence's result derives from that tile's
output. The sequence has no other way to produce a `RecurControl`.

### 2. `RecurBreak` is CFS-pinned structure, not a signed object, and it is zero-sized

The intuition this proposal came from described a break object "signed by the current
RecurSequence". Raster has no signing infrastructure and needs none:
`InputBinding::SequenceScope { input_index }` (`raster-core/src/cfs.rs:622`) already records that
a tile argument came from the enclosing sequence's scope, and the CFS is hashed into program
identity (`program-identity.md`). A tile that was not handed the capability structurally cannot
have one.

That is **stronger** than a signature, not weaker. A signature would be a runtime value, subject
to the data-provenance rules and forgeable by anyone who can produce the bytes; a CFS binding is
program identity, and swapping it makes a different program.

**`RecurBreak` is a ZST.** It carries no runtime value. Postcard encodes it as nothing, so the
tile's ABI gains zero bytes and its `input_commitment` is unchanged; only the signature moves, so
the image id moves, which is correct.

This also keeps a new category out of the model. Raster has exactly two kinds of thing today —
data with provenance, and structure pinned by the CFS. A ZST capability is structure. A runtime
token would have been a third kind, and would have needed its own provenance rules.

### 3. Two invariants

> **I1 — Declaration.** A recur sequence may declare `RecurBreak` only if its body is non-empty
> and contains at least one execution unit (`Tile` or `RecurTile`).
>
> **I2 — Scope.** A break terminates the **innermost live recur frame**, and only that frame.

**I1 is a compile-time CFS check.** Only an execution unit produces a replay-proven break, so a
body without one could never break; declaring the capability there is a program error, not a
runtime condition. It has to be checked where the body's children are known, which is
`raster-compiler/src/flow_resolver.rs:124`, where `RecurSequenceItem` is built — **not** in
`raster-macros/src/recur.rs:233` (`validate_recur_sequence_shape`), which sees one function's
signature and cannot know what its body calls. The macro checks the *signature* half (a
`RecurBreak` parameter implies a `RecurControl` return and vice versa); the compiler checks the
*body* half.

**I2 is what makes this affordable**, and it is worth being explicit about what it buys.
`RecurProgressStack` (`raster-core/src/recur_progress.rs`) is strictly LIFO — the recorder models
the active site as a single `Option` and refuses an ordinary tile while iterations are live — and
`advance_tile_iteration` / `advance_sequence_iteration` operate on `last_mut()`. Under I2 that
stays true: a tile directly in the body at `[s][i][j]` has `[s]` as its innermost frame, while a
tile inside a nested `call_recur!` at `[s][i][k][…]` has `[s][i][k]`, so its break cannot name the
outer site at all.

The consequence is that **the capability's site check is a cheap assertion, not a forgery
defence** — it catches recorder bugs, because a genuine mis-scoped break is already impossible by
construction.

Without I2 the stack would become randomly addressable, inner frames would be unwound mid-sweep,
and their completeness would need redefining for the unwound case. That was the bulk of the cost;
I2 removes it.

### 4. The carried bit, and the parity trap it must not repeat

The tile breaks at `[s][i][j]`. The frame for `[s]` advances at the iteration boundary `[s][i]`,
which is a **later** step. The guest verifies one step per execution, so "a tile in this iteration
broke" has to be carried across steps — which puts it under `recur_progress_commitment` — which
means **the recorder must be able to see it too**.

That is precisely the failure that sank `recur-progress-commitment` revision 1: a field folded
into the commitment that only the verifier could observe, making every honest trace unverifiable.
Its §1 states the rule this proposal is bound by:

> Commit-and-recompute requires input parity. The producer that writes a commitment and the
> verifier that recomputes it must see the same facts.

**The fix is already in revision 2 and needs no new mechanism.** Its §3.1 adds `recur_control` to
`FnCallRecord` for recur-tile iterations, with the guest binding the trace copy to the replay
journal's copy. A break-capable tile in a recur-sequence body becomes one more producer of that
same field, under the same bind:

```text
trace_event.recur_control == replay_journal.<capability field>.control
```

Frame cost: one bit. `RecurProgressFrame.last_control` already exists and is currently constant
`Continue` for `Sequence` frames (`recur-progress-commitment` Uncertainty 6 asks whether to keep
it for that reason). This proposal answers that question — **keep it**, because it stops being
constant.

### 5. Journal membership

`lazy-list-recur.md` §5 says an ordinary tile in a recur sequence's own body emits `recur: None`,
justified by two clauses: "it carries no `RecurInput`, and the sequence cannot terminate early".
The first stays true; the second does not.

**Use a separate journal field rather than widening `recur`.** `TileReplayJournal.recur` means
"this tile was a recur-tile iteration", and a break-capable body tile is not one — it has no
`RecurInput`, no `iteration_index`, no `consumed_elements`. Overloading `recur` would force those
fields to be optional-within-optional and would make the membership rule ("by recur site, not by
subtree") ambiguous exactly where it matters most.

```rust
// raster-core/src/draft.rs
pub struct TileReplayJournal {
    // ...
    pub recur: Option<RecurTileReplay>,
    /// `Some` when the tile was handed a `RecurBreak` and is therefore able to
    /// end the enclosing recur sequence. Distinct from `recur`: this tile is
    /// not an iteration of anything, it is a step inside one.
    pub recur_break: Option<RecurControlKind>,
}
```

The two are mutually exclusive by I2 — a recur-tile iteration's own control is its `recur.control`
— which is worth asserting in the guest rather than leaving implicit.

### 6. Restating S4

`lazy-list-recur.md` §S4 is currently the strongest completeness statement in the design:

> **S4.** the observed iteration count equals `L`.

It becomes the prefix/terminal split the recur-tile family already has:

> **S4′.** the observed iteration count is `≤ L`; and if it is `< L`, the final iteration must
> carry a replay-proven `Break` attributed to this site.

Its two companion notes in that document change with it. "No prefix rule" and "no separate
empty-source rule" were both justified by "the runner has no early exit"; the first no longer
holds, while the second still does — `count == L` and `count < L with a proven Break` both cover
`L == 0` without a special case, since a sweep with zero iterations has no final iteration to
carry a break and therefore requires `L == 0` exactly as before.

**`close_site` converges.** `raster-core/src/recur_progress.rs`'s `close_site` currently branches
`Tile` versus `Sequence` with genuinely different rules; after S4′ both kinds run "complete prefix
on `Continue`, free on `Break`". That is a simplification of the code — but it should be adopted
as a deliberate trade, because what is being given up is a rule that held unconditionally.

## Soundness

- **The capability cannot be forged**, because it is not a value. It exists only as a CFS binding,
  and the CFS is in program identity. A program where a different tile holds it is a different
  program.
- **A break cannot be mis-attributed**, by I2: the terminated frame is the innermost live one, and
  a tile inside a nested loop has a different innermost frame. The containment trap
  `lazy-list-recur.md` describes cannot arise, for the same structural reason it could not before
   — the outer loop's control is reachable only from the outer loop's own body.
- **A break cannot be invented by the host.** The control is replay-proven in the tile's journal;
  the trace copy exists only for producer parity and is bound to it (§4).
- **Truncation still costs a proof.** A short sweep requires a final iteration whose journal
  carries `Break`. Dropping the tail without one fails S4′ exactly as it fails S4 today.

### What this weakens, stated plainly

S4 stops being unconditional. Before this change, "a recur sequence visited every element" is a
fact derivable from the trace's shape alone. After it, the claim is "visited every element, or
stopped at a replay-proven break" — strictly weaker, and it is the price of the feature. Any
release note or downstream claim (notably `paged-bytes`' sweep coverage) must use the second
form for recur sequences, not the first.

## Modules touched

| file | change | blast radius |
| --- | --- | --- |
| `raster/src/input.rs` | `RecurBreak` (ZST); recur-sequence runners observe the returned control | small |
| `raster-macros/src/recur.rs` | replace the `:317` panic with a conditional; `RecurBreak` parameter kind in `validate_recur_sequence_shape` (`:233`); recur-sequence return kind carrying `RecurControl` | moderate |
| `raster-macros/src/lib.rs` | capability-holding tiles fill `recur_break` from the same `ProtocolReturnKind` match that fills `recur` | small |
| `raster-core/src/draft.rs` | `recur_break: Option<RecurControlKind>` on `TileReplayJournal` | **journal encoding — image ids move** |
| `raster-core/src/trace.rs` | none beyond rev 2's `recur_control` — this reuses it | — |
| `raster-core/src/cfs.rs` | record the capability binding on `RecurSequenceItem` (`:614`) | small |
| `raster-compiler/src/flow_resolver.rs` | the I1 body check, where `RecurSequenceItem` is built (`:124`) | small |
| `raster-core/src/recur_progress.rs` | `advance_sequence_iteration` accepts a control; `close_site` converges to one rule set | small |
| `guests/transition/src/checks/cfs.rs` | S4′; the `recur_break` ↔ trace-bit bind; assert `recur` and `recur_break` are mutually exclusive | additive |

**Not** changed: the recur ABI, `RecurInput`, `RecurSequenceInput`'s opacity, `.rindex`, the
selector or proof-step enums, `RecurProgressStack`'s LIFO structure.

**Blocked on `recur-progress-commitment` revision 2.** Not merely ordered after it — §4 reuses its
trace bit and §6 rewrites the `close_site` it introduces. Landing this first would mean
implementing both halves of that proposal anyway, in the wrong order.

## Verification

- **The motivating program works:** a recur sequence scanning to a match with multi-step
  per-item orchestration stops at the match, and the trace contains iterations `0..=k` only.
- **The cost claim, measured:** the same program at `L = 1000` breaking at item 12 produces a
  trace with 13 iterations, against 1000 for the `done`-flag workaround. If this is not
  dramatically smaller, the feature does not justify itself and should be withdrawn.
- **I1:** a recur sequence declaring `RecurBreak` with an empty body, or a body of only nested
  sequences, fails to compile with a message naming the missing execution unit.
- **I2, the containment trap:** a `Break` from a tile inside a `call_recur!` nested in a
  recur-sequence iteration terminates **only** the inner site; the outer sweep still runs to `L`.
  This is the regression test `lazy-list-recur.md` already specifies for the current design, and
  it must keep passing with breaks enabled — it is the single most important test here.
- **Capability scoping:** a tile without `RecurBreak` cannot be given the sequence's return
  position (compile error), and a program that moves the capability to a different tile has a
  different program commitment.
- **Producer parity, the rev 1 lesson applied:** an honest end-to-end run of a breaking recur
  sequence **verifies** under `--commit`/`--audit`. No field added here may be one the recorder
  cannot see; check any new field against `recur-progress-commitment` §1's parity table before
  merging.
- **The trace bit is bound, not trusted:** a trace whose `recur_control` disagrees with the
  tile's `recur_break` journal value is rejected.
- **S4′, one test per branch:** `count == L` with a terminal `Continue` accepted; `count < L` with
  a proven `Break` accepted; `count < L` **without** one rejected; `count > L` rejected;
  `L == 0` with zero iterations accepted and with one iteration rejected.
- **Mutual exclusivity:** a journal carrying both `recur` and `recur_break` is rejected.
- Existing recur-sequence suites stay green with no capability declared — the feature is opt-in
  and a sequence without `RecurBreak` must behave exactly as it does today, S4 included.

## Performance

Negligible at runtime, and that is the point of the ZST: zero ABI bytes, zero
`input_commitment` change, one `Option<RecurControlKind>` (one byte when absent) per tile journal.

The gain is the whole motivation and is unbounded in the source length: a sweep that stops at
item `k` of `L` pays `k` iterations instead of `L`, in step records, storage transitions, replay
units and proving time alike.

## Uncertainties for review

1. **Is `RecurBreak` redundant with dataflow the CFS already pins?** The sequence must return the
   control it obtained, and `PriorItemOutput` already records that its return derives from that
   tile. One could argue the capability is implied and the parameter is ceremony.

   Against removing it: the same tile type at a recur-*tile* site means "break my own loop", and
   in a recur-sequence body means "break the enclosing sequence" — one type, two meanings,
   disambiguated only by position. The capability makes the meaning local to the signature, and
   makes "this program can terminate early" visible in the CFS without inferring it from a return
   type plus a position. That is the recommendation, but it is a real call and the cheaper design
   deserves an explicit rejection rather than an omission.
2. **Should `RecurBreak` name its site in the type?** Under I2 the site is determined by where the
   tile runs, so the type could stay opaque. Naming it (`RecurBreak<Site>`) would make a
   mis-scoped use a compile error rather than a guest assertion — but it needs a type-level site
   identity the language does not currently have.
3. **Should recur *tiles* gain the same capability spelling?** They already break via
   `RecurControl` with no capability, so there would be two idioms for one concept. Unifying
   is tempting and is a breaking change to a shipped surface; leaving them apart means the
   authoring guide has to explain why.
4. **Does `paged-bytes` care?** Its sweep-coverage claim rests on `lazy-list-recur` §5. If a
   region sweep is ever written as a breakable recur sequence, the claim weakens from "processed
   the whole artifact" to "processed it or stopped at a proven break". Probably fine — a sweep
   that must be complete simply does not declare the capability — but it should be stated there
   rather than discovered.
