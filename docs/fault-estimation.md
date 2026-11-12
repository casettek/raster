# Fault-window cycle estimation

Use the live audit path to execute the actual fault-window guests without creating
proofs. This is for comparing whole-tile execution with non-tile verification
work while tuning tile sizes.

```sh
cargo raster run --audit /path/disputed.commit.bin \
  --input /path/input.json --input-manifest /path/input_manifest.json \
  --estimate --report /path/fault-estimate.json
```

The commitment must contain a detectable fault, produced with the existing
corruption/fault-injection workflow. Audit executes the stage once, verifies the commitment incrementally, and keeps a rolling window of each step's recorded inputs and outputs. It stops at the first divergence, builds that window's proof evidence once, and uses it for estimation or proving. An honest commitment
produces a `no-fault` report; it does not select a synthetic window. Native audit
and cold guest builds still take time.

Omit `--report` to write `fault-estimate.json` in the printed run directory.
Add `--estimate-steps 1` or `--estimate-steps 2` for a prefix measurement. The
limit is positive and capped at the actual window length. Only a complete window
can have `complete: true`; a successful prefix has `outcome: partial`. Failures
return a nonzero exit code and an incomplete error report with phase and execution
index where available. Successful measurements preceding a failure remain in the
report. Failures before live-audit output-path validation may not create a report.

This option is only available on `run --audit`, not `trace audit`. It conflicts
with `--proof-output`. The default audit still generates and verifies a real
proof. Estimation needs compiled RISC Zero guests and the executor, but does not
need Docker. Leave `RISC0_DEV_MODE` disabled. No receipt is produced and no
cryptographic proof verification, recursive composition, or Groth16 compression
is performed.

## Measurement boundaries

- **Authorization:** one execution for the input manifest, kept separate from
  per-step overhead.
- **Tile:** the complete tile guest, including decoding, input hashing, computation,
  and output encoding. This is not function-body-only timing.
- **Transition:** the existing transition guest for each trace step, including
  input decoding, state/commitment checks and receipt-assumption checks.

Guest code and identities are unchanged. Successfully executed guest journals
are carried forward as unresolved receipt claims; this exercises guest checks
but is not cryptographic evidence. The final complete window must still produce
the expected fault state bound to the disputed commitment.

## Reading the report

Version 1 reports window identity and coverage, guest image IDs, raw per-step
measurements, the final transition journal, and summary distributions. Each
measurement contains `user_cycles`, `segments` (including `po2`), `segment_count`,
`padded_cycles`, and `executor_duration_ms`. Padded cycles are the sum of
`2^po2` over segments. Executor time excludes native audit and guest compilation;
`audit_duration_ms` includes the single native audit and handoff validation.
Iris maps this total to its existing stage-replay timing. Estimation and proving
continue to run independent audits.

`RASTER_AUDIT_PROGRESS` emits `audit` before `estimating` or `proving`. No raw
trace is written by live auditing. The runtime retains authenticated storage,
provenance, active scopes, a log of storage writes, and a bounded rolling window of
recorded step inputs and outputs; total memory is not constant. Proof evidence is built
only for the divergent window, after rebuilding the storage proof state it opens with. Window positions are zero-based `[start, end)`;
execution indices are one-based.

Deploy the CLI and runtime together and rebuild native stages. Normal
SIGINT/SIGTERM cancellation reaps the child and removes its private handoff;
SIGKILL cannot execute cleanup. Recorded-trace auditing is unchanged.

`summary.tiles` and `summary.transitions` contain user-cycle and padded-cycle
count/min/max/median/mean/total statistics. Even-sized medians average the middle
two values. Empty populations have count zero and null statistics.
`summary.opening` and `summary.continuation` separate window initialization from
later steps. `transitions_by_kind` and `tiles_by_id` expose workload differences.

`summary.user_cycles` and `summary.padded_cycles` include:

- `authorization`, `transitions`, `tiles`, and their `total`;
- `non_tile_overhead = authorization + transitions`;
- `overhead_share = non_tile_overhead / total`;
- `overhead_per_tile_invocation = non_tile_overhead / measured tile count`.

Each tile step also has `transition_to_tile_ratio`. Zero denominators produce
null ratios. A failed transition may follow a successfully measured tile:
that tile contributes to the tile population, while `measured_step_count` counts
only completed transitions. All statistics on partial/error reports cover only
completed measurements, never extrapolated missing executions.

A 128-step window is not necessarily 128 tile calls: structural steps also cost
cycles. Use the actual populations and separate opening overhead. Matching tile
cycles to overhead is a useful tuning reference, not a demonstrated optimum;
larger tiles may also make transitions more expensive. Re-measure after authoring
changes. Neither cycle counts nor executor timings calibrate full proving time,
which also includes proof composition, compression, and host work.

## Validation

```sh
cargo test -p raster-prover --lib estimate::tests
cargo test -p raster-prover --lib replay::measure_tests
cargo test -p raster-cli --bin cargo-raster estimation_arguments
cargo test -p raster-cli --test fault_estimate_cli -- --ignored --nocapture
RISC0_DEV_MODE=0 RISC0_PROVER=ipc cargo test -p raster-prover --lib \
  estimation_matches_proof_cycles_and_rejects_bad_assumptions -- --ignored --nocapture
```

The real-prover command uses the matching installed `r0vm` to avoid debug-build
proving overhead. Omit `RISC0_PROVER=ipc` to use the in-process prover.

The recursive CLI fixture checks one-step, two-step, complete and oversized-limit
runs, honest commitments, error reports, stable cycle counts, and absence of proof
files or Docker calls. The real-prover test compares identical transition journals,
user cycles and segment counts with unresolved versus proven assumptions, verifies
the resulting real receipt, and rejects incorrect dependency claims.

## Single-pass audit checks

```sh
cargo test --release -p raster-cli --bin cargo-raster
cargo test --release -p raster-cli --test live_commitment_cli
cargo test --release -p raster-cli --test fault_estimate_cli -- --ignored
RASTER_TEST_REAL_PROOF=1 cargo test --release -p raster-cli --test fault_estimate_cli -- --ignored
```

Native regression tests compare live audit evidence against recorded-trace
verification for windows 2 and 128, including active scopes and final tails.
The estimation fixture checks that both faulty and honest commitments use one
audit phase and that no detection/capture timing fields are emitted.

### Reverted two-pass experiment (2026-11-01)

The two-pass experiment was reverted after its complete 1,310,495-item late-fault
audit took 1,366.830 seconds versus 1,143.675 seconds for the single-pass baseline
(19.5% slower). Native peak RSS fell only 4.3%. The additional execution outweighed
the reduction in witness construction. Live audit now uses the original
single-pass rolling-window implementation again.

Measurement artifacts remain in `target/two-pass-optimized-1793581951/` and
`target/two-pass-benchmark-1793579032/`; the verified experimental Groth16 receipt
is retained in `target/two-pass-proof-iQpRm2/`. The experiment's source, including
its benchmark harness, is recoverable from `target/single-pass-source-backup/`.
