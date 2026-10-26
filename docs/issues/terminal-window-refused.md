# Issue: `terminal-window-refused` — `chain fraud-prove` detects output fraud and then cannot prove its evidence

Status: **open**, opened 2026-10-22. **Completeness gap, fails closed**: no receipt is produced, so
nothing unsound is accepted. But it leaves the chain without any working execution-fraud path.
Reproduced in dev mode (§Reproduction).

Related:
- [`chain-io-commitment`](../proposals/chain-io-commitment.md) §3 (the dispute protocol) and §4b
  ("the interim execution-fraud path"): the design that needs a terminal-window receipt. Both the
  interim evidence and a §3 challenge start from one.
- [`trace-end-windows`](../proposals/trace-end-windows.md): where "the last window item must
  diverge" comes from.
- [`transition-split`](../proposals/transition-split.md): FRAUD WINDOW, whose receipt is now the
  only window proof. Its journal is documented as existing "only for a window whose last step
  diverged".

## What happens

When `chain fraud-prove` finds a stage whose committed output differs from an honest re-run, it
proves the **terminal window** of the auditor's own honest re-run, as evidence
(`crates/raster-cli/src/chain.rs:1285` `prove_terminal_window`, called at `:1590`). The window and
the commitment it is proven against come from the same run:

```rust
// crates/raster-cli/src/chain.rs, prove_terminal_window
let trace_commitment = TraceCommitment::try_build(&trace, &EMPTY_LEAF, fraud_proof_config)?;
let mut verifier = TraceVerifier::new(trace_commitment.clone(), &EMPTY_LEAF, &cfs)?;
let window = verifier.terminal_window(&trace)?;
```

`TraceVerifier::terminal_window` says so explicitly (`crates/raster-prover/src/trace.rs:949`):
"Not a fraud path — no divergence is sought, and none is expected: the caller builds the
commitment and the trace from the same run, so the slice matches by construction."

The window then goes through the same prover as a fraud window. The trace guest applies the fraud
verdict at every step, and at the window's last index it requires a divergence
(`crates/raster-prover/guests/trace/src/segment.rs:72-85`):

```rust
if index as u64 + 1 == window_len {
    // ...
    assert!(
        diverges || diverges_by_root,
        "A fraud proof's final step must diverge from the commitment, by fingerprint \
         entry or by revealed trace root",
    );
}
```

The terminal window's last step matches its own commitment by construction, so the trace guest
always refuses it, and `chain fraud-prove` panics before any receipt exists.

## When it broke

- **2026-08-27, `8e3daad`:** `chain-io-commitment` §4b added the terminal-window evidence path.
  The transition guest then compared items against the fingerprint without requiring the last one
  to diverge.
- **2026-09-16, `c484ef7` ("fix fingerprint tail"):** the rule "every item before the last must
  match, and the last must diverge" landed in the transition guest's `fraud_proof.rs`. It was
  needed for fraud windows: before it, a one-item window was unprovable, and every window lost an
  item of margin. It also made every terminal window unprovable, and nothing tested that path.
- **`transition-split` steps 2–5 (2026-10-22)** moved the same rule into the trace guest and the
  terminal path onto FRAUD WINDOW (`verify_fraud_window_receipt` at `chain.rs:1597`). That changed
  where the refusal happens, not whether it happens.

No test covers it. `crates/raster-cli/tests/chain_repeat_cli.rs` exercises `chain fraud-prove` for
shape faults only, and `chain_stage_cli.rs` runs `--no-auth`, which records no trace.

## What it is not

- **Not a soundness gap.** Nothing accepts a wrong claim; the path produces nothing.
- **Not the recur limitation** that `chain-io-commitment` names as **S3** ("a window opening
  inside a live recur loop is rejected"). That one depends on where the window opens. This one
  refuses every terminal window, including on programs with no recursion at all
  (`examples/chain-example` stage `aggregate`).
- **Not `window_is_terminal`.** The trace guest derives the terminal flag correctly. It's the
  verdict that has no terminal mode.

## Reproduction

Dev mode, about a minute. Needs `examples/chain-example`'s three stages built with the risc0
backend so their locks match (`(cd phase1-normalize && cargo raster build --backend risc0)` and
likewise for `phase2-` and `phase3-`).

1. From `examples/chain-example`: `cargo-raster raster chain run`. This writes an authenticated
   `chain-commitment` under `target/raster/chains/<run>/`.
2. Flip one byte of stage 1's `output_payload_commitment` in that file. It is a postcard
   `raster_core::chain::ChainCommitment`; decode, edit `stages[1].output_payload_commitment[0]`,
   re-encode.
3. `RISC0_DEV_MODE=1 cargo-raster raster chain fraud-prove`.

Observed 2026-10-22, on the `feature/prover-split` work:

```
output fraud: stage 1 'aggregate' committed acef828bfb21… but honest execution yields adef828bfb21…
proving stage 'aggregate' terminal window (FRAUD WINDOW)…
Proving the fraud window...
thread 'main' panicked at src/segment.rs:81:13:
A fraud proof's final step must diverge from the commitment, by fingerprint entry or by revealed trace root
trace guest refused the segment starting at trace position 9: Guest panicked: ...
```

Detection works; only the proof fails.

## Directions

Each one changes what a window receipt means, so this is a design decision, not a fix.

1. **A terminal verdict in the trace guest.** The trace part takes a verdict kind in its header:
   `Divergence` (today's rule) or `Terminal` (every step matches, and the window ends where the
   commitment ends). Its output, and `FraudWindowJournal`, carry which one was proven.
   - Cost: the journal stops being a verdict by its existence. Every consumer of a fraud proof
     (`verify_fraud_window_receipt`, a settlement layer, `chain_fraud`) must check the kind, and
     one that forgets accepts an honest window as fraud. That is the bug class of
     [`authorization-image-id-unpinned`](./authorization-image-id-unpinned.md) in another form.
2. **A separate aggregator image id for terminal claims.** The same parts, but a second build of
   FRAUD WINDOW (or a second compiled-in mode) for terminal windows, with its own image id and
   journal type. A fraud-proof verifier pins the fraud id and can never accept a terminal receipt.
   - Cost: a second id to pin for the dispute path, and a third pass or a second binary in the
     guest build.
3. **Drop the interim evidence receipt.** `chain fraud-prove` reports the detected output fraud
   without proving anything, until `chain-io-commitment` §3's challenge protocol is built, which
   needs a terminal-window receipt anyway.
   - Cost: no artifact at all for execution fraud in the meantime. It only defers the choice
     between 1 and 2.

Whichever is picked needs the test that would have caught this: an authenticated chain, a
tampered stage output, and `chain fraud-prove` producing a verifiable receipt in dev mode.
