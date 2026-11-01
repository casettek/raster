# Native-guided selective profiling

Use authenticated live commitment execution to select and capture a small number
of calls without writing full traces:

```sh
cargo raster run --commit timing.commit.bin --fraud-proof-window-size 128 --profile-native-top --profile-top-tiles 3 --profile-calls-per-tile 1 --input input.json --input-manifest input_manifest.json
cargo raster run --commit capture.commit.bin --fraud-proof-window-size 128 --capture-profile-calls /exact/timing/run/native-selection.json --sample-max-bytes 8589934592 --input input.json --input-manifest input_manifest.json
cargo raster replay profile /exact/capture/run
```

Use the run directory printed by each command, never the newest global run.
Native timing automatically enables the program's profiling feature. It retains
only K slowest calls per tile, then selects N tiles by their maximum native time.
Ties prefer the earlier execution index. Native totals include wrapper and final
output-store work. Tile ID, coordinates and execution index identify a call;
profiling invocation counters are not commitment indices.

Capture runs the entire program and retains payloads only for selected calls.
Every selection must match once. Both modes require --commit and disallow
--no-auth, --audit and --save-trace. The original execution/proof formats do not
change. Native runtime only guides selection; measured cycles determine the final
ordering and may differ from native ordering.

The 8 GiB default payload budget fails explicitly when exceeded, with a 2 GiB
free-space reserve. No raw trace, historical witness archive or unbounded native
profile stream is created. Live runtime state is still required. Samples are
stored as individual input/output files under samples/ with hashed metadata.
Standalone runs retain them; Iris workers delete them after saving reports.

Successful runs publish run.json version 2 with a replay_source identifying the
selection/capture manifest. The capture manifest is bound to program, inputs,
output and canonical commitment. replay profile verifies payload hashes and guest
image IDs before measurement and writes report version 5, with selection policy
native-runtime and original native timings. Version-1 trace runs and version-4
reports remain supported. They continue to use the original size-based sampler.
