# Proposal: `loop-carried-state` — recur state by reference, not only by value

Status: Proposed (2026-07-30).
Related: [`bounded-collections.md`](./bounded-collections.md) — made unbounded
materialization unrepresentable; this proposal is about the value that *survives*
an iteration, which that document did not cover.
[`draft-provenance.md`](./draft-provenance.md) — the sibling gap on the other
side of the same wall: there, a collection built in a tile loses its binding;
here, a collection cannot cross an iteration at all.
[`carried-state-channel.md`](./carried-state-channel.md) — **read before building
§2's `TrackedStateRoot`.** Mirroring `active_drafts` (`:280` below) reproduces its
window-open gap: a map carried in `Transition` is host-supplied and unchecked at a
fresh `Init`, so a window opening mid-loop inherits a state root nobody proved.
[`recur-progress-commitment.md`](./recur-progress-commitment.md) shows the fix —
commit the state per step and reproduce it from the seed — and the channel proposal
is where this state root should live rather than in a third parallel map.
Motivating program: `raster-inference/raster-tokenizer`, whose BPE merge loop is
unrolled 48 times because its loop-carried value is a `List`.

## Problem

A recur site has two loop-carried slots, and they are built on opposite
foundations.

**`output` is a committed root chain.** `RecurOutput<S>` is a `Draft<S>`. Each
iteration emits a `DraftReplayTransition { draft_id, schema_hash, root_before,
ops }` (`crates/raster-core/src/draft.rs:30`), and the transition guest chains
them (`crates/raster-prover/guests/transition/src/checks/drafts.rs`):

```rust
if let Some(tracked_state) = active_drafts.get(draft_id) {
    assert_eq!(tracked_state.root, *root_before,
        "Replay journal root_before does not match tracked draft root");
}
let (_, root_after) = apply_draft_ops(&witness.pre_state, ops)...;
active_drafts.insert(*draft_id, TrackedDraftState { schema_hash, root: root_after });
```

Iteration *N*'s pre-state root must equal iteration *N−1*'s post-state root. The
draft is never materialized to cross the boundary; only its root travels, and
continuity is proved.

**`state` is an inline value.** `RecurState<T>` (`crates/raster/src/input.rs:98`)
and `RecurSequenceState<T>` (`:119`) both hold `T` **by value**. The state is
serialized into each iteration's `FnInput.values` as `FnInputValue::Inline`, and
the guest's entire obligation for an inline binding is
(`checks/cfs.rs:339`):

```rust
InputBinding::Direct(InputSource::Inline) => {
    assert!(matches!(resolved_source, ResolvedSource::Inline(_)), ...);
}
```

It checks that the source *is* inline. Nothing compares iteration *N*'s state
bytes against iteration *N−1*'s output. The tile replay journal's
`input_commitment` binds `B(recorded input) = output` for each iteration in
isolation, and `verify_recur_iteration_chunking` (`checks/cfs.rs:123`) checks
chunk widths — but no check links one iteration's carried value to the next.
The codebase already names what that costs
(`crates/raster-compiler/src/flow_resolver.rs:581`):

> Binding it as `Inline` would let a claimed trace substitute arbitrary bytes for
> it and still verify.

So the two slots have different security properties, and nothing in the model
says why. **This is the primary claim of this proposal: the asymmetry is a bug,
not a design.**

The seam is already visible in the API. `RecurSequenceState` accepts an
`AuthRef` today (`crates/raster/src/input.rs:1627`) — and throws the reference
away:

```rust
impl<T> From<AuthRef<T>> for RecurSequenceState<T> {
    fn from(value: AuthRef<T>) -> Self {
        Self { inner: into_auth_value::<T, _>(value)?.into_inner() }   // materializes
    }
}
```

A step that returns a tile's `AuthRef` as its next state already has it
materialized behind its back. Everything below is the missing half of that
conversion, not a new concept.

### The expressiveness consequence

The same asymmetry is why a loop cannot carry a collection.

Because `state` is materialized, its type must survive a tile boundary, and
`bounded-collections` (correctly) forbids a `List<T>` there. Because it is
*opaque* — `RecurSequenceState` implements neither `SelectSource`
(`crates/raster/src/input.rs:986`, `:1020` are the only impls) nor
`__raster_into_ref` (`:227`, `RecurSequenceInput` only) — a value placed in it
cannot be selected back out. And `Block<T>` is materializable but cannot drive a
recur. So:

| type | can be carried in `state` | can drive a recur |
| --- | --- | --- |
| `List<T>` | no — not `Materializable` | yes |
| `Block<T>` | yes | no |

There is no type that does both, so **no loop can transform a collection**.
Algorithms that iterate a mutating sequence to a fixed point — BPE merging is the
motivating one — have no expressible form.

The tokenizer's workaround is to unroll:

```rust
let round_1 = call_seq!(merge_round, pieces, clone!(merge_buckets));
let pieces_1 = select!(List<Piece>, round_1.pieces);
// ... 48 times
call!(assert_merges_complete, leftover_found)?;   // fails if the budget was short
```

That is 96 lines and 50 CFS items to express one loop; it pins an arbitrary
constant into `program_commitment`; and it caps prompt length at a number
unrelated to anything in the tokenizer. The check at the end is the honest part —
an over-long prompt errors rather than tokenizing incorrectly — but a program
should not have to buy correctness with a budget it cannot size.

## Why `output` cannot serve

The first objection to a third slot is that `output` is already a loop-carried
structure with a committed root chain — so why not put the collection there?
Three reasons, and the first is decisive on its own.

**1. An output draft is write-only inside the loop.** `RecurOutput<S>` is a
`Draft<S>`, and a draft exposes write handles only (`.field().set(v)`,
`.list().push(v)`). `SelectSource` is implemented for `TypedStorageBinding` and
`AuthRef` and nothing else (`crates/raster/src/input.rs:986`, `:1020`), so a
draft cannot be selected into at any point. It becomes readable only when the
*site* finalizes it, after the loop has ended.

A fixpoint round must **read** the previous round's value — BPE needs the
adjacent pairs to find the next merge. There is no iteration at which that read
is expressible against a draft. This alone rules `output` out, before any
argument about representation or cost.

**2. Drafts are monotonic, and the guest depends on it.** `apply_draft_ops`
(`crates/raster-core/src/draft.rs:734`) — the function the transition guest runs
— enforces the two modes of `SchemaFieldMode`
(`crates/raster-core/src/input.rs:173`):

```rust
DraftOp::Set { .. } => {
    if fields.contains_key(field) {
        return Err(... "Draft field '{}' can only be written once");
    }
}
DraftOp::Push { .. } => { /* AppendOnlyVec only */ }
```

Set-once fields, append-only lists — a verification rule, not a host-side
convention. A BPE round *replaces* its list with a shorter one (35 pieces, then
34, then 33), which no sequence of `Set`/`Push` can express.

Monotonicity is also *why* the chain is checkable: the guest derives `root_after`
by applying increments to an authenticated pre-state. A `DraftOp::Replace` would
dissolve that property rather than extend it.

**3. A `Replace` op would inline the whole value, every iteration.** Ops travel
in the replay journal as data. A replace carrying a new `List` would put the
entire collection in each iteration's ops — O(N²) inline trace bytes for an
N-iteration loop. A state reference passes a 32-byte root to a value already
committed through the ordinary storage path, so nothing is inlined and the
per-iteration cost is a hash comparison.

### The two are complementary, not competing

The BPE loop wants **both**, at different levels:

```rust
#[sequence]
fn merge_round(pieces: List<Piece>, merge_buckets: List<MergeBucket>) -> PieceSequence {
    // ...
    call_recur!(
        tile = apply_merge,
        input = pieces,
        output = new!(PieceSequence),   // output BUILDS round N+1's list, append-only
        args = (found, index, merged_token)
    )
}
// the state ref PASSES that finished list into round N+1, readable.
```

Within a round the new list is built by an append-only draft — exactly what
`output` is for, and it already works today. Across rounds the finished value has
to travel as something readable.

> **`output` is how a loop builds a value; a state reference is how it passes one.**

A recur's output is legible only after the loop ends; there is no channel today
by which iteration *N+1* reads iteration *N*'s result. A by-reference state is
that missing channel. It is also why unrolling works around the gap at all: each unrolled
`call_seq!` is its own site, so each round's output *does* get finalized and
become readable — at the cost of a CFS item per round and a fixed budget.

## Goal

1. Loop-carried state is bound across iterations, at the same strength `output`
   already enjoys.
2. It may be a collection, because it can travel as a root rather than as bytes.
3. Fixpoint loops become expressible, with a termination rule the verifier can
   check.

## Surface

**One slot; recur sequences gain a second representation.** `state` keeps its key
and its role — the loop-carried intermediate. What is new is a by-reference form,
following the `AuthRef` / `AuthValue` split the codebase already uses everywhere
else (`IntoAuthRef`, `crates/raster/src/input.rs:969`; `IntoAuthValue`, `:1477`):

| step kind | type | holds | for |
| --- | --- | --- | --- |
| recur tile | `RecurState<S>` | `S` by value, inline | scalars — unchanged |
| recur sequence | `RecurSequenceState<S>` | `S` by value, inline | scalars — unchanged |
| recur sequence | **`RecurSequenceStateRef<S>`** | `AuthRef<S>` | collections, large structs |

### Why there is no tile-side `RecurStateRef`

The by-reference form exists for recur **sequences** only, and the restriction is
the model's central distinction rather than an implementation limit: *sequences
route authorized references; tiles receive materialized values.*

A recur tile could do nothing with a reference. `RecurState<T>` holds `T` by
value (`crates/raster/src/input.rs:98`) and the tile body mutates it directly —
to read a reference the tile would have to materialize it, which is no different
from today except for the indirection, and is forbidden outright when `S` holds a
`List`. Nor could a tile *produce* one: tile outputs are plain values that the
call site commits, so a tile has no way to return an `AuthRef` at all.

This is the same boundary that lets `Draft<T>` cross into a tile while `AuthRef<T>`
does not — a draft is a write-only protocol handle, whereas a reference is a
sequence-level concept. A loop whose carried value is a collection needs
sequence-level reads (`into_ref!`, `select!`) on that value anyway, so it wants a
recur sequence regardless; the BPE round is exactly this case, since its bucket
lookups are `select!`s.

```rust
#[sequence(kind = recur)]
fn merge_round_seq(
    input: RecurSequenceInput<Piece>,
    state: RecurSequenceStateRef<PieceSequence>,   // the only change from today
    merge_buckets: List<MergeBucket>,
) -> RecurSequenceStateRef<PieceSequence> {
    let current = into_ref!(state);                 // AuthRef — materializes nothing
    let pieces = select!(List<Piece>, current.pieces);
    call_seq!(merge_round, pieces, merge_buckets)    // becomes the next state
}
```

```rust
let merged = call_recur_seq!(
    sequence = merge_round_seq,
    input = pieces,           // the budget: L pieces => at most L-1 merges
    state = exploded,         // already an AuthRef — no seed macro needed
    fixpoint = true,          // stop when the state root stops changing
    args = (merge_buckets,)
);
```

The call-site key is unchanged: the step's declared parameter type decides the
representation, exactly as `From<AuthRef<T>>` vs `From<T>` already dispatches
today. `RecurSequenceStateRef<S>` implements `SelectSource`, so the sequence body
can select into it; passing it to a tile materializes it, as any `AuthRef` does.

Making the choice a **type** rather than a convention is the same move
`bounded-collections` made with `List` / `Block`: the authoring rule "state must
stay scalar-small" stops being prose and becomes something the signature states.

## Mechanism

### 1. The reference travels as a storage binding

Each iteration records the state reference in `FnInput.storage` as ordinary `StorageData`
(`crates/raster-core/src/trace.rs:37`) — coordinates, commitment, selector,
selection — not in `values` as `Inline`. The guest's existing read loop
authorizes it with no change: `verify_storage_read_witness` plus
`verify_selection_witness`, exactly as for any other storage-backed value. This
is the same "do not introduce a second way to authorize a value" move that
`dynamic-index-selection` made for indexes.

### 2. Continuity is the draft check, generalized

Add a `TrackedStateRoot { schema_hash, root }` map keyed by a `state_id`,
mirroring `active_drafts`:

```text
iteration 0:  state root == the seed's committed root          (from the call site)
iteration N:  state root == iteration N-1's recorded output commitment
final:        the site's binding == the last iteration's state root
```

Each is an equality between two hashes the guest already has. Nothing new is
proved about the *contents* — the contents are authorized by the storage read;
what is new is that the chain cannot be broken, which is precisely what the
inline form lacks today.

Note this is strictly cheaper than the draft mechanism it copies: a draft must
re-apply `ops` to a pre-state witness to derive `root_after`, because it mutates
in place. A state reference is *replaced* wholesale by a value already committed
in its own right, so continuity is a hash comparison and nothing needs replaying.

### 3. Termination: fixpoint on the state root

`fixpoint = true` stops the loop when an iteration's output root equals its input
root. This is worth having for its own sake, but it also resolves a modelling
objection: without it, a fixpoint loop's `input` is a budget the step body never
reads, which is the "fake recur" shape the authoring rules call a confession.

With `fixpoint`, the two roles separate cleanly and both are honest:

- `input` is the **budget**, and it is a real one derived from the data — L
  pieces admit at most L−1 merges, so the list bounds the loop by construction
  rather than by a constant someone chose.
- the **stopping rule** is `root_after == root_before`, a hash equality recorded
  in the trace and re-checkable by the guest.

The verifier's obligation is symmetric and cheap: for every iteration before the
last, roots must differ; at the recorded last iteration, they must be equal (or
the budget must be exhausted). A prover cannot stop early — that would require
two equal roots it does not have — and cannot run long, because the budget is
pinned by the input list's committed length.

This is a strictly stronger termination story than the unrolled form, where
"did we converge?" is a program-level assertion inside a tile
(`assert_merges_complete`) rather than a property of the schema.

### 4. The inline form stays, and gets chained regardless

`RecurState<S>` and `RecurSequenceState<S>` keep their meaning and cost model:
inline is the right representation for a counter, and a scalar should not pay a
storage round-trip per iteration. Nothing about existing folds changes.

**The two halves of this proposal have different scope, and it is worth being
explicit:**

| | recur tile | recur sequence |
| --- | --- | --- |
| chaining fix (§Problem) | applies | applies |
| by-reference state | not applicable (above) | applies |

So the soundness fix covers every recur in every program today — including the
seven state-carrying recur tiles in the motivating tokenizer — while the new
representation is available only where references are the medium. Closing the
gap needs none of the machinery above: bind iteration *N*'s state bytes to
iteration *N−1*'s output bytes, a hash equality. That fix is smaller than this
proposal and should land first; see Migration.

The two forms then have the same guarantee and differ only in representation,
which is the point: today they differ in *both*, and only by accident.

## What this does not fix

- **It does not make `List` materializable.** A referenced state is read by
  `select!` or by materializing it into a tile, under the existing rules. One
  holding a `List` still cannot be passed whole to a tile — `bounded-collections`
  is untouched.
- **It does not bound the state's size.** A referenced state that grows every iteration
  re-commits a growing value, and the O(N²) warning that applies to `state`
  applies to the by-reference form too — with the difference that the cost is
  Merkleization of a root chain rather than re-serialization of bytes. Guidance,
  not a type rule.
- **It does not give recur sequences general early exit.** `fixpoint` is a
  specific, checkable stopping rule. `RecurControl` in recur sequences remains
  out of scope, because a data-dependent break has no equivalent verifiable
  witness.
- **It does not remove the tokenizer's need for a budget** — it replaces a
  constant with the piece list, which is the right budget rather than no budget.

## Edge cases

- **Empty input.** The seed is the result, as for `state` today.
- **A state never rewritten.** Roots equal at iteration 0; `fixpoint` stops
  immediately, which is correct.
- **State reference and output together.** Legal; independent chains keyed by
  different ids. The step returns the pair.
- **Wanting an inline scalar *and* a referenced collection.** One slot means
  one or the other. Bundle them into the referenced struct — which costs one
  root per iteration instead of two, and is what a program should want anyway.
- **Seeding.** `state = <expr>` must be an authorized reference when the step
  declares `RecurStateRef` — a `call!`/`call_seq!`/`select!` result. Seeding from
  an inline literal would reintroduce at iteration 0 the hole this proposal
  closes; a literal seed should be stored first. This is a bound the macro can
  enforce, since `RecurStateRef<S>` would implement `From<AuthRef<T>>` and not
  `From<T>` — the exact inverse of today's pair.
- **Program identity.** A by-reference state is a new CFS shape, so any program
  adopting it re-locks. Programs keeping the inline form are unaffected.

## Migration

1. **Close the `state` chaining gap** (§4). Independent, small, and a strict
   soundness fix on today's programs — it should not wait for the rest.
2. **`RecurStateRef` + storage-binding recording**, with the continuity check in
   the transition guest. Negative guest tests per obligation: substituted state
   root, broken chain, reference absent, seed not authorized.
3. **`fixpoint = true`** and its two-sided termination check.
4. **Migrate the tokenizer** off its 48 unrolled rounds and compare: the output
   commitment must be bit-identical, which makes this a clean regression test for
   the whole feature.

## Open questions

- **Should the inline form eventually go away?** If the chaining fix (§4) lands
  and storage round-trips get cheap enough, one representation would be simpler
  than two. Against: a scalar counter genuinely does not want a root per
  iteration, and the inline form is why folds are cheap today. Proposed: keep
  both, and let the type carry the guidance.
- **Is `fixpoint` the right spelling, or should the step signal it?** A step
  returning its state reference unchanged is already the signal; `fixpoint = true` only
  tells the driver to *act* on it. An alternative is to always stop on a stable
  root, making it the semantics rather than a flag. That would be simpler but
  changes the meaning of existing state-only recurs if the two slots are ever
  merged.
- **How does a by-reference state interact with `chunk = N`?** Chunking bounds
  what an iteration *reads* from the input; it says nothing about the state. They
  should compose, but the combination is untested and worth a UI test.
- **Does the fraud proof need anything new?** Such a step's divergence is located
  exactly like any other step's, because the state is a storage binding and the
  roots are in the trace. Believed to be free; worth confirming against
  `chain-fraud-proof.md` before implementation.
