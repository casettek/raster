# Replay profiling

The profiler is now called **replay profiling**. See the
[replay profiling guide](replay-profiling.md) and the
[native-to-replay workflow](profiling-workflow.md).

Use `cargo raster run --profile replay --tile tile_a,tile_b`.
The earlier `--profile zkvm --tile ...` and `--zkvm-profile ...` commands remain
compatible, and `cargo raster analyze` still reads existing `zkvm-profile.json`
reports. New runs write `replay-profile.json`.
