# Proposal: `tile-cost-estimator` — what the `risc0-estimator` branch should be

Status: implemented and verified 2026-10-30 on branch `replay-profile` (§6, steps 1–7; step 7
results in §6). Began as the scope document for `origin/risc0-estimator` (`c826103`). Usage:
[`replay-profiling.md`](../replay-profiling.md).

Related:
- [`zkvm-dry-run.md`](./zkvm-dry-run.md) — executor replay without proving; §4's blockers and
  §5's cycle-model defects apply here unchanged.
- [`bounded-collections.md`](./bounded-collections.md) — `Block<T>` and `chunk = N` pin how many
  elements reach a replay unit; §4.3 uses those bounds.
- [`authoring-skill-and-tooling.md`](./authoring-skill-and-tooling.md) — RAS-208 ("tiles MUST
  stay small enough to replay"), enforcement `[none]`.

## 1. The question

One question, per tile:

> What does one invocation of tile `T` cost in the zkVM, and what is the most it can cost?

"Cost" means guest user cycles, segments, and padded proof cycles (Σ 2^po2 over segments). It
does not mean proving time, and it is not tied to any proof structure built from tiles.

**Non-goals.**
- **No fraud-proof window.** The 128 in the branch reads as `PROTOCOL_WINDOW_SIZE`. It is not
  that, and it should not look like it. The window decides how many steps a defence proves.
  It says nothing about what one tile costs.
- **No transition overhead.** The transition guest's cost belongs to the proof chain, not to
  a tile. Measuring it needs a modelled 128-step window context (`transition_profile.rs`,
  `replay_profile_context.rs`). That is about 800 lines that couple the profiler to
  fraud-proof internals, and those internals have changed on main since the branch point.
- **No proving.** Executor only.

## 2. What the branch has, and what stays

`origin/risc0-estimator` is 2 commits on top of `eb7712a` (2026-09-13), +3,905/−52 over 27 files.

| Part | Files | Verdict |
| --- | --- | --- |
| Prepare a tile once, replay with the executor, check input commitment and output, return cycles | `raster-prover/src/replay.rs` (`prepare_profile`, `profile_with_journal`) | **keep** — this is `zkvm-dry-run` §4 done |
| Journal returned without a receipt | `raster-backend/src/backend.rs` (`TileExecutionResult.journal`) | **keep** |
| Guest stdout sink, exit-code check in `Estimate` | `raster-backend-risc0/src/risc0.rs` | **keep** |
| Report artifact | `raster-analysis/src/replay_profile.rs` | **keep**, reshape per §5. It is read by `replay report`, no longer by `analyze` |
| CLI walk over the trace, failure handling, partial reports | `raster-cli/src/commands/replay_profile.rs`, `profiling.rs`, `run.rs`, `main.rs` | **keep the logic, move it** into `commands/replay.rs` (§5.1). Drop the cap, the overhead hook and the `run` flags |
| Transition overhead | `raster-prover/src/transition_profile.rs`, `raster-cli/src/commands/replay_profile_context.rs`, `raster-runtime/src/tracing/replay_profile.rs`, the capture hooks in `recorder.rs`, `StorageManager::profile_index_witness` | **drop** |
| `RecurTileItem` always serializes every field in binary formats | `raster-core/src/cfs.rs` | **drop** if only the transition program frame needed it. A `raster-core` edit moves tile IDs. |
| Recur frame records its sequence depth, plus a `recur_draft.rs` assertion | `raster-runtime/src/storage.rs` | **check on main first** — this looks like a real fix for a sequence nested inside a recur site, not profiling code |
| Trace writer joins its thread on a send failure | `raster-runtime/src/tracing/publishers/binary.rs` | **check on main first** — error reporting, unrelated to profiling |
| "128 invocations", "replay total + 128 × overhead", window context | docs, constants, report text | **remove** |

## 3. Measured: zkVM executor vs native

**Setup (2026-10-30).** iris `prefill_range_l0` (`bench-iris/sc`, raster `8c81c47`, risc0
3.0.6, the same version as main), 24-core host, executor single-threaded. A native run with `--features profiling`
gave per-invocation native times. A dev-mode `--audit` of a 128-step window at step 757
replayed its 68 tile steps. Each step ran once through `default_executor().execute()`, timed
around the call, before the dev-mode prove. The probe is `bench-iris/est-probe.patch` and the
join script is `bench-iris/est-scripts/join.py`. Both were reverted or kept out of the trees.

| tile | steps | user cycles (avg) | segments | executor | native body | native total | executor / native total |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `mac_weight_page` | 18 | 3,300,178 | 4.2 | 74.5 ms | 21.0 µs | 141.3 µs | 528× |
| `accumulate_context` | 2 | 936,980 | 2 | 48.8 ms | 2.1 µs | 61.6 µs | 792× |
| `gelu_mul` | 2 | 488,602 | 1 | 27.0 ms | 21.9 µs | 61.1 µs | 443× |
| `zero_accum` | 12 | 57,776 | 1 | 17.2 ms | 0.3 µs | 13.4 µs | 1,290× |
| `ple_gate_mul` | 2 | 30,644 | 1 | 16.6 ms | 0.7 µs | 31.6 µs | 526× |
| **all 68 steps** | | 69.5M total | | **2.47 s** | **0.49 ms** | **3.95 ms** | **627×** |

"Native body" is the tile function alone. "Native total" adds Raster's wrapper (input
resolution, output store, trace). The executor column covers everything the guest does.

**Findings.**

1. **The executor costs about 500–1,300× native per invocation.** It is about 5,000× the tile
   body alone, but that ratio is meaningless for tiles whose body is sub-microsecond.
2. **Cost = about 20 ms fixed + cycles at about 60 MHz** (least-squares fit over the 68
   steps). The fixed part dominates every tile under about 1M cycles. It is per-execution
   setup, which we infer is loading the program binary and building its memory image (not
   measured separately). The branch already avoids the second fixed cost: the audit path
   reloads the artifact and runs a dev-mode prove each step, about 90 ms per step against 36
   ms for the bare executor.
3. **Cycles follow byte counts more closely than native time.** `mac_weight_page` runs for
   21 µs natively and costs 3.3M cycles. It takes a 97.5 KB input, about 36 cycles per byte.
   The next heaviest tiles run at 50–120 cycles per byte. We infer that the guest's input
   decode and input-commitment hash dominate, but did not split this inside the guest. Rank
   correlation with cycles over the 68 steps (`bench-iris/est-scripts/proxy.py`):

   | native signal | correlation | true top 10 found | packed_values pick |
   | --- | ---: | ---: | ---: |
   | input + output bytes | 0.99 | 10/10 | 100% of max |
   | input bytes | 0.95 | 9/10 | — |
   | native total time | 0.92 | 10/10 | 100% of max |
   | native body time | 0.86 | 6/10 | 32% of max |

   Within one tile, cycles vary up to 36× (`mac_weight_page`: 185k to 6.7M), so which call
   gets measured matters. In this 68-step sample, byte counts picked the true heaviest call
   in each of the three tiles with more than two calls. The full run is less clean: see the
   step 7 results in §6. Body-only native time is the weakest signal.
4. **Replaying every invocation is affordable.** The stage makes 1,119 tile calls, and
   native tile work totals 60 ms. Extrapolating from these averages, replaying all of them
   costs about 40 s single-threaded. That is about 4× the 11 s native run, before
   parallelising across cores. So **there is no reason for a per-tile cap by default**.
   Sampling is a fallback for traces large enough to need it, not the design.

Caveats: one stage, one run, on risc0 3.0.6. That is main's version, so the numbers apply
to the port. The branch itself is still on risc0 1.2, so its APIs need porting (§6).

## 4. Worst case: why synthetic inputs cannot bound it, and what can

### 4.1 Synthetic inputs give a lower bound, never an upper one

A tile's cycle count is a function of its input *data*, not its input *size*. Loop trip
counts, early exits, branch choices and allocator behaviour all depend on values. A
maximum-size input can be cheaper than a smaller one, for example a scan that stops at the
first match. Finding the input that maximises cycles is worst-case execution time (WCET)
analysis. On a RISC-V ELF compiled from general Rust, with value-dependent loops, it has no
sound general answer. Static WCET tools need annotated loop bounds that Raster tiles do not
declare.

So a synthetic or fuzzed input proves only "there exists an input costing X". That is a
**lower** bound on the worst case. It is still useful as a stress test (§4.3), but not as the
number to plan against.

There is also a practical problem. A tile input is not arbitrary bytes: it is a postcard
value whose storage selections must resolve and whose commitments must match. Generating
valid synthetic inputs needs a generator per type, which Raster does not have.

### 4.2 The only sound upper bound is an enforced one

Give each tile a declared budget, `max_cycles`. The `estimated_cycles` tile attribute already
exists, documents "known-heavy tiles" (RAS-208), and is used only by
`Backend::estimate_resources`. Enforce the budget at replay with the executor's session
limit, so an over-budget execution fails the same way a guest panic does. Then
**worst case = budget by construction**, whatever the data.

This is a protocol change, not a profiler feature:
- an honest claimer must know before committing that every step is within budget. Per §3
  that is affordable: one executor pass over the trace.
- the step rules must say what an over-budget step means. A failed step is the natural
  reading, so a challenger can dispute it like any other replay failure.

It belongs in its own proposal. This branch's job is to produce the numbers a budget is
chosen from.

### 4.3 What the estimator should report to choose a budget

1. **The observed maximum over every invocation** (§3.4), with p50/p99 and the heaviest
   invocation's coordinate. This is a fact about the workload, not a bound.
2. **Cycles against input bytes, per tile**: a fit of the form `a + b · input_bytes` and its
   residual. When cost is driven by size, as in §3.3, the fit extrapolates to the declared
   size limit: `Block<T>` ranges and `chunk = N` pin element counts in the CFS, and pages pin
   byte sizes. When the residual is large, cost is driven by the data and the fit is not
   used.
3. **Headroom** against a declared `max_cycles` when one exists: `max_cycles / observed max`.

Synthetic inputs then serve one purpose: a **stress check** at the size limit. Build a
maximum-count, maximum-page input from the declared bounds and confirm the tile stays under
budget. A failure is a real finding. A pass does not prove the bound.

Recommended path: **measure the heaviest calls by size (all calls when in doubt) → model
cycles by size → enforce a budget → stress-check at the bounds.**

## 5. Branch scope after the rebase

### 5.1 Commands

```bash
cargo raster run --input input.json --input-manifest input_manifest.json   # any authenticated run
cargo raster replay profile target/raster/runs/<run-id>                    # measure → replay-profile.json
cargo raster replay report  target/raster/runs/<run-id>/replay-profile.json  # redraw a saved report
```

- **`replay` is a new command group in its own file**, `crates/raster-cli/src/commands/replay.rs`.
  The selection, replay and report code moves there from today's `replay_profile.rs`.
- **`replay profile` takes the run directory and no options.** It replays the top 10 calls
  per tile (§5.3). `10` is a constant in `replay.rs`, not a flag. Every tile in the CFS is
  covered; there is no tile filter.
- **`replay report` redraws a saved `replay-profile.json`** without rerunning anything.
  `cargo raster analyze` stays native-only: the branch's replay and `zkvm-profile` reading in
  `analyze` is removed.
- **`--commit` is not needed.** Every authenticated `run` writes the full trace.
- **`run` gains one artifact, `run.json`, in the run directory.** It holds the input and
  input-manifest paths, the trace format and a digest of the CFS (§6, step 3). With it, `replay profile`
  needs only the directory. It also lets `replay profile` warn when the project has changed
  since the run. Each replay still checks every call's output bytes against the trace, so a
  changed tile cannot go unnoticed.
- **Removed from `run`:** `--profile replay`, `--profile zkvm`, `--zkvm-profile` and `--tile`.
  The branch is unmerged, so nothing depends on them. `--profile native` stays exactly as it
  is: it times the native run itself, so it belongs on `run`.

### 5.2 Data flow: nothing new is recorded

1. **Native run, authenticated (the default).** The program process runs every tile
   natively and writes the full trace to `trace.bin`. Every tile call's record carries its
   replay input and output bytes (`FnCallRecord::input_data` / `output_data`). The program
   process has no replayer and knows nothing about the zkVM. `--no-auth` runs write no trace,
   so profiling requires an authenticated run, as the branch already enforces. On iris
   `prefill_range_l0` the trace is 30 MB for 1,119 tile calls.
2. **`replay profile`, later and separately.** Read `run.json`, rebuild the CFS from the
   project, and load the trace (`load_trace_from_file`), which rebuilds
   each step's witness. No program code runs in this step.
3. **Select.** For each tile, rank its calls by input + output witness bytes and take the
   top 10.
4. **Replay.** Run each selected call once in the RISC Zero **executor**, using its recorded
   input bytes. This is `ExecutionMode::Estimate`, which calls
   `default_executor().execute()`. **No proof is generated:** the branch's
   `profile_with_journal` returns an error if a receipt ever appears. Each replay checks the
   input commitment and output bytes against the trace.

The input/output sizes come from the trace, not from a wrapper around `ObjectStore`. The
guest pays for the replay input, which is selected sub-values, inline arguments and recur
state, not for whole stored objects. The trace already holds that exact number for every
call (it matched the profile's `input_bytes` on all 68 iris steps in §3).

### 5.3 Selection (version 1)

- For each tile, rank all calls by **input + output witness bytes** and replay the **top
  10**. Ties keep trace order. A tile with 10 calls or fewer is replayed in full. On iris,
  the top 10 held the exact heaviest call for 18 of 28 tiles, and came within 0.2% of it
  for the rest. Every miss was a tie on size (§6, step 7 results).
- No replay-all mode in version 1. Checking the ranking against every call is a one-off
  development check on iris: raise the constant temporarily and compare.
- A tile whose cost depends on values rather than size can hide its heaviest call outside
  the top 10. Version 1 accepts that risk and shows it: the report puts each replayed call's
  byte rank next to its cycle rank.

### 5.4 Measures

Per replayed call, from the executor's `SessionInfo`, at no extra cost:

| measure | source |
| --- | --- |
| user cycles | `session.cycles()` |
| segments | `SessionInfo.segments.len()` |
| padded proof cycles, Σ 2^po2 | `SessionInfo.segments[*].po2` — fixes `zkvm-dry-run` §5.1 |

Nothing inside the guest is split by phase (decode, hashing, body). That would need guest
instrumentation and a second guest build per tile, and is out of scope.

### 5.5 Report

**Rows are the CFS's tiles.** The tile list comes from the program's CFS (`cfs.tiles`), not
from whatever happened to run. Every tile the program declares appears once, by tile ID.
A tile that was never called says so instead of silently missing from the list.

**Default view: the cost bar.** Rows are sorted by the cycles of each tile's heaviest
replayed call. The bar is that call as a share of the most expensive tile's heaviest call.
The other columns give the absolute numbers:

```text
tile                   heaviest call, user cycles (% of max)  seg  padded   exec  calls
mac_weight_page        ████████████████████████  6.71M  100%   8   8.39M  127ms    288
accumulate_context     ███▎                       939k   14%   2   1.31M   49ms     24
score_key              ███                        875k   13%   2   1.11M   46ms     24
gelu_mul               █▋                         489k    7%   1   1.05M   27ms     24
finish_layer           █▎                         367k    5%   1    524k   32ms     24
attn_residual          █                          282k    4%   1    524k   26ms     24
...
finish_context         ▎                           81k    1%   1    131k   19ms     24
ple_gate_mul                                       31k    0%   1    131k   16ms     24
assert_layer_complete  not executed                                                  0
```

Mockup rendered from the iris measurement in §3 (`bench-iris/est-scripts/bars.py`). That
sample replayed only the 68 steps of one 128-step audit window, so 17 of the stage's 28
tiles appear in it. Version 1 replays the top 10 of every tile, so every executed tile gets a
row. The `assert_layer_complete` row shows how a declared but uncalled tile is rendered.

Columns:

| column | meaning | source |
| --- | --- | --- |
| bar, % | heaviest call's user cycles ÷ the largest such value of any tile | §5.4 |
| user cycles | heaviest replayed call | `session.cycles()` |
| seg | segments of that call | `SessionInfo.segments` |
| padded | padded proof cycles of that call, Σ 2^po2 | `SessionInfo.segments[*].po2` |
| exec | measured executor wall time of that call, on this machine | timer around `execute()` |
| calls | all recorded calls of the tile | trace |

**Why cycles drive the bar and time is only a column.** Cycles depend only on the program and
its input, so the bar is the same on every machine. The only time measured is executor time,
and it is machine-specific. A proving-time estimate would need a per-machine calibration
(seconds per segment at a given po2), so it is left for later (§5.6).

**The bar measures the heaviest single call, not the share of the run.** A tile with many
cheap calls can dominate the total while having a short bar. A share-of-the-run view needs
every call replayed, so it is not part of version 1.

Below the bar view, each tile also gets:
- the size histogram of input + output bytes over all its recorded calls, power-of-2 buckets
  (trace witnesses);
- max, p50 and total of user cycles, segments and padded cycles over its replayed calls;
- the heaviest call's coordinate, its bytes, and its byte rank next to its cycle rank.

Because all of this is saved in `replay-profile.json`, `cargo raster replay report` redraws
it without rerunning anything.

Removed: the transition-overhead phase, `REPLAY_INVOCATION_LIMIT`, `CONTEXT_WINDOW_SIZE`, the
"rough cost" formula, and every mention of 128 or windows. The artifact version moves to 4.
Version 1–3 reports exist only from the unmerged branch, so `replay report` does not load
them.

### 5.6 Later, not in version 1

1. **More `replay` subcommands in `replay.rs`.**
   - `replay prove <run-dir> --call <coords>`: really prove one call, such as a tile's
     heaviest, for a true proving time. That calibrates item 5.
   - `replay check <run-dir>`: replay every call, pass/fail only (`zkvm-dry-run`).
2. **Self-check by random sample.** Add calls picked at random with a fixed seed to the top
   10. If one costs more than the top 10, or bytes and cycles disagree in ranking, replay every
   call of that tile. This also catches heavier calls hidden in a size tie (§7 Q2).
3. **Parallel replay.** Calls are independent (`zkvm-dry-run` §10.2).
4. **Heavy-call set.** Save each tile's heaviest calls to re-measure them without a native
   run. Synthetic stress inputs (§4.3) join the same set.
5. **Proving-time column.** Segments × a calibrated seconds-per-segment for the local prover.
6. **Cycles-per-byte fit** and extrapolation to declared size limits (§4.3).
7. **Rows per call site.** Group by CFS call site instead of tile ID, for a tile called from
   several places.
8. **Share-of-the-run bar**, from `replay check`'s full replay or from the cycles-per-byte fit.

### 5.7 Order of work

See §6.

## 6. Implementation plan

**Port, don't rebase.** Start a new branch, `replay-profile`, from main (`1237ec9`), and port
the parts §2 keeps by hand. `origin/risc0-estimator` stays as the reference. A `git rebase`
would replay the transition-overhead work, and then resolve conflicts in 4 files
(`cfs.rs`, `commands.rs`, `storage.rs`, `recorder.rs`) only to delete most of it.

The branch is on risc0 1.2 and main on 3.0.6, so the ported backend code (step 1) is
rewritten against 3.0.6's executor API, not copied.

**No tile image IDs move.** Every change below is in `raster-backend`,
`raster-backend-risc0`, `raster-prover`, `raster-analysis` or `raster-cli`. None of them is
linked into tile guests, and none changes `raster-core` or `raster`. Re-lock hello-tiles at
the end to confirm.

Each step leaves the workspace building with its tests green. Changes stay uncommitted for
review after each step.

### Step 0 — the two "check on main" items (separate from this branch)

1. Port the branch's `recur_draft.rs` assertion (the consumer reads its producer inside the
   same iteration) to main and run it. If it fails, the recur-frame `sequence_depth` fix in
   `raster-runtime/src/storage.rs` is a real bug fix. It goes in its own small change, not in
   this branch.
2. The trace writer's join on a send failure (`publishers/binary.rs`) only improves the error
   message. Same handling: its own change, or dropped.

### Step 1 — executor results carry what the report needs

- `raster-backend/src/backend.rs`, `TileExecutionResult`:
  - `journal: Option<Vec<u8>>`, the raw journal, set in both modes;
  - `segments: Vec<SegmentStat { po2: u32, cycles: u32 }>`, empty for native.
  The constructors set the defaults.
- `raster-backend-risc0/src/risc0.rs`, `execute_tile`:
  - the `Estimate` arm returns an error unless the session exits with `Halted(0)`;
  - it sets `journal` and `segments` from `SessionInfo`;
  - the `Prove` arm sets `journal`;
  - port `with_guest_stdout(false)`, which sends guest stdout to a sink.
- `calculate_proof_cycles` is left alone. Padded cycles are computed from `segments`
  (`zkvm-dry-run` §5.1).
- Tests: the existing backend tests, plus a unit test for the padded-cycle sum.

### Step 2 — `PreparedTileProfile` in `raster-prover/src/replay.rs`

- Port `Replayer::prepare_profile(tile_id)`: discover the tile and compile or load it once.
  It returns `PreparedTileProfile`.
- `PreparedTileProfile::measure(input, expected_output) -> CallMeasure`:
  1. execute in `ExecutionMode::Estimate`, and return an error if a receipt appears;
  2. decode `TileReplayJournal` from `journal`;
  3. check `sha256(input) == input_commitment` and the output bytes;
  4. return `{ user_cycles, segments, padded_cycles, exec_time }`, where `exec_time` is a
     host timer around the `execute_tile` call.
- Port the branch's `profiling_tests`, which use a fake backend and need no zkVM.
- `Replayer::replay`, used by the fraud path, is unchanged.

### Step 3 — `run` writes `run.json`

- New `RunManifest` (serde) in `raster-cli`. It is written next to `trace.bin` by
  authenticated runs only, since `--no-auth` has no trace. It holds:
  - `version: 1`, `run_id`;
  - `project_root`, as an absolute path;
  - `input`, `input_manifest`, as the input reader takes them: a file path made absolute, or
    inline JSON kept verbatim (an inline argument made "absolute" no longer parses);
  - `trace_format`;
  - `cfs_digest`: sha256 of the CFS's JSON, which every run builds anyway. This deliberately
    replaces the program commitment proposed earlier. The commitment needs `program.bin` or
    `Raster.lock`, and a plain `run` may have neither. Tile code changes are already caught
    by the per-call output check.
- Tests: serde round trip, plus the existing run CLI test asserts that the file exists.

### Step 4 — report model and rendering in `raster-analysis/src/replay_profile.rs`

Replace the branch's model with version 4. It holds `kind: "replay-profile"`, `version: 4`,
`run_id`, `complete`, an optional `failure { tile, coordinates, message }`, and a `tiles` map
with **one entry per CFS tile**. Each entry has:
- `calls_recorded`;
- `size_histogram`: counts per power-of-2 bucket of input + output bytes;
- `replayed`: up to 10 entries `{ exec_index, coordinates, input_bytes, output_bytes,
  byte_rank, user_cycles, segments, padded_cycles, exec_ns }`.

Max, p50, totals and cycle ranks are computed when rendering, not stored. `to_text()` draws
the bar view of §5.5, followed by each tile's histogram and details. It is pure code, tested
with fixed fixtures that compare against the expected text. That includes a declared but
uncalled tile, a partial report, and narrow-terminal truncation of long tile names.

### Step 5 — `cargo raster replay` in `raster-cli/src/commands/replay.rs`

- `main.rs`: `Replay { #[command(subcommand)] command: ReplayCommand }`, the same pattern as
  `Chain`. Variants: `Profile { run_dir }` and `Report { path }`.
- `replay profile <run-dir>`:
  1. Read `run.json` and build `Project` from `project_root`.
  2. Rebuild the CFS. Warn if `cfs_digest` differs, then carry on: the per-call checks
     decide.
  3. `load_trace_from_file(...)` with the recorded inputs.
  4. **Select**: one pass over the trace. For each `Tile` step, take its sizes from the
     recorder's witness. Per tile, keep the top `TOP_CALLS_PER_TILE = 10` by in + out bytes,
     ties in trace order. This is a pure function, unit tested: ties, ≤ 10 calls, 0 calls.
  5. **Replay**: tile by tile, `prepare_profile` once, then `measure` each selected call.
     Print the planned replay count first, then progress at most once a second.
  6. Write `replay-profile.json` into the run directory, also on failure, marked partial.
     Print the report. Exit non-zero on failure.
- `replay report <path>`: load the file and print `to_text()`.
- Nothing is added to `run`. The branch's `profiling.rs`, `--tile`, `--profile replay`,
  `--zkvm-profile` and the `analyze` replay reader are not ported. `--profile native` stays
  as it is on main, which spells it `--features profiling`. The branch's `--profile native`
  shorthand is a separate decision and not part of this plan.

### Step 6 — tests and docs

- `raster-cli/tests/replay_profile_cli.rs`, adapted from the branch:
  - not ignored: `replay profile` on a missing or garbage run directory, and
    `replay report` on a fixture JSON;
  - `#[ignore]`, needs the RISC Zero toolchain, about 10 min for the first tile build:
    hello-tiles `run` → `replay profile` → `replay report`. It asserts every CFS tile has a
    row, ≤ 10 replays per tile, and that `report` prints the same text as `profile`.
- Docs: `docs/replay-profiling.md` is rewritten for the new commands, and its link is added
  to the README. The branch's `profiling-workflow.md` and `zkvm-profiling.md` are not ported.
  This proposal's status moves to "implemented".

### Step 7 — verify on iris

1. Rebuild `bench-iris` against the new branch and run `prefill_range_l0` natively.
2. `replay profile` on it. Compare against §3's numbers (cycles per call should match).
3. One-off check: set `TOP_CALLS_PER_TILE` to `usize::MAX` locally and confirm that the
   top 10 contained every tile's true heaviest call. Record the result in §3, then revert.
4. Re-lock hello-tiles and compare against a baseline lock built at the same path.

### Step 7 results (2026-10-30)

Setup: iris `prefill_range_l0`, release `cargo-raster` from this branch, risc0 3.0.6. The iris
copy is `bench-iris/rp/iris`. Its `raster` dependency points at the checkout's real path,
because a symlinked path breaks guest builds (`build-path-dependence` §1).

- **End to end.** A native authenticated run took 248 s, including the stage's first release
  build. `replay profile` replayed 233 of 1,119 calls with status complete, and showed the
  stage's 5 uncalled tiles as `not executed`. That took 919 s, almost all of it compiling 28
  tile guests the first time.
- **Cycles match the earlier measurement exactly.** 20 calls appear both here and in the
  §3 probe measurement. All 20 have identical tile, user cycles and segments. That is
  expected: no guest source changed between the probe's `8c81c47` and this branch.
- **Replaying every call.** With `TOP_CALLS_PER_TILE` temporarily raised, all 1,119 calls
  replayed in **38 s**, single-threaded (§3's estimate was about 40 s).
- **Top 10 by size against every call.** The top 10 held the tile's exact heaviest call for
  **18 of 28** tiles. For the other 10, the top 10's maximum was **99.8–100%** of the true
  maximum. Every miss was a **tie on size**:
  - 9 tiles have 24 calls of one identical size, whose cycles differ by 0.1–1.8%;
  - `mac_weight_page` has 72 calls tied at its largest size, differing by 1.3%.

  Ties keep trace order, so the top 10 are the earliest of the tied calls, and the heaviest
  sat later (byte ranks 13–24). `finish_qkv` looked value-dependent, at 405k–968k for one
  size, but only its first call is cheap; the other 23 are within 0.6% of each other.
  Selecting by size finds the right size class. Within a class it does not choose, and on
  iris that costs at most 0.2%.
- **Image ids.** No guest input changed against main: `raster`, `raster-core` and
  `raster-macros` (the crates guests link), and `guest_builder.rs`, have no diff against
  `7b8230c`. So no tile image id can move. On hello-tiles, the measured ids equal a freshly
  built lock 23/23 (step 5).

## 7. Open questions

1. **Executor fixed cost.** At about 20 ms per invocation it dominates small tiles. Can the
   memory image be built once per tile and reused across executions? If so, a full replay
   gets cheaper by roughly the fixed-cost share (our estimate, not measured).
2. **Ties on size — decided 2026-10-30: keep trace order.** When more than 10 calls tie at a
   tile's largest size, the top 10 are the earliest in trace order. On iris the heaviest tied
   call sat later, costing up to 0.2%. That is documented in `replay-profiling.md`. Rejected
   for version 1, to keep the selection rule simple: breaking ties latest first (a
   heuristic), spreading the picks across the tie, and replaying every call tied at the
   cut-off (exact for size classes, but its cost grows with the tie). The random-sample
   self-check (§5.6) covers ties as well.
3. **Budget unit.** User cycles are what the guest controls. Padded proof cycles are what
   proving pays. A budget in user cycles is simpler to state. A budget in segments matches
   the prover.
