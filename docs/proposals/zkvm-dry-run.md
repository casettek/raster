# Proposal: `zkvm-dry-run` — execute every tile in RISC0 without proving, and price it

Status: proposed 2026-08-17 (rev 2 — narrowed from `guest-replayability-check`)

Revision note: rev 1 proposed four tiers (static lint, cross-compile probe, executor replay,
cycle budget). Rev 2 keeps only the executor replay and the cost model. The static and
cross-compile tiers belong to [`authoring-skill-and-tooling`](./authoring-skill-and-tooling.md)
§3 and are dropped here rather than duplicated.

Related:
- [`authoring-skill-and-tooling.md`](./authoring-skill-and-tooling.md) — owns RAS-206 ("tiles
  MUST be deterministic", enforcement `[none]`) and RAS-208 ("tiles MUST stay small enough to
  replay", enforcement `[none]`). §6 Non-goals says "true enforcement is the zkVM replay
  itself". This proposal makes that replay cheap enough to run on every change.
- `docs/specs/core/3-execute/04-zkvm-preview-execution.md` — specifies `ExecutionMode::Estimate`
  and `calculate_proof_cycles`. §5 of this document reports two defects in what it specifies.
- [`chain-fraud-proof.md`](./chain-fraud-proof.md) — owns the fraud-proof *window*. §3 explains
  why a dry run is not windowed.

## 1. Problem

RAS-206 and RAS-208 are the two rules in the authoring set with enforcement `[none]`, and they
are the two that decide whether a program can be proven at all:

- **Does the tile run in the guest?** Panics, out-of-bounds, and allocation past the guest's
  192 MiB address space (`GUEST_MAX_MEM = 0x0C00_0000`) do not exist on a host with 64 GB.
- **Does it produce the same bytes?** `usize` is 64-bit natively and 32-bit in the guest;
  `HashMap` iteration order and `libm` transcendentals need not agree across targets.
- **What will proving cost?** Today: unknown until you prove.

The check ladder (`.claude/skills/raster/SKILL.md` §9) has six rungs and exactly one — rung 4,
`cargo raster run --audit` — reaches the zkVM. It reaches it with
`ExecutionMode::prove_and_verify()` (`raster-cli/src/commands/run.rs:757`), and only for the
steps inside a fraud-proof window, and only once verification has already found fraud. So the
author's options are an unenforced rule or the most expensive operation the toolchain has.

## 2. Why this is a decision, not an estimate

The RISC0 executor is not a model of the prover. It is the prover's first stage:
`default_prover().prove()` executes the ELF to produce segments and then proves them;
`default_executor().execute()` stops after the first half. Segment po2 limits are applied
during execution (`risc0-zkvm-1.2.6/src/host/server/exec/executor.rs:133-147`), so a program
too large to segment fails in the executor too.

> If `execute()` returns a `SessionInfo`, `prove()` on the same ELF and input produces a
> receipt, subject only to *host* resources — not to anything about the program.

The honest limit: it decides this **for the input actually run**. A tile that survives a 64-row
fixture and exhausts the address space at 64k rows is not caught by replaying the 64-row
fixture. That is a coverage argument, and §6's headroom column is the only answer this proposal
offers to it.

## 3. Design: `cargo raster run --dry-run`

A flag on `run`, not a separate command. The reason is mechanical: the dry run needs the
`(Trace, TraceRecorder)` pair — the recorder is what holds per-step input/output witnesses via
`step_witness_at` — and that pair comes from `load_trace_from_file` on the **trace artifact**
every native run writes (`run.rs:255`). It does *not* come from a commit file. A standalone
command would have to redo the native run to produce one.

```bash
cargo raster run --input input.json --input-manifest input_manifest.json --dry-run
```

Runs the program natively as usual, then for every `StepKind::Exec(ExecTarget::Tile(_))` step
in the trace:

1. compile the tile for RISC0 (cached by `Risc0ArtifactStore` on source ⊕ recipe fingerprint —
   unchanged tiles cost nothing after the first run),
2. execute the guest ELF in the executor with the step's recorded `input_data()`,
3. compare `TileReplayJournal.output_bytes` against the step's recorded `output_data()`,
4. compare `TileReplayJournal.input_commitment` against `sha256(replay_input)`,
5. record the session's segment vector for §5.

`--dry-run` composes with `--commit` and conflicts with nothing. It requires the RISC0
toolchain, and should degrade to a clear diagnostic — not a panic — when `find_risc0_cargo`
returns `None`.

### Not windowed

The fraud-proof window exists because proving is the budget: `chain-fraud-proof` makes window
size the constant that keeps proving cost independent of trace length. Remove the prover and the
reason for the window goes with it. "Is my program replayable" is a question about every tile
step, and at executor cost every step is affordable. `--dry-run --steps a..b` is a follow-on if
a trace ever gets large enough to hurt.

## 4. What blocks this today

Rev 1 called this "a mode flag through an existing path". That was wrong, and the correction
matters for sizing. Three things in the current code prevent `Replayer` from running in
`Estimate` mode at all:

**4.1 — `Replayer::replay` hard-requires a receipt.** `raster-prover/src/replay.rs:106`:

```rust
let receipt_bytes = exec_result.receipt.clone().ok_or_else(|| {
    Error::Other("Replay requires a proof receipt to recover the replay journal".into())
})?;
```

`ExecutionMode::Estimate` returns `receipt: None` by construction, so passing `Estimate` to
`replay()` fails on the line after execution succeeds.

**4.2 — the journal is decoded and discarded, twice.** `Risc0Backend::execute_tile`'s `Estimate`
arm already decodes `TileReplayJournal` from `session.journal.bytes` and keeps only
`output_bytes`. The `Prove` arm does the same from `receipt.journal.bytes`. Then `Replayer`
re-decodes the journal from the receipt it just serialized. `TileExecutionResult` has no field
for the journal, which is why the receipt is the only channel back.

**4.3 — `ReplayResult` drops cycles.** `execute_tile` returns `cycles` and `proof_cycles`;
`Replayer::replay` reads neither. The cycle data this proposal is half about is already computed
and thrown away one frame below where it is needed.

One field unblocks all three:

```rust
// raster-backend/src/backend.rs
pub struct TileExecutionResult {
    …
    /// Raw zkVM journal bytes, in both Estimate and Prove mode. The receipt is
    /// not the only way back to the journal — dry runs have no receipt.
    pub journal: Option<Vec<u8>>,
}
```

populated in both arms of `execute_tile`, with `Replayer` decoding `TileReplayJournal` from it
instead of from the receipt.

**Do not make `ReplayResult.receipt` optional.** It is consumed by `step_transitions` on the
fraud-proof path, where "a replay always has a receipt" is a useful invariant. Add a sibling
instead:

```rust
// raster-prover/src/replay.rs
pub struct DryRunResult {
    pub fn_name: String,
    pub output: Vec<u8>,
    pub replay_journal: TileReplayJournal,
    pub cycles: u64,
    pub segments: Vec<(u32, u32)>, // (po2, cycles) per segment
    pub exit_code: ExitCode,
}

impl<'a> Replayer<'a> {
    pub fn dry_run(&self, tile_id: &str, input_bytes: &[u8]) -> Result<DryRunResult>;
}
```

sharing `compile_tile` + `execute_tile` with `replay()` and perturbing the soundness-critical
path not at all.

## 5. Cycle estimation: two defects in the current model

The dry run is where cycle numbers become available, so it is where the model should be fixed.

### 5.1 — `calculate_proof_cycles` is wrong for multi-segment executions

`raster-backend/src/backend.rs:112`:

```rust
pub fn calculate_proof_cycles(actual_cycles: u64) -> u64 {
    if actual_cycles <= MIN_PROOF_SEGMENT_CYCLES { MIN_PROOF_SEGMENT_CYCLES }
    else { actual_cycles.next_power_of_two() }
}
```

RISC0 proves **each segment separately**. Proof cost is `Σ 2^po2` over segments, not
`next_power_of_two(Σ cycles)`. `SessionInfo` carries exactly what is needed:

```rust
pub struct SessionInfo { pub segments: Vec<SegmentInfo>, … }
pub struct SegmentInfo { pub po2: u32, pub cycles: u32 }
```

For a 10M-cycle execution segmented at po2 20, the true cost is ≈ 10 × 2²⁰ ≈ 10.5M padded
cycles; `calculate_proof_cycles` reports 16.8M — 60% high. It is correct only in the
single-segment case, which is the case the tile-preview path was written against.

Proposed: keep `calculate_proof_cycles` as the estimator for callers holding nothing but a cycle
count, and add the exact form where segments are in hand:

```rust
pub fn proof_cycles_from_segments(segments: &[SegmentInfo]) -> u64 {
    segments.iter().map(|s| 1u64 << s.po2).sum()
}
```

### 5.2 — `Estimate` and `Prove` report different quantities under the same name

- `Estimate` arm: `session.cycles()` — documented in risc0 as "the total number of **user**
  cycles across all segments, without any overhead for continuations or po2 padding".
- `Prove` arm: `prove_info.stats.total_cycles` — where `SessionStats` separately carries
  `user_cycles`, `paging_cycles`, and `reserved_cycles`.

Both land in `TileExecutionResult.cycles`. So a dry run and a later prove of the same tile
disagree by the paging and reserved overhead — not a rounding difference for a tile that touches
a lot of memory, since paging cycles scale with the working set.

Proposed: report user cycles in both modes (`stats.user_cycles` in the `Prove` arm) and carry the
breakdown separately where available. A dry-run number that does not predict the prove-run number
is worse than no number, because it will be trusted.

## 6. Report

```
zkVM dry run — 4 tiles, 516 steps, RISC0 executor (no proving)

tile           steps   user cycles      segments   proof cycles   vs budget   status
parse_header       1       412_003        1×2^19        524_288        1.6%    ok
score_row        512     1_204_889        2×2^20      2_097_152        6.3%    ok
render             1    18_442_110       18×2^20     18_874_368       56.3%    WARN
summarize          1             —             —              —           —    FAIL

  summarize (step [0,3]): guest aborted
    index out of bounds: the len is 0 but the index is 3
    tiles build as release; this panicked in the guest and not natively

total proof cycles: 21_495_808   budget 33_554_432 (2^25)   headroom 1.6×
FAILED: 1 tile aborted
```

Three verdicts, and only these: `ok`, `WARN` (over `warn_tile_cycles`), `FAIL` (guest abort, or
output mismatch, or over `max_tile_cycles`). Non-zero exit on any `FAIL`.

An output mismatch renders as the first differing byte range plus both lengths — usually enough
to identify which §7 hazard fired.

Budgets in `Raster.toml`:

```toml
[dry_run]
warn_tile_cycles = 8_000_000
max_tile_cycles  = 33_554_432
```

Headroom, not absolute cost, is the number to read: it answers "how much bigger can my input get
before this stops proving", which is the question §2's coverage limit leaves open.

## 7. What a mismatch means

The failure the dry run reports is one of these. This table is the feature's diagnostic value —
the report should link to it.

| Symptom | Cause | Note |
| --- | --- | --- |
| output mismatch, integer fields wrong | `usize` is 32-bit in the guest, 64-bit natively | postcard varint-encodes `usize`, so the wire format survives below 2³² but the value need not |
| output mismatch, collection order wrong | `HashMap`/`HashSet` iteration order — RAS-206 | the guest links `std` (see the Cargo.toml comment in `guest_builder.rs`), so `RandomState` exists there and need not agree with the host's |
| output mismatch, float low bits differ | `riscv32im` has no F/D extension; `libm` transcendentals route through a different implementation | basic IEEE-754 ops agree; `sin`/`exp`/`powf` need not |
| guest abort, allocation failure | working set exceeds `GUEST_MAX_MEM` (192 MiB) | RAS-208's only real teeth — no static check sees this |
| guest abort, panic message | `unwrap`, index, explicit panic | `generate_guest_main` calls `.expect("Tile runtime failure")` and there is no structured error channel, so the message must be scraped from the executor error verbatim |
| aborts natively but not in the guest, or vice versa | the generated guest crate declares its own `[profile.release]` and an empty `[workspace]`, so the project's `overflow-checks` / `debug-assertions` never reach guest builds | native `run` builds under the project's profile (`run.rs:77`); worth its own fix, which changes image ids (`GUEST_BUILD_ABI` bump) |

## 8. Implementation

| File | Change |
| --- | --- |
| `raster-backend/src/backend.rs` | `TileExecutionResult.journal: Option<Vec<u8>>`; `proof_cycles_from_segments`; document `cycles` as user cycles |
| `raster-backend-risc0/src/risc0.rs` | populate `journal` in both arms; `Prove` arm reports `stats.user_cycles`; surface `SessionInfo.segments` |
| `raster-prover/src/replay.rs` | `DryRunResult` + `Replayer::dry_run`; `replay()` decodes the journal from `exec_result.journal` rather than from the receipt |
| `raster-cli/src/commands/dry_run.rs` *(new)* | trace walk, comparison, aggregation, report |
| `raster-cli/src/commands/run.rs` | call it after `load_trace_from_file` when the flag is set |
| `raster-cli/src/main.rs` | `--dry-run` on `Run` |
| `raster-compiler/src/project.rs` | `[dry_run]` budgets |
| `.claude/skills/raster/SKILL.md` | rung 3.5 in §9; §7's table folded into the failure→rule map |

Tests: a fixture crate with one tile that diverges (`usize` truncation) and one that aborts
(index OOB), asserting the dry run reports both and exits non-zero. Both are cheap — they need
the executor, not the prover.

## 9. Non-goals

- **No proving, no trace-format, no commitment change.** The dry run reads a trace artifact and
  writes a report; nothing it produces is consumed by any verifier.
- **No replacement for rung 4.** A green dry run says every tile ran in the zkVM and produced the
  recorded bytes. It says nothing about trace honesty, CFS conformance, or fraud-proof soundness.
  The commit/audit round-trip stays in the ladder.
- **No static analysis.** Deliberately dropped in rev 2; that is `cargo raster check`'s job.
- **No input-space coverage claim.** §2.

## 10. Uncertainties

1. **`--dry-run` on by default?** Once it is cached and fast, the argument for making a plain
   `cargo raster run` do it is strong. Against: it hard-requires the RISC0 toolchain, which `run`
   currently does not. Leaning opt-in until the cache behaviour is measured.
2. **Parallelism.** Steps are independent and `rayon` is already a dependency of
   `raster-backend-risc0`. Worth measuring before adding — the executor may already be
   memory-bound at one instance per core.
3. **Recur steps.** A recur site with 512 iterations produces 512 tile steps with 512 distinct
   inputs. Replaying all of them is correct, and is also where the cost concentrates.
   Deduplicating by input hash would help programs that re-execute identical inputs and do
   nothing for the normal case; §3's `--steps` is probably the better lever.
4. **§5.1 changes reported numbers.** Fixing `calculate_proof_cycles` at its call sites changes
   what `run-tile` and the preview path print. That is a correction, not a regression, but the
   spec (`04-zkvm-preview-execution.md`) documents the current formula and must be updated with
   it.
