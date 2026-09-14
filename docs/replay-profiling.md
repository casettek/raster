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

## Saved reports

Each run writes `replay-profile.json` beside its trace in the printed run-artifact
directory. Reopen it with:

```sh
cargo raster analyze <run-directory>/replay-profile.json
cargo raster analyze <run-directory>/replay-profile.json --format json
```

The version 2 artifact has `kind: "replay-profile"`, `invocation_limit: 128`, and contains the run ID,
completion status, per-tile counts/cycles, executable image IDs, and the
coordinates and execution index of the heaviest invocation. It stores
aggregates rather than a second full list of invocation records.

`invocations` remains the full recorded count for each tile;
`profiled_invocations` counts successful replays. `complete: true` means the
requested replays finished, up to the per-tile cap. Reaching the cap is a normal
completion, not a failure or partial report.

`cargo raster analyze` also accepts earlier `zkvm-profile.json` reports with
`kind: "zkvm-profile"`, as well as version 1 replay reports. Those reports have
no invocation limit and retain their original measurements; there is no need
to rename or convert existing files.

Missing witnesses, guest execution failures, input-commitment mismatches, or
output mismatches stop profiling and produce a nonzero exit status. A partial
report retains measurements from prior successful invocations and identifies
the failing tile and coordinate. Its counts show completed versus recorded
invocations, and its totals are explicitly marked partial.

## Runtime and compatibility

Additional runtime consists of guest preparation plus executor time for up to
128 invocations per selected tile ID. A tile called a million times requires
only 128 guest executions, while its full native trace is still recorded and
counted. Replays follow trace order, without repeated benchmarking or
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
on this input. It is not a proof or a measurement of authorization, transition,
or proof-composition costs.

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
