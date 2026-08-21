# Proposal: `carried-state-channel` — one provable channel for state that crosses a step boundary

Status: proposed 2026-08-07 — **enhancement.** Nothing is blocked on it and no soundness claim
waits for it. It consolidates mechanisms that each work on their own, and it should be done
when the *second* of them is ready, not before (§When).

Related:
- [`recur-progress-commitment.md`](./recur-progress-commitment.md) — the first component, and
  the proposal that establishes the mechanism this one generalizes. It ships standalone, with
  its own field, on purpose (§When).
- [`loop-carried-state.md`](./loop-carried-state.md) — proposes `TrackedStateRoot` "mirroring
  `active_drafts`" (`:280`), which would make this the third copy of the same idea. Its
  implementer should extend this channel instead.
- [`draft-provenance.md`](./draft-provenance.md) — adjacent: it is about a draft losing its
  binding at `finalize`, not about a draft's root crossing a step. Different gap, same subject.

## Problem

Some state is not an input and not an output — it is what makes step *N+1* a continuation of
step *N*. The system has three such things, arrived at independently, and they have converged
on the same shape without ever being named as one:

| state | carrier | per-step anchor | continuity check | trust at a window open |
| --- | --- | --- | --- | --- |
| draft roots | `Transition.active_drafts` + `InitTransition.active_drafts` | `DraftReplayTransition.root_before` — replay-proven | `checks/drafts.rs:69`, **permissive**: `if let Some(..)` | host-supplied, unchecked |
| loop-carried state roots | `TrackedStateRoot` (proposed) | storage read + selection witness | proposed, "mirroring `active_drafts`" | would be host-supplied, unchecked |
| recur progress | `RecurProgressStack` (proposed) | `recur_progress_commitment` on `StepRecord` | advance-and-compare per step | **checked** — the point of that proposal |

The first two share three defects, and the third fixes all of them for itself alone:

1. **A window that opens mid-chain inherits state it cannot verify.** The transition guest
   proves one step per execution (`guests/transition/src/main.rs:29`); a fresh `Init` has no
   previous journal, so its starting state is read off the host and compared against nothing.
2. **Absence is not a claim.** `if let Some(tracked_state)` means a dropped map entry is not a
   failure, so the cheapest attack on a continuity check is to omit its subject.
3. **Each new kind of carried state re-litigates the same design.** Three carriers, three seed
   fields, three places to get the same argument right or wrong.

`recur-progress-commitment` answers all three for one kind of state. This proposal is the
observation that the answer is not specific to loops, and that the *next* kind of carried
state should extend a channel rather than open one.

## Design

### 1. What counts as carried state

Three properties, all three required:

- it lives in `Transition` because a later step's validity depends on it;
- its value at step *N+1*'s start must equal its value at step *N*'s end;
- it is **not** the step's own output — outputs are already bound by
  `output_commitment` and, for tiles, by a replay proof.

Anything meeting those joins the channel. Anything failing them does not (a step's output, a
storage root, `next_expected_coordinates` — see §5).

### 2. One struct, one commitment, one seed

```rust
// raster-core/src/carried_state.rs
pub struct CarriedState {
    pub recur_progress: RecurProgressStack,
    // pub drafts:       DraftRootChain,        // when §4 answers the creation question
    // pub state_roots:  LoopStateRootChain,    // when `loop-carried-state` lands
}

impl CarriedState {
    /// `H(b"carried-state" ‖ postcard(self))`. The all-empty value is canonical
    /// and non-zero, so "nothing is in flight" is a statement in the trace
    /// rather than a missing field.
    pub fn commitment(&self) -> Hash32;
}
```

| carrier | contents | pinned by |
| --- | --- | --- |
| `StepRecord.carried_state_commitment: Hash32` | the commitment, **after** this step | fingerprint agreement with `commit.bin`, like `storage.root_before` |
| `Transition.carried_state: CarriedState` | the preimage | `verify_previous_journal` + `assert_state_continuity` |
| `TransitionInput.window_start_carried_state: Option<CarriedState>` | the preimage at a fresh `Init` | reproduced, never believed — §3 |

`InitTransition` carries none of it. Migrating `active_drafts` into the channel therefore
*removes* a field from `InitTransition` rather than adding one.

### 3. The check, unchanged from `recur-progress-commitment`

```text
advance(carried, this step's facts).commitment() == step.carried_state_commitment
```

`carried` comes from `Transition` for a `Next` step and from `window_start_carried_state` at a
fresh `Init`. A wrong seed advances to a different value and fails against the recorded one, so
mid-chain window opening costs nothing and proves something. Recording only the state *after*
each step is what lets one 32-byte field do this — the seed is validated by reproducing the
first window step's own commitment, not by matching a predecessor record the window does not
contain.

Each component supplies two functions, and the channel supplies nothing else:

```rust
pub trait CarriedComponent {
    fn advance(&mut self, step: &StepRecord, witnesses: &StepWitnesses)
        -> Result<(), CarriedStateViolation>;
    fn is_empty(&self) -> bool;         // for the canonical empty commitment
}
```

`advance` must be one shared implementation called by **both** the native recorder (which
stamps the commitment) and the guest (which checks it). A divergence between two copies is a
verification failure that no test of either copy alone would catch — the same reason
`chunking.rs` lives in `raster-core` today.

### 4. What each component needs before it can join

This is the substance of the proposal; the struct above is bookkeeping.

**`recur_progress` — ready.** Ships in `lazy-list-recur` phase 5 with its own field
(`recur_progress_commitment`). Joining the channel is a rename of the field and the seed.

**`drafts` — one open question first.** `create_draft` (`raster-runtime/src/storage.rs:817`)
emits no trace event; it is a thread-local insert in a sequence body. So on a draft's first
mutating step there legitimately is no tracked entry, which is exactly why `checks/drafts.rs:69`
is permissive. To make absence impossible the trace must first express *creation* — either a
step, or a creation marker on the first mutating step's journal, or a rule deriving it from the
draft anchor's synthetic coordinates (`storage.rs:822`). Until that is decided, folding drafts
in would preserve the hole under a better name.

**`state_roots` — waits on its own proposal.** `loop-carried-state` is not implemented; there
is nothing to carry yet. The dependency is one-directional: that proposal should reference this
channel instead of specifying a map, and nothing here waits on it.

### 5. What does *not* join

- **Storage roots.** Already anchored per step (`storage.root_before` vs the current frontier,
  `checks/store.rs:154`) and the frontier is reconstructed, not inherited.
- **`next_expected_coordinates`.** It is unconstrained at a fresh `Init` and that is a real gap
  — but it is derivable from the first step's own coordinates plus the CFS, so it wants
  *derivation*, not a carrier. Naming it here would be scope creep; it is recorded in
  `recur-progress-commitment` §Problem as a sibling observation.
- **Anything a step outputs.** Already bound by `output_commitment` and the replay proof.

## What this does *not* buy

Stated plainly because it was got wrong once already, in review of
`recur-progress-commitment` §Uncertainty 1:

> **The channel does not avoid a trace-format break.** The commitment is a hash over a struct;
> adding a component changes the encoded bytes — including for a trace where the new component
> is empty, since an empty collection still encodes — so every step's commitment changes, hence
> every trace item hash, hence every fingerprint. Adding a component later costs exactly what
> adding a second `StepRecord` field costs.

A component map keyed by name (`BTreeMap<ComponentId, Hash32>`, absent when empty) would dodge
that for traces not using the new component. It is rejected: absent-means-unchecked is the
precise weakness the canonical empty value exists to remove, and nearly every trace has drafts
anyway, so it buys little and costs the invariant.

What the channel does buy is narrower and real: one seed field instead of three, one
advance-and-compare in the guest instead of three, one place where this argument is settled,
and a name that tells the next author there is a channel to join.

## When

Do it when the **second** component is ready — that is, when either the draft-creation question
(§4) is answered or `loop-carried-state` lands. Two reasons for the ordering:

1. A one-member category is speculative generality: `CarriedState { recur_progress }` promises
   a shape for state whose requirements (draft ids and schema hashes; `state_id` and fixpoint
   termination) are not yet designed, and a wrong shape is harder to remove than to add.
2. There is no cost to waiting, because the break is unavoidable in either ordering.

When it does happen, batch it with a release that already breaks the trace format — every
proposal in the `lazy-list-recur` / `paged-bytes` line does — so the migration is shared rather
than doubled.

## Modules touched (sketch)

| file | change |
| --- | --- |
| `raster-core/src/carried_state.rs` | new: `CarriedState`, `CarriedComponent`, `commitment()`, `CarriedStateViolation` |
| `raster-core/src/trace.rs` | `recur_progress_commitment` → `carried_state_commitment` on `StepRecord` |
| `raster-core/src/transition.rs` | `carried_state` on `Transition`; `window_start_carried_state` on `TransitionInput`; **remove** `active_drafts` from `Transition` *and* `InitTransition` |
| `raster-runtime/src/tracing/recorder.rs` | stamp one commitment instead of one per component |
| `guests/transition/src/checks/drafts.rs` | the permissive `if let Some(..)` becomes a required continuity check, once §4 is answered |
| `guests/transition/src/checks/cfs.rs`, `fraud_proof.rs` | one advance-and-compare for the whole channel |

## Verification

- The `recur-progress-commitment` suite must pass unchanged after the rename — the channel adds
  no behaviour to that component, so any diff in its tests means the migration changed
  semantics.
- The window-open attack, per component: a seed claiming progress/roots that did not happen is
  rejected because advancing it does not reproduce the recorded commitment.
- **Absence is a claim:** for each component, a seed omitting a live entry fails; the all-empty
  state verifies against the canonical commitment.
- Recorder/guest agreement on `advance` for every component, over one fixture exercising all of
  them together — the divergence a single shared implementation exists to prevent.
- Draft continuity across a window boundary: a spliced draft chain (a root the tile legitimately
  produced, but not the one the previous step of *this* chain ended with) is rejected. This is
  the case `if let Some(..)` accepts today, and the reason drafts belong in the channel at all.

## Uncertainties for review

1. **Whether `advance` can really be one signature.** Recur progress advances from the replay
   journal plus selection proofs; drafts advance by re-applying ops to a witness; state roots
   advance by hash equality. If the witness bundle a component needs cannot be expressed
   without a union type that every component ignores most of, the trait is the wrong shape and
   the channel should be three components sharing a commitment rather than a trait.
2. **Whether the draft creation question belongs here or in `draft-provenance`.** It is a trace
   expressiveness question, not a carrier question; this proposal only needs the answer.
3. **Ordering against `loop-carried-state`.** If that proposal lands first, it should be built
   *into* the channel directly rather than shipping a `TrackedStateRoot` map that this then
   migrates — one break instead of two.
