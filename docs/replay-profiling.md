# Replay profiling

Use the replay profiler to measure the first **up to 128 invocations per selected
tile ID** and find the heaviest measured invocation. Replay profiling currently
executes replays in the RISC Zero zkVM:

```sh
cargo raster run --input input.json --input-manifest input_manifest.json \
  --profile replay --tile tile_a,tile_b
```

Tile names are required. The `--tile` flag can also be repeated; duplicate names are
ignored. Unknown names fail before the program is built or run.

This uses the same profiling interface as `--profile native`. For the full
native-discovery-to-replay workflow in a separate program repository, see
[the profiling workflow guide](profiling-workflow.md). The earlier
`--profile zkvm --tile tile_a,tile_b` and `--zkvm-profile tile_a,tile_b`
spellings remain supported for compatibility.

The program runs natively once, recording its normal trace and input/output
witnesses. Raster then executes the **first 128 recorded invocations of each
selected tile ID**, or all of them if there are fewer, once each in RISC Zero
without generating proofs. The cap is per tile ID across the entire run,
including all call sites and recursive iterations. A recursive tile with 1,000
iterations gets 128 replays; a second selected tile has its own budget of 128.
Recursive site records and sequence boundaries do not consume that budget.
Each selected tile is compiled or loaded once, and its executable is reused.
The full native run and trace still include invocations beyond the replay cap.

The report ranks selected tiles by their maximum measured guest cycles, with invocation
counts, total guest cycles, and the heaviest invocation's trace coordinate.
These are guest-user cycles, including guest wrapper work such as serialization
and input hashing. They are not native CPU cycles, padded proving cycles, or an
estimate of proving time. Totals and maxima cover **only the invocations actually
profiled**, without extrapolating to the full run. A completed capped row shows
`calls 128/1000` and `capped (872 beyond limit)`. Later invocations may be heavier
than those measured. Tiles that were selected but never called are marked
`not executed`.

## Sample transition overhead

Replay profiling automatically measures transition-verification overhead once
per executed selected tile, using its **first successfully replayed invocation**.
The existing replay journal is reused; the tile is not executed again. Each row
adds `Sample transition overhead: ... guest cycles at [...] (representative)`.
This coordinate can differ from the heaviest replay's coordinate.

The production transition guest executes its normal continuation path without
proving. It checks the invocation's recorded I/O, storage and selections,
control flow, recursive progress, draft updates, and fingerprint, then commits
a journal. Its guest-user cycles include the transition wrapper and remain
**separate from replay totals**. Authorization executes once per run; its cost
is not included in the per-tile overhead number.

The surrounding proof chain is modeled: context version 1 places the sample in
the second, nonterminal position of a 128-item window. Local state and witnesses
come from the native run, including the recursive stack before the invocation.
Recursive-sequence scope inputs resolve to their recorded storage selections,
while the sampled step and its input bytes remain unchanged. Only the sampled
draft is carried; unrelated draft history is excluded. The
program frame preserves the original CFS, manifest and registry shape, using
the selected tile's real image ID and deterministic placeholders for uncompiled
entries. Its identity is profiling-only and is never saved as `program.bin` or
`Raster.lock`. Executor assumption claims stand in for receipts. No completed
fault window or proof is constructed.

For a tile with 128 successfully profiled invocations, a manual estimate is:

```text
rough cost for 128 measured invocations
  = replay total + 128 × sampled transition overhead
```

For fewer measured invocations, use `profiled_invocations` instead of 128. This
excludes window initialization, authorization execution, non-tile boundary
transitions, and cryptographic proving/composition. Different invocations can
have different overhead; carried window state can also grow. Selected tile
invocations are not necessarily a contiguous fault window. The calculation is
neither a worst-case bound nor a measured complete fault window or proving time.

## Saved reports

Each run writes `replay-profile.json` beside its trace in the printed run-artifact
directory. Reopen it with:

```sh
cargo raster analyze <run-directory>/replay-profile.json
cargo raster analyze <run-directory>/replay-profile.json --format json
```

The version 3 artifact has `kind: "replay-profile"`, `invocation_limit: 128`, and contains the run ID,
completion status, per-tile counts/cycles, executable image IDs, and the
coordinates and execution index of the heaviest invocation. It stores
aggregates rather than a second full list of invocation records. Each tile's
optional `transition_overhead` contains `guest_cycles`, the sampled `invocation`
(coordinates and execution index), `transition_image_id`,
`context: "representative-continuation"`, and `context_version: 1`.

`invocations` remains the full recorded count for each tile;
`profiled_invocations` counts successful replays. `complete: true` means the
requested replays and one overhead measurement per executed selected tile
finished, up to the per-tile replay cap. Reaching the cap is a normal
completion, not a failure or partial report.

`cargo raster analyze` also accepts earlier `zkvm-profile.json` reports with
`kind: "zkvm-profile"`, as well as version 1 replay reports. Those reports have
no invocation limit and retain their original measurements. Version 2 reports
retain their 128-invocation limit. Both older versions show transition overhead
as `not measured`; there is no need to rename or convert existing files.

Missing witnesses, guest execution failures, input-commitment mismatches, or
output mismatches stop profiling and produce a nonzero exit status. A partial
report retains measurements from prior successful invocations and identifies
the failing tile, coordinate, and phase (`replay`, `authorization`, or
`transition_overhead`). If overhead fails after a successful replay, that replay
still contributes to the partial totals; the missing overhead is not zero.
Its counts show completed versus recorded
invocations, and its totals are explicitly marked partial.

## Runtime and compatibility

Additional runtime consists of guest preparation plus executor time for up to
128 invocations per selected tile ID, one transition execution per executed
selected tile, one shared authorization execution, and host witness preparation.
A tile called a million times requires 128 tile replays plus one transition
execution, while its full native trace is still recorded and counted. Replays follow trace order, without repeated benchmarking or
extrapolation. A first run may spend substantial time compiling guests; later
runs can use cached executables.

The profiler requires the RISC Zero guest toolchain when a guest needs building.
It uses the backend's executor selection and does not require proving or GPU
acceleration. Progress counts only the planned replays within the cap and is
printed periodically between invocations. A long
individual invocation can still take substantial time.
Guest standard output is discarded during profiling; guest writes still run
and count toward cycles. Guest standard error remains available for diagnostics.

`--profile replay` supports `--commit` with the usual fraud-proof window setting.
The profiling cap is fixed at 128 per tile ID; changing the commitment's
`--fraud-proof-window-size` does not change it.
It conflicts with `--no-auth` (which records no replay witnesses) and `--audit`.
Normal native execution is unchanged when profiling is not requested. During
profiling, add `--verbose` to retain the ordinary per-item trace printout when
running without `--commit`.

A successful profile confirms matching execution for the selected invocations
on this input. It is not a proof or a measurement of complete fault-window or proof-composition costs.

## Reproducing the timing check

The opt-in integration test profiles ordinary and recursive tiles from
`hello-tiles`, checks their recorded outputs and fingerprint, and prints timings:

```sh
cargo test --release -p raster-cli --test replay_profile_cli -- --ignored --nocapture
```

It measures the native CLI run with trace/fingerprint work and cached native
build checks, guest compilation in a fresh artifact directory, and executor time
for each tile's recorded invocations separately. These small fixtures validate
the profiler; their timings do not predict inference-workload performance.

The transition fixture validates ordinary and recursive tiles (including draft
updates and a tile inside a recursive sequence), rejects corrupted witnesses,
and prints cold compilation, warm replay, host witness preparation, and warm
transition-execution timings separately:

```sh
cargo test -p raster-cli --bin cargo-raster real_transition_overhead \
  -- --ignored --nocapture
```

The transition and authorization guests are bundled when Raster is built;
per-tile cold compilation timings refer to the selected tile's executable.

On this checkout's macOS ARM64 CPU executor (debug host harness, release guests),
the fixture measured native execution at 0.199 seconds and trace loading plus
sample capture at 0.655 seconds. First-invocation measurements were:

- `greet`: 16.756 seconds cold compilation, 0.677 seconds replay, 0.005 seconds
  host witness preparation, and 9.248 seconds transition execution
  (14,517,986 transition guest cycles).
- `build_recur_draft_greeting`: 15.689 seconds cold compilation, 0.722 seconds
  replay, 0.009 seconds preparation, and 11.837 seconds transition execution
  (18,928,146 transition guest cycles).
- `decorate_address_line`, inside a recursive sequence: 15.987 seconds cold
  compilation, 0.680 seconds replay, 0.009 seconds preparation, and 11.701 seconds
  transition execution (18,723,914 transition guest cycles).

Cold preparation used a fresh tile-artifact directory with an installed
toolchain and cached Cargo dependencies. These are individual fixture results,
not repeated benchmarks or a universal slowdown multiplier. The transition cost
is paid once per selected tile in profiling, not for each of its 128 replays.

The implementation also fixes binary CFS serialization of default recursive
fields. JSON CFS output is unchanged. Rebuild the CLI and its bundled guests
together; regenerate program identity artifacts for recursive programs that
were built with the broken, non-round-tripping binary encoding.

Native builds predating the recursive-sequence coordinate fix could also write
child tile outputs under the enclosing loop's iteration counter. The recorded
input bindings then disagreed with the CFS coordinates reconstructed from the
trace, causing transition-overhead preparation to report a missing storage
object or a storage commitment mismatch. Update both the CLI and the program's
Raster dependency, rebuild the native program, and capture a fresh run. Existing
partial reports remain readable, but rebuilding the CLI cannot repair those old
trace bindings.
