# Issue: `fraud-window-trace-ends` — the window the 128-bit argument assumes does not exist at either end of a trace

Status: open 2026-09-11. Unowned.

Reproduced against `feature/recur-mid-seed` at `63980a9`. Every citation is committed code.

The scheme's security parameter is a *window*: `FRAUD_DETECTION_SECURITY_BITS = 128`
(`raster-prover/src/trace.rs:129`), spent as `bits_per_item = ceil(128 / window_size)`
(`:157-176`). Every statement of the guarantee is per-window. A trace has two places where a
full window does not exist — its first `window_size` steps and its last `window_size` steps — and
the two fail differently: **at the head, detection is full strength and the proof window handed
to the guest is malformed; at the tail, the proof window is well-formed and detection degrades to
`bits_per_item`.** Neither end is named by any proposal.

Related:

- [`chain-fraud-proof.md`](../proposals/chain-fraud-proof.md) §Security notes (l.343-351) —
  **owns the margin statement, and its scope stops exactly here.** It states the residual
  soundness margin is "matching the committed fingerprint bits across the *pre-divergence
  window* (`FRAUD_DETECTION_SECURITY_BITS` = 128 revealed bits per window) under deterministic
  execution — unchanged by this proposal." It is right that §2's slice check does not change the
  margin. It does not say what the margin is when the pre-divergence window is *shorter than
  `window_size`*, which is every divergence in the trace's first `window_size` steps, nor what
  the forward margin is when fewer than `window_size` steps follow the divergence.
- [`chain-io-commitment.md`](../proposals/chain-io-commitment.md) §3 — owns the dispute path in
  which a contested stage's `commit.bin` is the artifact a fraud proof is built from. It
  specifies what the re-run must produce and who bears the cost. It assumes throughout that a
  detected divergence yields a constructible proof; §2 below is a class of divergence where it
  does not.
- [`program-chain.md`](../proposals/program-chain.md) — owns layer-0 link checks, which verify
  each stage's `output.bin` commitment publicly and independently of the fingerprint. That
  narrows §3's exposure for chain runs (a tail divergence that moves the published output is
  caught as a link fault) without removing it, and does not apply to
  `cargo raster run --audit` on a single program.
- [`window-seed-reconstruction.md`](../proposals/window-seed-reconstruction.md) — owns the other
  case of a window that cannot stand alone: one opening inside a live recur site. That is about
  *state* a window lacks; this is about *length* a window lacks. `assert_seed_present_for_mid_loop_open`
  (`fraud_proof.rs:118-133`) is the precedent for refusing a window whose shape the machine
  cannot honour.

## 1. Two windows, built by different arithmetic

They are easy to conflate because both are called "the window". They point in opposite
directions.

**Detection looks forward.** `TraceVerifier::verify` (`raster-prover/src/trace.rs:852`) compares
one fingerprint index per step and returns at the first mismatch (`:872-878`) — it never waits
for a window to fill. The 128 bits are an *accumulation*: the trace tree is cumulative, so a
divergence at step `i` makes every root from `i` onward differ, and each subsequent index
contributes another `bits_per_item` bits of evidence. Spending the full budget takes
`window_size` steps **after** the divergence.

**The proof looks backward.** In the guest, `finalize` (`fraud_proof.rs:630-654`):

```rust
StepPosition::First => TransitionState::Next(self.into_transition()),
StepPosition::Subsequent => {
    let diverges = actual_fingerprint.diff_at_index(last_index, committed_fingerprint);
    if actual_fingerprint.len() == committed_fingerprint.len() {
        assert!(diverges);  TransitionState::Finished
    } else {
        assert!(!diverges); ... TransitionState::Next(...)
    }
}
```

Three consequences, all load-bearing:

1. The `First` step is "committed without a fingerprint comparison" (`:96-101`) and always
   returns `Next`. A one-item window can never reach `Finished`.
2. `Finished` requires the window be **fully consumed** — `actual.len() == committed.len()`.
3. Every intermediate step must *match* (`assert!(!diverges)`). This is the soundness mechanism:
   the challenger supplies the window's opening state, and the only thing pinning that state to
   reality is that replaying forward from it reproduces the committed fingerprint. The
   pre-divergence window **is** the margin §Security notes names.

So a fraud proof needs exactly `window_size` items with at least one `Subsequent` step among
them. That requirement is not stated anywhere; it is implied by (2).

## 2. The head — detection succeeds, the window it produces cannot be proven

At a divergence at index `i`, the host builds the window from a rolling buffer pre-filled with
`None` and flattened on read (`Window`, `trace.rs:504-528`), so it holds `min(i + 1,
window_size)` items. It takes the committed slice as:

```rust
.get_range(index.saturating_sub(self.window_size) + 1, index + 1, ...)   // trace.rs:884
```

That expression yields `window_size` entries only for `index >= window_size`. Below it, the item
count and the entry count disagree, and the declared length agrees with neither —
`Fingerprint::from(diff_bits, packer, self.window_size)` (`trace.rs:890`) stores `len` verbatim
with no validation (`raster-core/src/fingerprint.rs:518-524`):

| divergence index (`window_size` = 32) | window items | slice entries | declared `len` |
| --- | --- | --- | --- |
| 0 | 1 | 0 | 32 |
| 5 | 6 | 5 | 32 |
| 31 | 32 | 31 | 32 |
| 32 | 32 | 32 | 32 |
| 40 | 32 | 32 | 32 |

The off-by-one persists to `index == window_size - 1` — the first index at which the buffer is
full — and resolves only at `index >= window_size`.

The guest then reads the *declared* length. `assert_window_is_commitment_slice`
(`fraud_proof.rs:287`) sets `window_len = window_fingerprint.len()`, so
`assert!(window_len > 0, "Window fingerprint is empty")` (`:299`) passes on the declared 32 even
when `bits` is empty, and the comparison loop (`:341-355`) calls

```rust
.try_get(item, &window_fingerprint.bits)
.expect("Window fingerprint is shorter than its declared length")
```

for `item` in `0..32`. A head window's `bits` is shorter than that by construction.

The contrast inside the same type is the sharp part. `terminal_window` (`trace.rs:943`) — the
non-fraud path that builds the *last* window — computes `window_start = trace.len() -
window_size` and `get_range(window_start, last_index + 1)`, which is exactly `window_size`
entries, and refuses a trace shorter than the window up front (`:945-951`). Its doc comment says
the two windows "are constructed the same way and can be fed to the same prover" (`:938-942`).
They are not constructed the same way: one clamps and the other saturates.

### What currently preempts this in practice

On `raster-inference`'s `prompt-prepare` (1,997 trace steps, 125 packed blocks), a head
divergence does not reach the guest — it panics earlier, in evidence construction:

```
thread 'main' panicked at crates/raster-prover/src/trace.rs:627:
Failed to resolve active sequence invocation for step StepRecord { exec_index: 2,
  sequence_id: "merge_round", coordinates: CfsCoordinates([0]), kind: SequenceStart { .. } }
  in frame CfsCoordinates([])
```

`witness_record_inputs` (`trace.rs:736`) `rposition`s the trace for a `SequenceStart` whose
coordinates equal the parent frame (`:620-630`). For a top-level sequence the parent frame is
`[]`, which is opened by `ProgramStart`, not `SequenceStart` (`raster-core/src/trace.rs:410-411`),
so nothing is found. **This is a distinct defect** — it is keyed on step *kind*, not window
position, and would fire for a divergence flagged at any top-level `SequenceStart` — but it is
what makes the head case crash rather than produce a malformed window, so the two are entangled
in any reproduction and §5 lists it.

## 3. The tail — the proof window is fine, detection is not

A divergence at index `N - k` has only `k` indices of forward runway, so it is compared against
`k * bits_per_item` bits, not 128. At `k = 1` — a trace whose only divergence is its last step —
the whole guarantee is one item's worth:

| `--fraud-proof-window-size` | `bits_per_item` | tail-only divergence survives audit with prob. |
| --- | --- | --- |
| 2 (the `chain run` default) | 64 | 2⁻⁶⁴ |
| 32 (the single-program example in `raster-inference/README.md:408`) | 4 | 2⁻⁴ |
| 128 | 1 | **2⁻¹** |
| 1024 | 1 | **2⁻¹** |

The degradation is graded across the final `window_size` steps, from 128 bits down to
`bits_per_item`. Nothing in the CLI says so: `--fraud-proof-window-size`'s help text
(`raster-cli/src/main.rs:200`) explains that bits per item are derived to reach 128-bit
detection, which is true per window and not true for the trace's last window. A larger window is
presented as a pure size trade; it is also a reduction in per-item strength exactly where
per-item strength is all there is.

`validate()` enforces `fingerprint.len() > window_size` (`trace.rs:333-341`) and `try_build`
refuses a trace no longer than the window (`:294-302`), so a trace always has at least one full
window somewhere. Neither check says anything about where in the trace a divergence may fall.

What narrows this, and does not close it: at the chain layer a stage's `output.bin` commitment is
checked publicly by the link layer, so a tail divergence that moves the published output is a
`Link` fault regardless of fingerprint luck. That leaves the exposure at (a)
`cargo raster run --audit` on a single program, where there is no link layer, and (b) anything in
the final steps not reflected in the output commitment — `ProgramEnd`'s
`recur_progress_commitment` and `recur_state` (`raster-core/src/trace.rs:440-456`) among them.

## 4. What this is not

- **Not a soundness hole.** Nothing here lets a receipt exist against an honest prover. A
  degenerate head window fails closed — it panics or trips `assert!(!diverges)`; it does not mint
  a `Finished`. §3 is a completeness gap, not a soundness one.
- **Not the `revealed_items` gap.** That field is `trace[..window_size]` and is read only by
  `.len()` and by `sha256` into `TraceCommitmentHeader` (`trace.rs:259`, `:430-435`); it is
  unrelated to either window's arithmetic. A separate observation, not part of this issue.
- **Not closed by §2's slice check.** `assert_window_is_commitment_slice` closed
  `chain-fraud-proof.md`'s problem 3 — a challenger proving divergence against a fabricated
  fingerprint. It binds the window to the commitment *given a well-formed window*. It does not
  establish that the host produces one.
- **Not reachable through `terminal_window`.** That path clamps correctly and expects no
  divergence; it builds the terminal-window receipt, not a fraud receipt.
- **Not an argument for a particular window size.** §3's table is a property of the 128-bit
  budget being spread, not of any one setting. The default of 2 for `chain run` happens to sit at
  the safe end of it; the README's single-program example at 32 does not, and nothing connects
  the two numbers to this consequence.

## 5. Directions, none chosen

1. **Clamp the fraud slice the way `terminal_window` does.** Make `verify`'s `get_range` produce
   exactly as many entries as the buffer holds and declare that length, rather than saturating
   and declaring `window_size`. Costs: a head window then legitimately has fewer than
   `window_size` items, so `finalize`'s `actual.len() == committed.len()` becomes reachable with a
   short window — which means deciding whether a short pre-divergence window is an acceptable
   margin, i.e. it does not avoid the question in §2, it relocates it to the guest.
2. **Refuse to open a window before index `window_size`**, on the precedent of
   `assert_seed_present_for_mid_loop_open` (`fraud_proof.rs:118`). Costs: a genuine divergence in
   the trace's first `window_size` steps is then detected and unprovable *by rule* rather than by
   accident — in an optimistic protocol where silence is acceptance, that settles identically to
   undetected, and `ProgramStart` and the entrypoint authorization live in exactly that prefix.
3. **Pad the trace tree** so every real step has a full window behind and ahead of it — e.g.
   `window_size` synthetic leaves at each end, committed but not replayed. Costs: synthetic
   leaves are trace items nothing authorizes, which is the shape §2 of `chain-fraud-proof.md`
   spent its effort removing; and `fingerprint_len` stops meaning "steps".
4. **Decouple the two budgets**: keep `window_size` for the proof and give detection its own
   per-item floor, so `bits_per_item` never drops below a stated minimum regardless of window.
   Costs: the fingerprint grows for large windows, which is the size the window parameter exists
   to control; `MIN_BITS_PER_ITEM = 1` (`trace.rs:133`) becomes a security parameter rather than
   a packing limit, and `validate()`'s block-count arithmetic has to keep agreeing with it.
5. **Say it in the CLI and the docs, change nothing.** Document that the guarantee is per-window,
   that the final window degrades to `bits_per_item`, and that window size is therefore a
   security choice. Costs: leaves §2 unaddressed entirely — a head divergence remains a crash.

Directions 2 and 4 answer different ends and are not exclusive. All five turn on a question no
proposal has had to ask: **is `window_size` one parameter or two?** Today it sets the proof's
length and the detector's per-item strength from a single number, and the two ends of a trace are
where those two roles stop being compatible.

The `witness_record_inputs` panic in §2 is a separate defect and should be filed or fixed
separately; it is recorded here only because it preempts every head-divergence reproduction.

## 6. Reproducing

The two windows and their different arithmetic:

```bash
sed -n '852,900p'  crates/raster-prover/src/trace.rs   # verify: saturating_sub slice
sed -n '925,975p'  crates/raster-prover/src/trace.rs   # terminal_window: clamped slice
sed -n '504,529p'  crates/raster-prover/src/trace.rs   # the rolling Window buffer
sed -n '630,654p'  crates/raster-prover/guests/transition/src/fraud_proof.rs   # finalize
sed -n '287,360p'  crates/raster-prover/guests/transition/src/fraud_proof.rs   # slice check
sed -n '127,177p'  crates/raster-prover/src/trace.rs   # the 128-bit budget, split
```

The head case end to end, in `raster-inference`'s `prompt-prepare` (trace = 1,997 steps):

```bash
cd prompt-prepare
cargo raster run --input input.json --input-manifest input_manifest.json \
  --commit /tmp/c.bin --fraud-proof-window-size 32

# flip one fingerprint bit near the head, preserving postcard varint framing.
# byte 0 is bits_packer and byte 1 the block-vector length; bits[0] starts at 2.
python3 -c "
import pathlib
b = bytearray(pathlib.Path('/tmp/c.bin').read_bytes()); b[3] ^= 0x01
pathlib.Path('/tmp/head.bin').write_bytes(bytes(b))"

cargo raster run --input input.json --input-manifest input_manifest.json --audit /tmp/head.bin
# divergence is detected, then: panic at raster-prover/src/trace.rs:627,
# "Failed to resolve active sequence invocation ... in frame CfsCoordinates([])"
```

Note two adjacent outcomes worth not confusing with detection, both reachable from the same
sweep. Flipping byte 0 instead changes `bits_packer` and is refused by `validate()`
(`trace.rs:313`) — `Fingerprint claims 1997 items of 5 bits (157 packed blocks) but holds 125
blocks`. Flipping any byte with `^ 0xFF` rather than `^ 0x01` clears postcard's varint
continuation bit and is refused by the decoder — `Found a varint that didn't terminate` — before
any verification runs. Both exit non-zero without a fingerprint ever being compared.

§3 needs no tampering to read off the code: `FraudProofConfig::from_window_size`
(`trace.rs:157-176`) with `window_size >= 128` yields `bits_per_item == 1`, and
`TraceVerifier::verify` compares exactly that many bits at the trace's final index before
returning `VerificationResult::Ok` (`:852-878`, `:923`).
