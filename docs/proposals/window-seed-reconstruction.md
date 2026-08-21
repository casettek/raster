# Proposal: `window-seed-reconstruction` — give a fraud-proof window the carried state it opens with

Status: proposed 2026-08-14

Related:
- [`recur-progress-commitment.md`](./recur-progress-commitment.md) — **the immediate consumer.**
  Its `TransitionInput.window_start_recur_progress` exists and is threaded; nothing ever fills it.
  This proposal fills it.
- [`chain-fraud-proof.md`](./chain-fraud-proof.md) — the window model. Window size is the proving
  budget, and this is what keeps it a constant rather than a function of the data.
- [`carried-state-channel.md`](./carried-state-channel.md) — the same reconstruction serves
  `active_drafts` and `loop-carried-state`'s `TrackedStateRoot` when they fold in (§5).

## Problem

`recur-progress-commitment` revision 2 is implemented. Every step records the commitment of the
recur-progress stack **after** it, and the guest validates by advancing the carried state and
comparing. For a `Next` step the carried state arrives from the previous journal. For a window's
**first** step it arrives in `TransitionInput.window_start_recur_progress`.

That field is plumbed end to end and **is always `None`** (`raster-prover/src/transition.rs`). So a
window whose first step sits inside a live loop advances the *empty* stack, produces a different
commitment from the one the recorder stamped, and is rejected.

### This is the design the proposal explicitly rejected, reinstated by omission

`recur-progress-commitment` §Problem lists "refuse to open mid-loop" among the fixes that **do not
work**:

> Sound, and unacceptable: refuting a divergence at iteration 900 of a 1000-iteration loop forces
> the window to open at iteration 0, turning a 128-item window into a 900+-item one. Window size is
> the proving budget; a rule that makes it data-dependent is not a rule anyone can operate.

That is the behaviour shipped today. Not by choice — by an unfilled parameter.

It is in one respect **worse** than the rejected design. The rejected version panics with a clear
"cannot open mid-loop". The current one fails with a recur-progress commitment mismatch, which
reads like a soundness violation in the trace rather than a missing host input, and points the
reader at the guest instead of at `build_transition_input`.

### What it does not do is compromise soundness

The failure is closed. A window opening outside any loop starts from the empty stack — which is
the true state — and verifies normally. A window opening inside a loop is *rejected*, never
wrongly accepted. So this is a **completeness** gap, and the practical cost is that fraud proofs
over long sweeps are either impossible or must open at iteration 0, which is exactly the proving
budget the window model exists to bound.

## Design

### 1. Reconstruct from the trace prefix, where the store already does

`raster-cli`'s `prove()` already faces this problem for storage and solves it:

```rust
// raster-cli/src/commands/run.rs:774
let mut current_storage_state =
    storage_state_from_prefix(&trace[..window_start_index], trace_recorder);
```

One walk of the prefix, **once per fraud proof**, before the window loop. Recur progress is the
same shape of state and wants the same treatment on the same walk, so the marginal cost is close to
zero.

### 2. The recorder is the right source, not a second re-derivation

The reconstruction must produce the stack the *recorder* had at that step, and the recorder already
computes it: `push_site` at a site `Start`, `advance_*_iteration` per iteration, `close_site` at
`End` (`raster-runtime/src/tracing/recorder.rs`).

Re-deriving that walk in `raster-cli` would be a second implementation of the same rules. That is
the drift risk `recur-progress-commitment` §4 already argues against for the recorder-versus-guest
split, and the reason both call the shared `raster-core` function. The same argument applies here
with more force, because a divergence would produce a *wrong seed*, which fails as a commitment
mismatch — indistinguishable from the bug this proposal is fixing.

**So: the recorder retains its stack per step and exposes it.**

```rust
// raster-runtime/src/tracing/recorder.rs
impl TraceRecorder {
    /// The recur-progress stack as it stood **after** the step at these
    /// coordinates — the state the *next* step's guest must start from.
    pub fn recur_progress_after(&self, coordinates: &CfsCoordinates)
        -> Option<RecurProgressStack>;
}
```

This mirrors `step_witness_at` (`:325`), which retains per-step witness data for exactly this kind
of after-the-fact host query.

### 3. The seed is supplied only at the window's first step

`build_transition_input` already takes `entrypoint_membership_witness` under a
`current_journal.is_none()` guard — "only the `Init` step needs this". The seed follows that
established shape: `Some` on the first step, `None` after, because every later step inherits the
preimage through `Transition`.

### 4. Why a wrong reconstruction cannot become a soundness hole

The seed is **never believed**. The guest advances it by the step's own facts and compares against
the step's recorded `recur_progress_commitment`; a wrong seed advances to a different stack and
fails. That property is already implemented and is what makes host-side reconstruction acceptable
here at all.

It is worth stating the contrast: `storage_state_from_prefix` reconstructs state that is *partly*
trusted — `init_storage_root` is bound only through the first step's `storage.root_before`
assertion. The recur-progress seed is bound strictly: nothing about it is taken on faith.

So reconstruction quality is a **liveness** concern, not a security one. Getting it wrong means
honest proofs fail, which is the failure we already have.

## Soundness

- **No new trust.** The seed enters the guest and is immediately validated against a
  fingerprint-pinned commitment. Nothing downstream reads it before that check.
- **`InitTransition` still gains nothing**, preserving the property
  `recur-progress-commitment` §3 was built around.
- **The empty-stack case stays a positive claim.** A window opening outside a loop supplies the
  canonical empty stack; a prover claiming a live frame where the record says empty still fails.

## Modules touched

| file | change | blast radius |
| --- | --- | --- |
| `raster-runtime/src/tracing/recorder.rs` | retain the stack per step; `recur_progress_after` | small — additive, mirrors `step_witness_at` |
| `raster-cli/src/commands/run.rs` | read the seed at `window_start_index` and pass it on the first step only | small |
| `raster-prover/src/transition.rs` | pass the supplied seed through instead of the hard-coded `None` | one line |

**Not** changed: `InitTransition`, the trace format, the journal, the CFS, the guest — the guest
side is already implemented and correct; it is only ever handed `None` today.

## Verification

- **The case the whole thing exists for:** a window opening at iteration 900 of a 1000-iteration
  loop verifies, with an unchanged window size. This is `recur-progress-commitment`'s own stated
  regression test for the rejected "refuse to open mid-loop" design, and it currently fails.
- **The attack still fails:** a *forged* seed at that same window — one claiming a different
  `next_iteration_index` — is rejected, because advancing it does not reproduce the recorded
  commitment. Reconstruction must not weaken this; the test pins that it does not.
- **Nesting:** a window opening inside a `call_recur!` nested in a recur-sequence iteration seeds
  **both** frames and verifies; a seed naming only the inner frame fails.
- **Windows outside a loop are unaffected:** the existing `hello-tiles` `--commit`/`--audit` path
  keeps passing, seeded with the empty stack.
- **Recorder/reconstruction agreement**, asserted directly rather than inferred: for every step of
  a fixture with element, chunked and recur-sequence sites, `recur_progress_after(step)` hashes to
  that step's recorded `recur_progress_commitment`. A divergence here is precisely a wrong seed, so
  it is checked at the source rather than only through a proof that fails for unclear reasons.

## Performance

One `RecurProgressStack` clone per step retained in the recorder. A frame is a `CfsCoordinates`
plus five scalars, and stack depth is loop-*nesting* depth — 1 in almost every program, 2 for a
`call_recur!` inside a recur sequence. At ~64 bytes per frame that is a few megabytes for a
100 000-step trace, against a trace that is already far larger, and it lives only in the
`--commit`/`--audit` host process.

No change to window size, proving time, receipt count, or the trace itself. The prefix walk is the
one `storage_state_from_prefix` already performs.

## Uncertainties for review

1. **Retain per step, or reconstruct on demand?** Retaining is simplest and drift-free. Within a
   loop the frame is *almost* derivable from the site's frame plus the iteration coordinate — but
   not quite: `last_control` cannot be derived, which is the same field that forced a trace bit in
   `recur-progress-commitment` §3.1. Deriving would therefore need the journals anyway, so
   retaining is recommended; the memory figure above is what a reviewer should push back on if it
   is wrong for their trace sizes.
2. **Should this generalize now or later?** `active_drafts` has the identical unfilled-seed shape
   (`recur-progress-commitment` §7), and `carried-state-channel.md` proposes one channel for all
   carried state. Reconstructing them together is the obvious end state. Doing it now would widen
   this from a one-line fix plus an accessor into a refactor of a mechanism that has just changed
   twice; the recommendation is to fix recur progress and let the channel proposal absorb the
   pattern when a second component is actually ready.
3. **Should the guest detect a mid-loop open and say so?** Today a missing seed surfaces as a
   commitment mismatch. Even with this fixed, a *host bug* would present the same way. A cheap
   assert — the first step's coordinates imply a live site, but the seed is empty — would turn that
   into a message naming the cause. Diagnostics only, no soundness weight.
