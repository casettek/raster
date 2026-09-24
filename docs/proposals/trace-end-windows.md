# Proposal: `trace-end-windows` — make both ends of a trace provable

Status: **implemented** (2026-09-16)

Closes [`fraud-window-trace-ends`](../issues/fraud-window-trace-ends.md) §2, §3 and the head/tail
halves of §5. The issue's other reproduction — `witness_record_inputs` panicking on top-level
steps — and the soundness defects found while fixing this one are owned by
[`trace-leaf-field-binding`](./trace-leaf-field-binding.md).

Related:

- [`chain-fraud-proof.md`](./chain-fraud-proof.md) §Security notes (l.343-351) — **owns the margin
  statement**, and said it was "unchanged by this proposal". Still true of *that* proposal; this
  one changes it. The margin is no longer the only thing pinning a window's opening state, and at
  the head it is no longer needed at all.
- [`chain-io-commitment.md`](./chain-io-commitment.md) §3 — assumes throughout that a detected
  divergence yields a constructible proof. That assumption held everywhere except the two ends;
  it now holds everywhere.
- [`window-seed-reconstruction.md`](./window-seed-reconstruction.md) — the other case of a window
  that cannot stand alone. That one is about *state* a window lacks; this is about *length*.

## Problem

The scheme's security parameter is a **window**: `FRAUD_DETECTION_SECURITY_BITS = 128`, spent as
`bits_per_item = ceil(128 / window_size)`. Every statement of the guarantee is per-window, and a
trace has two places where a full window does not exist. They failed differently.

**At the head, detection worked and the proof did not.** A divergence at index `i < window_size`
produced a window of `min(i + 1, window_size)` items — correct — paired with a fingerprint slice
computed as:

```rust
index.saturating_sub(self.window_size) + 1  ..  index + 1
```

Below `window_size` the `saturating_sub` floors to 0 and the `+ 1` lands on **1**: one entry too
few, *and* starting one item late, so window item 0 was compared against committed item 1. At
`index == 0` the range was `1..1` and the slice came back empty. The declared length was the
constant `window_size` regardless, and `Fingerprint::from` stores `len` verbatim, so the
mismatch travelled to the guest, which read the declared length as authoritative and ran off the
end of the bits.

**At the tail, the proof was well-formed and detection degraded.** A divergence at index `N - k`
has only `k` indices of forward runway, so it was compared against `k * bits_per_item` bits
rather than 128. At `window_size >= 128`, `bits_per_item` is 1 and a last-step divergence was a
coin flip.

Underneath both sat a third problem the issue did not name: **nothing bound a window's shape to
the commitment at all.** The guest never learned `window_size`, so `window_len` and
`window_start` were challenger-supplied with only `window_start + window_len <= fingerprint_len`
constraining them. That is an upper bound, not a shape — and a two-item window has *zero*
matching comparisons, so nothing tied the challenger's opening state to reality. Fixing the head
without fixing this would have made short windows legitimate output and removed the last informal
signal that something was wrong.

## Design

### The tail: reveal the roots, stop squeezing

`TraceCommitment` gains `revealed_tail_roots` — the cumulative trace roots for the final
`window_size` indices, in the clear. The tail's counterpart to `revealed_items` revealing the
first `window_size` step records.

Detection in that range compares full 256-bit roots instead of packed entries, so it is exact and
also pinpoints the divergence index. The header gains
`revealed_tail_roots_commitment = sha256(postcard(roots))`, keeping the header constant-size.

`validate()` holds each revealed root to the fingerprint entry it squeezes to, via a
`fingerprint_value` helper extracted from `FingerprintAccumulator::append` — the same function
that *produced* those entries. A commitment whose roots and bits disagree is unrepresentable
rather than merely detectable.

The guest half matters as much as the host half: `finalize` accepts divergence proven by
fingerprint entry **or** by the guest's computed root differing from the revealed one. Without it
a challenger would see a tail fraud with certainty and be unable to produce a receipt. Root
equality implies entry equality, so this widens what is *provable* and never what is accepted as
a match.

### The head: clamp, and assert genesis instead of a margin

The slice becomes `(index + 1).saturating_sub(window_size)` — identical for `index >= w`, and 0
at the head, which is where the window genuinely starts. The declared length becomes the
*measured* item count via a new `Window::len()`.

That makes short windows well-formed, which raises the question the issue's §5 direction 1 names:
a short window has less margin than a full one, and the margin is what pins the challenger's
opening state.

**It needs no margin, because at index 0 the opening state is not the challenger's to choose.**
The trace tree holds only the seed — the same public constant for every program — storage is
empty, and no draft or loop is in flight. `assert_opens_at_genesis` checks exactly that. This is
not tolerance of a missing margin; it removes the need for one.

The shape rule the guest enforces is therefore:

```
window_len == window_size
  OR (window_start == 0 AND window_len < window_size AND opening state is genesis)
```

which is precisely what an honest host can emit: `try_build` requires `trace.len() > window_size`
and the rolling buffer yields `min(i + 1, w)` items starting at `max(0, i + 1 - w)`, so a short
window always starts at 0.

### The `First` exemption, removed

The window-opening step was committed without any fingerprint comparison. That cost an item of
margin on every window, and made a one-item window unprovable *in principle* — its only item was
the one never examined. A divergence in the trace's very first step was detectable and could
never be proven.

The rule is now uniform: every item before the window's last must match, the last must diverge.
`StepPosition` had no readers left and was removed rather than kept as a distinction that no
longer exists.

## Soundness

A one-item genesis window is the sharpest case, and it is worth writing out what pins it, since
"no margin" sounds alarming:

| what | pinned by |
|---|---|
| opening machine state | `assert_opens_at_genesis` — seed leaf, empty storage, no drafts |
| which program, which inputs | `ProgramStart.output_commitment == combined_root(CFS names, authorization journal)` |
| the record's structural fields | `verify_exec_index`, `verify_sequence_id` — see `trace-leaf-field-binding` |
| the window's shape | `window_len == window_size`, relaxed only at genesis |

The seed is **program-independent**, so genesis alone says nothing about what ran. The second row
is what supplies that, and following it back terminates at the input manifest — see
`raster-prover::availability`, which names that assumption rather than leaving it implicit. A
one-item head window is exactly as trustworthy as the manifest behind it.

The third row is why this proposal could not land first. Without `exec_index` and `sequence_id`
pinned, a one-item genesis window is forgeable by changing a field nothing verifies.

## Format break

`TRACE_COMMITMENT_DOMAIN` moves `v1 -> v2`. Existing `commit.bin` files and any receipt carrying
a v1 `refuted_trace_commitment` stop matching. `TraceCommitmentHeader` gains `window_size` and
`revealed_tail_roots_commitment`; `TraceCommitment` gains `revealed_tail_roots`;
`TransitionJournal` gains `final_committed_root`; `TransitionInput` gains `revealed_tail_roots`.
The guest image id moves.

## What this settles about `window_size`

The issue closes on: **is `window_size` one parameter or two?** It stays one.

The question arose because the number set both the proof's length and the detector's per-item
strength, and those stopped being compatible at the ends. Revealing the tail roots removes the
conflict rather than splitting the parameter: detection in the final window no longer reads the
fingerprint at all, so per-item strength stops mattering exactly where it used to be weakest.
Sizing the window is now a proof-size choice. The CLI help says so.

## Modules touched

- `raster-core/src/trace.rs` — `TraceCommitment.revealed_tail_roots`
- `raster-core/src/transition.rs` — header fields, domain bump, journal and input fields
- `raster-core/src/fingerprint.rs` — `fingerprint_value` extracted
- `raster-prover/src/trace.rs` — `build`, `validate`, `verify`, the clamp, `Window::len`
- `raster-prover/src/transition.rs`, `raster-cli/src/commands/run.rs` — threading the roots
- `guests/transition/src/fraud_proof.rs` — shape rule, `assert_opens_at_genesis`, `WindowBinding`,
  `finalize`
- `raster-cli/src/main.rs` — the help text that claimed 128-bit detection unconditionally

## Verification

Host: `fraud_window_geometry_agrees_across_the_head_boundary` sweeps divergences
`0 ..= window_size + 1` and asserts all three channels agree — including that window item 0 is the
committed entry at the frontier's position, because a length-only assertion passes on a slice
that is short *and* shifted, which is what the old expression produced.

`a_final_step_divergence_is_detected_when_its_fingerprint_bit_collides` *searches* for a tamper
whose 1-bit entry collides with the honest one at `window_size = 128`. Asserting detection of an
arbitrary tamper would have passed under the old code too; only a collision isolates what the
revealed roots buy.

Guest: short-window-at-genesis accepted, one-item genesis window accepted, and two refusals
proving position 0 alone is not enough (dirty storage, live draft); tail-root binding accepted,
absent outside the tail, and refused when fabricated.

## Outstanding

- The end-to-end run against `raster-inference`'s `prompt-prepare` (1,997 steps) described in the
  issue's §6 has not been done since the format break. Regenerating a commitment and auditing it
  would exercise the `v2` format on a real trace rather than on fixtures.
- The multi-step `Next` chain has no end-to-end coverage. Pre-existing — the one-step fixture now
  ends in `Finished` rather than `Next`, so it never exercised `assert_state_continuity` or
  `verify_previous_journal` even before this change.
- `revealed_tail_roots` costs `window_size * 32` bytes in the commitment and, when a tail
  divergence is proven, in the window-opening step's input. 64 bytes at the `chain run` default
  of 2; 32 KiB at the maximum window of 1024. Not measured against real proofs.
