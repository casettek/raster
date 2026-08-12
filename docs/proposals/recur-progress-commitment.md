# Proposal: `recur-progress-commitment` — cross-step loop state a window can verify instead of inherit

Status: proposed 2026-08-07

Extracted from [`lazy-list-recur.md`](./lazy-list-recur.md) §5, which needs this and cannot
state it: §5 defines the per-iteration *facts* and the completeness *rules*, and assumes a
carrier for the accumulation those rules read. There is no sound carrier today.

Related:
- [`lazy-list-recur.md`](./lazy-list-recur.md) — **the consumer.** Its rules 1–8 and S1–S5
  accumulate across iterations; without this, they bind only in a window that happens to
  contain iteration 0, and the prover chooses the window. Ship together (§Dependency).
- [`paged-bytes.md`](./paged-bytes.md) — depends transitively: its sweep-coverage claim rests
  on `lazy-list-recur` §5.
- [`loop-carried-state.md`](./loop-carried-state.md) — proposes `TrackedStateRoot` "mirroring
  `active_drafts`" (`:280`), inheriting the identical gap. §7 explains why one mechanism should
  serve both.
- [`chain-fraud-proof.md`](./chain-fraud-proof.md) — the window model this extends. It already
  closed the neighbouring hole where the *window fingerprint* was host-claimed; the state a
  window opens with was not in its scope.
- [`carried-state-channel.md`](./carried-state-channel.md) — **good to implement later, not
  now.** It generalizes this mechanism into one channel serving draft roots and loop-carried
  state roots as well. Deliberately deferred: a one-member category is speculative generality,
  and deferring costs nothing because adding a component breaks the trace format exactly as
  much as adding a field does (§7, §Uncertainty 1).

## Problem

The transition guest proves **one step per execution** (`guests/transition/src/main.rs:29`).
Anything that must hold *across* steps travels in `Transition`, which is pinned by the previous
journal's recursive receipt (`fraud_proof.rs:288`, `:303`) — sound for every `Next` step. A
window's **first** step has no previous journal, so its starting state comes from
`InitTransition`, read straight off the host.

That is fine for the fields that are re-derived or compared against something. It is not fine
for a map that is only ever read.

### What `Init` actually binds

| field | bound by |
| --- | --- |
| `init_frontier.position` | `assert_window_is_commitment_slice` derives the covering block range from it (`fraud_proof.rs:230`) |
| `init_frontier.leaf` / `ommers` | indirectly: they feed `frontier_root` for every window item, and items 1…w−1 are compared against the committed fingerprint |
| `init_storage_root` | the first step's `storage.root_before` assertion (`checks/store.rs:154`), and that record is fingerprint-pinned |
| `active_drafts` | **nothing** |
| `next_expected_coordinates` (absent at `Init`) | **nothing** — the window's first step's coordinates are unconstrained |

### The attack

Give `lazy-list-recur` §5 the obvious carrier — a per-site map in `Transition`, seeded from
`InitTransition` — and a fresh window opening at a recur site's own step can claim:

```text
site [2]:  next_iteration_index = 9,  consumed_total = 9
```

Rules 5 and 7 then pass over nine iterations that were never verified by anybody. The prover
picks where windows open, so this is not a corner case; it is the default way to defeat the
rules.

### The pattern the codebase already follows

`checks/drafts.rs` is the precedent, read correctly. The **anchor** for each draft step is
`DraftReplayTransition.root_before` — replay-proven, in the journal, and additionally
authenticated by a witness whose root must match it (`:55`). The carried `active_drafts` map is
*only* a continuity check, and it silently no-ops when the draft is absent (`:69`,
`if let Some(tracked_state) = ...`). So:

> **per-step anchor: authenticated. Carried map: continuity only, never the authority.**

A recur-progress map in `Transition` with no per-step anchor inverts that. This proposal
supplies the missing anchor.

### Two fixes that do not work

**Refuse to open mid-loop.** The guest can detect a mid-loop open from the first step's
coordinates plus the CFS, and panic. Sound, and unacceptable: refuting a divergence at
iteration 900 of a 1000-iteration loop forces the window to open at iteration 0, turning a
128-item window into a 900+-item one. Window size is the proving budget; a rule that makes it
data-dependent is not a rule anyone can operate.

**Prove one step of prefix.** `SerializableFrontier` carries `leaf` (`transition.rs:27`), which
*is* `hash_trace_item(step_{s−1})`, so the predecessor record can be proven for free. But rules
5 and 6 need that step's *journal* too (its `control`), which costs an extra `env::verify` per
window — and one step of carry is all it ever gives. It is a boundary patch, not a mechanism.

## Design

### 1. The state

```rust
// raster-core/src/recur_progress.rs — no_std, shared by the recorder and the guest
pub struct RecurProgressFrame {
    pub site: CfsCoordinates,
    pub kind: RecurSiteKind,             // Tile | Sequence
    pub chunk: u64,                      // C — the CFS literal, 1 when unchunked
    pub source_len: u64,                 // L
    pub next_iteration_index: u64,       // the name the recorder already uses
    pub consumed_total: u64,             // running sum of the journals' `consumed_elements`
    pub last_control: RecurControlKind,
}

/// Innermost last. Nesting is strictly LIFO — the recorder models the active
/// site as a single `Option` (`recorder.rs:722`) and refuses an ordinary tile
/// while iterations are live (`:650`) — so this is a stack, not a map.
pub struct RecurProgressStack(Vec<RecurProgressFrame>);
```

Every field is under the commitment, and each one blocks a specific forgery:

| field | what it stops |
| --- | --- |
| `site`, `kind` | attaching one loop's progress to another's iterations; the two kinds have different rule sets |
| `chunk` | re-declaring `C` mid-loop, which would make rule 4's `min(C, L − consumed_total)` partly prover-chosen |
| `source_len` | switching `L` mid-loop. Learned at iteration 0 from the item's own selection proof (`List.len` / `ListRange.len`, both inputs to the `b"list-root"` hash), re-asserted each iteration, cross-checked against `lazy-list-recur` §1's metadata at the site step |
| `next_iteration_index` | rules 1, 2 — first index is 0, indices are contiguous |
| `consumed_total` | rules 4, 5. Derivable (`min(next_iteration_index · C, L)`) once rule 4 holds; kept explicit so `close_site` compares rather than re-derives |
| `last_control` | rule 6. The **only** field the following step cannot derive from its own authenticated data — a `Break` is invisible to the iteration after it |

### 2. The commitment

```text
recur_progress_commitment = H(b"recur-progress" ‖ postcard(RecurProgressStack))
```

The empty stack has a canonical value, so *"no loop in flight"* is a positive statement in the
trace rather than an absent field. That is what lets **any** step seed a window, including an
ordinary tile inside a recur-sequence iteration.

### 3. Where it travels

| carrier | contents | pinned by |
| --- | --- | --- |
| `StepRecord.recur_progress_commitment: Hash32` | the commitment, **after** this step | fingerprint agreement with `commit.bin`, exactly like `storage.root_before` |
| `Transition.recur_progress: RecurProgressStack` | the preimage | `verify_previous_journal` + `assert_state_continuity` |
| `TransitionInput.window_start_recur_progress: Option<RecurProgressStack>` | the preimage, at a fresh `Init` | checked against the record — see §4 |

The preimage travels in the clear because the guest must **advance** it (`next_iteration_index
+ 1`, `consumed_total + consumed_elements`, `last_control = …`) and a hash cannot be advanced.
The commitment exists for the one thing the preimage cannot do: seed a chain that starts
mid-loop.

`InitTransition` gains **nothing**. That is the point of the proposal.

### 4. The per-step check, and how a window opens

One rule, uniform for every step:

```text
advance(carried, this step's facts).commitment() == step.recur_progress_commitment
```

where `carried` is `Transition.recur_progress` for a `Next` step and
`window_start_recur_progress` for a fresh `Init`. The seed is therefore never believed: a
wrong seed advances to a different stack, hashes to a different value, and fails against the
recorded one. Only the true predecessor state survives, up to hash collision.

Recording only the *after* state (not a `before`/`after` pair) is what makes this work with one
32-byte field: the seed is validated by reproducing the step's own recorded commitment, not by
matching a predecessor record the window does not contain.

`advance` is fed replay-proven facts (`RecurTileReplay`'s `iteration_index`,
`consumed_elements`, `control` — `lazy-list-recur` §5) and proof-authenticated coverage
(`ListRange.start`/`len`, or `Index(i)` + `List.len`). It returns
`Result<_, RecurProgressViolation>` in `raster-core`; the guest panics at the call site. Same
shape as `ChunkViolation` today.

One cheap extra assert: each frame's `site` must be a prefix of the step's coordinates, in
stack order, innermost matching. The commitment already pins the preimage, so this catches
recorder bugs rather than forgeries — but it makes a wrong-site stack fail loudly instead of
arithmetically.

### 5. Why the record and not the replay journal

A journal field would be strictly stronger *if the tile could see it*. It cannot: the tile
would have to receive the previous iteration's commitment through `RecurInput`, changing the
recur ABI, every recur tile's `input_commitment`, and putting an audit field in a user-facing
type — to carry a value the tile can only echo back. Nothing is gained, because the facts being
accumulated are **already** replay-proven; only the accumulation needs a carrier, and every
link of it is re-checked against those facts inside the window that contains the step.

### 6. What this is worth, precisely

The chain is verified link-by-link in every window, and at a window boundary the seed is
validated by reproducing the recorded commitment. What ultimately constrains that recorded
value is fingerprint agreement with `commit.bin`, at `bits_per_item` strength per item — with
the window's first item not itself compared (`finalize`'s `StepPosition::First`) but linked
forward into item 1, which is.

So recur progress ends up bound **exactly as strongly as `storage.root_before` and every other
per-step root**. The claim is not that it becomes unforgeable; it is that it stops sitting
outside the security argument and joins it. Today it would be reachable by `env::read()` and
compared against nothing at all.

### 7. The same gap, twice more

`active_drafts` (shipped) and `loop-carried-state`'s proposed `TrackedStateRoot` are the same
shape with the same `Init` hole. Drafts are partly protected — `root_before` is replay-proven,
so a forged map entry cannot invent a root out of nothing — but the *continuity* claim across a
window boundary rests on the same unchecked map, and the `if let Some(...)` means an absent
entry is not even a failure.

Folding all three under one channel is [`carried-state-channel.md`](./carried-state-channel.md),
and it should happen **after** this lands, not with it — see §Uncertainty 1 for why waiting is
free, and that proposal's §4 for the question drafts must answer first (`create_draft` emits no
trace event, so an absent entry is legitimate today).

## Dependency on `lazy-list-recur`

Strictly complementary, and neither is useful alone:

| | defines |
| --- | --- |
| `lazy-list-recur` §5 | the **facts** (`RecurTileReplay`, `RecurPosition`, `RecurControlKind`, replay-proven) and the **rules** (1–8, S1–S5) |
| this proposal | the **carrier** those rules accumulate in, and what makes it checkable when a window does not contain the whole loop |

Without §5 there are no authenticated facts to advance a frame with. Without this, §5's rules
constrain only a window that happens to contain iteration 0 — and the prover picks the window,
so in practice they constrain nothing. **They land in the same change**, as `lazy-list-recur`
phase 5. This proposal does not extend that proposal's scope: it replaces the
`InitTransition.active_recur_sweeps` field that phase would otherwise have added.

`paged-bytes` inherits the dependency through §5: "this program processed the whole artifact"
is exactly the claim these rules underwrite.

## Modules touched

| file | change | blast radius |
| --- | --- | --- |
| `raster-core/src/recur_progress.rs` | new: `RecurProgressFrame`, `RecurProgressStack`, `commitment()`, `advance_iteration`, `close_site`, `RecurProgressViolation` | additive |
| `raster-core/src/trace.rs` | `recur_progress_commitment: Hash32` on `StepRecord` (`:404`) | **trace encoding — fingerprints move** |
| `raster-core/src/transition.rs` | `recur_progress` on `Transition` (`:209`); `window_start_recur_progress` on `TransitionInput` (`:167`). `InitTransition` unchanged | journal encoding — image ids move |
| `raster-runtime/src/tracing/recorder.rs` | stamp `recur_progress_commitment` on every step; drive it from the existing `RecurExecutionState` (`:730`) | moderate |
| `guests/transition/src/checks/cfs.rs` | the per-step advance/compare, and the completeness rules it enables | additive |
| `guests/transition/src/fraud_proof.rs` | carry `recur_progress` through `LiveTransition` / `into_transition` | small |
| `raster-prover/src/transition.rs`, `raster-cli/src/commands/run.rs` | pass the seed and the carried state through window assembly | small |

**Not** changed: `InitTransition`, the recur ABI, `RecurInput`, the replay journal's shape,
`.rindex`, the selector or proof-step enums.

## Verification

- **The attack, as a test:** a fresh `Init` at a recur site step with a seed claiming nine
  completed iterations is rejected, because advancing it does not reproduce the step's recorded
  commitment. This is the test the whole proposal exists for.
- A window opening at iteration 900 of a 1000-iteration loop verifies with an unchanged
  128-item window — the regression test for the rejected "refuse to open mid-loop" design.
- Recorder/guest agreement: the commitment the native recorder stamps equals the one the guest
  computes, over a fixture exercising element, chunked, and recur-sequence sites. A divergence
  between the two implementations is the failure mode a shared `raster-core` function exists to
  prevent, so it is asserted directly rather than implied.
- Every field is load-bearing, one test each: mutating `chunk`, `source_len`,
  `next_iteration_index`, `consumed_total` or `last_control` in the seed must fail the compare.
- Nesting: a `call_recur!` inside a recur-sequence iteration pushes and pops a second frame; a
  window opening inside the inner loop seeds both frames and verifies; a seed naming only the
  inner frame fails.
- The canonical empty stack: a window opening at an ordinary tile with `None` verifies against
  the empty-stack commitment, and a seed claiming a live frame where the record says empty
  fails.
- Coordinate consistency: a seed whose frames are not prefixes of the first step's coordinates
  is rejected.
- `Next` continuity is unaffected: an existing multi-step window carries the preimage forward
  with no seed supplied.

## Performance

+32 bytes per step record, +1 SHA-256 per step in the guest, +1 `postcard` encode of a small
struct. `StepRecord` already carries four 32-byte roots (`StorageRoots`), so this is
proportionate. No extra receipt verification, and — the whole point — no change to window size.

## Uncertainties for review

1. ~~Should the field be a general `carried_state_commitment` from the start?~~ **Resolved: no,
   keep the specific name.** The argument for wrapping was that it would save a trace-format
   break when drafts and loop-carried state join. It does not: the commitment is a hash over a
   struct, so adding a component changes the encoded bytes — even where that component is empty
   — hence every step's commitment, hence every fingerprint. Adding a component later costs
   exactly what adding a second `StepRecord` field costs. With the cost argument void, what
   remains is coordination, which a cross-reference handles: see
   [`carried-state-channel.md`](./carried-state-channel.md), to be done when a **second**
   component is actually ready. Naming a one-member category now would fix a shape for state
   whose requirements are not yet designed.
2. **Whether `consumed_total` stays.** It is derivable from `next_iteration_index`, `chunk` and
   `source_len` once rule 4 holds. Keeping it is a robustness and readability call, not a
   soundness one.
3. **Whether the empty-stack commitment should be a compile-time constant** (as
   `EMPTY_LEAF` is in `merkle_tree.rs:27`) rather than computed. Cheap either way; a constant
   makes the "absence is a claim" property visible at the definition site.
4. **Whether `close_site` should also be replay-proven.** The site step carries no replay proof
   (`expected_tile_image_id` matches only `ExecTarget::Tile`), so the terminal rules read
   host-recorded structure plus the authenticated metadata length. That is the same standing as
   every other non-tile step, but it is the weakest link in the chain and should be stated in
   any release note rather than discovered later.
