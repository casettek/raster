# Issues — gaps without an owner

Index of `docs/issues/`. Last reviewed 2026-08-31.

An **issue** names a gap. A **proposal** owns a design for closing one. The two are kept
apart because the failure mode they guard against is different: a proposal that opens with a
weak problem statement is hard to argue with, because the design is already on the table and
the reader is arguing against work someone did. An issue is the problem statement alone,
written before anyone is invested in an answer.

An issue must:

- **be reproducible from the code as it stands** — every claim cited to a file and line, or to
  a program in `raster-inference` that hits it;
- **say what it is not** — which existing proposal is adjacent, and precisely where that
  proposal's scope stops short of this;
- **stop before the design.** A §Directions section may sketch shapes and name their costs. It
  must not pick one. When a direction is picked, it graduates to `docs/proposals/` and the issue
  becomes a `Closed by:` line.

An issue that cannot yet be reproduced is a note, not an issue; keep it in the relevant
proposal's §Uncertainties instead.

## Open

| issue | opened | subject | adjacent proposals |
| --- | --- | --- | --- |
| [`recur-accumulator-slots`](./recur-accumulator-slots.md) | 2026-08-25 | No loop-carried slot is both readable and incrementally committed. `state` is readable and re-committed whole every iteration; `output` is incremental and unreadable. A loop whose write is a function of the accumulated value — §2's test — has no cheap home, and a data-dependent `Break` can only be decided from the expensive one. Narrowed 2026-08-25: only 2 of `raster-chain-inference`'s 5 recur tiles pass the test; the rest are program-side, see that repo's `append-shaped-accumulators`. | [`loop-carried-state`](../proposals/loop-carried-state.md), [`recur-sequence-break`](../proposals/recur-sequence-break.md), [`incremental-draft-witness`](../proposals/incremental-draft-witness.md) |
| [`authenticated-chain-draft-output`](./authenticated-chain-draft-output.md) | 2026-08-27 | A stage returning a `Draft` panics the recorder in an authenticated `chain run`: the independent output-selection replay (`raster-runtime/src/tracing/recorder.rs:585`) finds no object at the finalized draft's `[u32::MAX, n]` coordinate. Stages 1–2 of `examples/chain-example` replay fine; stage 3 does not. Fails closed, not a soundness hole. Invisible because `chain_stage_cli.rs` only ever runs `--no-auth`, which installs no recorder — the fixture reached for the hardest output shape and never ran it in the posture that checks it. Confirmed pre-existing at `359582f`. | [`chain-stage-execution`](../proposals/chain-stage-execution.md), [`chain-io-commitment`](../proposals/chain-io-commitment.md), [`incremental-draft-witness`](../proposals/incremental-draft-witness.md) |
| [`counting-forked-from-profiling`](./counting-forked-from-profiling.md) | 2026-08-31 | Two accumulators answer "what did this run execute". `profiling` carries mode-independent fields (`tile_id`, `coordinates`, `input_bytes`) but is refused whole-artifact when unauthenticated (`unauthenticated-execution` §6.2, a ruling about *timings*), so `tile_census` re-derives a subset from a second hook with different gating into an untyped hand-built JSON — no `version`, no `run_id` to join the two, and `total_tile_executions` an identity over `trace_events` rather than a cross-check. Its "both authentication modes" header holds for the `tiles` map and not the `trace_events` one: four recur variants are structurally zero under `--no-auth` (`raster-macros/src/recur.rs:733`, `:856`, `:1251`) and nothing in the artifact says so. Not a soundness issue. `tile_census.rs` is untracked on `feature/recur-defered-finilize`. | [`unauthenticated-execution`](../proposals/unauthenticated-execution.md), [`artifact-inspection`](../proposals/artifact-inspection.md), [`trace-event-vocabulary`](../proposals/trace-event-vocabulary.md) |
| [`chain-shape-count-unverified`](./chain-shape-count-unverified.md) | 2026-08-31 | A `--stage` re-run takes its repeat-block counts from the run directory's `chain-shape` sidecar and checks them only against the manifest — `spec_digest` (`chain.rs:755`), `max` and producer position (`chain/expand.rs:271`, `:278`). The count itself is compared against nothing, though the producing stage's `output.bin` is in the same directory and `read_trip_count` (`chain.rs:806`) already decodes it without executing anything, and `verify_shape` re-derives every count from the producer (`chain/expand.rs:482-491`). A plausible altered count expands, executes, invalidates downstream and rewrites the sidecar against a graph the commitment `--stage` deliberately leaves untouched does not describe. Needs no tampering in one case: `--stage` on the count producer records the pre-run count over the artifact the run just wrote. | [`chain-repeat`](../proposals/chain-repeat.md), [`chain-io-commitment`](../proposals/chain-io-commitment.md), [`chain-stage-execution`](../proposals/chain-stage-execution.md) |

## Closed

_None yet._
