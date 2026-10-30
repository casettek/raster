# Replay profiling

Replay profiling measures what a program's tiles cost in the RISC Zero zkVM. It replays recorded
tile calls in the RISC Zero **executor**: the guest runs, nothing is proved. For each tile it
reports guest cycles, segments, and padded proof cycles, the size the prover would pay for.
Design and measurements: [`proposals/tile-cost-estimator.md`](proposals/tile-cost-estimator.md).

## Usage

```bash
# 1. Any authenticated run: it records every tile call's exact input and output bytes.
cargo raster run --input input.json --input-manifest input_manifest.json

# 2. Measure the run printed as "Run artifacts dir".
cargo raster replay profile target/raster/runs/<run-id>

# 3. Print the saved result again, without rerunning anything.
cargo raster replay report target/raster/runs/<run-id>
```

Requirements:
- **A completed authenticated run.** `run` writes `run.json` into the run directory only when the
  program completed. `run.json` records the input files and the CFS digest. `--no-auth` runs
  record no trace, and failed runs are incomplete, so neither can be profiled.
- **The RISC Zero guest toolchain.** Each tile's guest is compiled on first use and cached, the
  same as for `cargo raster build --backend risc0`.
- **A release build of `cargo-raster` for meaningful times.** The executor runs inside the
  `cargo-raster` process. A debug build is about 20 times slower: on hello-tiles, 290–420 ms
  per call against 15–22 ms in release. The report says when a debug build measured it. Cycle
  counts do not depend on the build.

## What is replayed

For each tile, its **10 largest recorded calls by input + output bytes**. A tile with 10 calls
or fewer is replayed in full. Every call is still counted in that tile's size histogram.

Size is the selection key because it predicts zkVM cost: guest cycles are dominated by decoding
and hashing the input. On iris `prefill_range_l0`, ranking by size found the true 10 heaviest
calls and every tile's heaviest call. Native time was a weaker predictor
(tile-cost-estimator §3).

The limit: a tile whose cost depends on its *values* rather than its size can have its heaviest
call outside the top 10. The report shows each heaviest call's byte rank next to its cycle rank,
so a disagreement is visible.

Every replay must reproduce the recording: the guest's input commitment must be
`sha256(input)`, and its output must equal the recorded output. A call that does not, for
example because the tile's code changed since the run, stops profiling.

## The report

`replay-profile.json` is written into the run directory and printed. Real output on
`examples/hello-tiles`, measured by a release build of `cargo-raster` (abridged):

```text
Replay profile — RISC Zero executor, no proving
  Run: 00000000001793385803-803200584-pid1401085-000000
  Status: complete
  Calls: top 10 per tile by input + output bytes; 53 of 58 replayed
  Cycles are guest user cycles, including the tile wrapper (input decode and hash, output encode).
  Exec: executor wall time on the measuring machine, release build.

tile                          heaviest call, user cycles (% of max)  seg  padded   exec  calls
build_limited_recur_greeting  ████████████████████████    38k  100%    1    131k   20ms      2
build_greeting                ███████████████████████▌    37k   98%    1    131k   22ms      1
…
exclaim                       █████████▏                  15k   38%    1     66k   15ms     15
…
count_to                      not executed                                                   0

exclaim — 15 calls, 10 replayed, image 16184ce77790… (Raster.lock has 9d5561f884ac…)
  in+out bytes  ≤ 32 B     ███▎                      1
                ≤ 64 B     ██████████                3
                ≤ 128 B    ███▎                      1
                ≤ 256 B    ████████████████████      6
                ≤ 512 B    █████████████▎            4
  user cycles   max 15k       p50 12k       total 131k
  segments      max 1         p50 1         total 10
  padded        max 66k       p50 66k       total 655k
  heaviest      [23, 7, 4, 1] (exec 108), 320 B in+out, byte rank 1, cycle rank 1
```

- **One row per tile the CFS declares.** Tiles that never ran say `not executed`. Tiles that ran
  but were not reached because profiling stopped say `not replayed`.
- **The bar** is each tile's heaviest replayed call as a share of the most expensive tile's. It
  compares single calls, not totals: a tile with many cheap calls can cost a lot overall and
  still have a short bar.
- **`seg`, `padded`, `exec`** belong to that heaviest call. `padded` is `Σ 2^po2` over its
  segments, because each segment is proven at its own power-of-two size.
- **Per-tile detail:** the size histogram over *all* recorded calls; max, median and total over
  the replayed calls; and the heaviest call's coordinates with its byte rank and cycle rank.
- **Image ids:** each tile's measured image id is compared with `Raster.lock` when the project
  has one. A mismatch means the lock does not describe what this checkout builds. The example
  above shows one: the committed hello-tiles lock is stale. Image ids also depend on the
  checkout path; see [`issues/build-path-dependence.md`](issues/build-path-dependence.md).

If a replay fails, the report is still saved, marked `PARTIAL`, with the failing tile,
coordinates and reason, and the command exits non-zero.

## What it does not measure

- **Proving time.** Cycles and segments determine proving work. The time per segment depends on
  the machine and prover.
- **Calls outside each tile's top 10.**
- **The proof chain around tile calls:** transition guests, authorization, and fraud-proof
  windows.
