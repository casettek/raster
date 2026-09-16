# From native profiling to replay profiling

Use native profiling to find expensive tiles on a representative workload, then
measure up to 128 invocations per selected tile ID with replay profiling. Replays
currently execute in the RISC Zero zkVM. Both modes use
`cargo raster run --profile <type>` and save reports that you reopen with
`cargo raster analyze <path>`.

This guide is for an existing Raster program in a separate repository. Run the
commands from the program's package directory containing its `Cargo.toml`, not
from the Raster toolchain repository or a virtual Cargo workspace root.

## 1. Use the matching Raster CLI and dependency

Install the CLI from the Raster checkout containing this feature:

```sh
cargo install --path /path/to/raster/crates/raster-cli --locked --force
cd /path/to/my-raster-program
cargo raster run --help
```

The help should list `--profile` with `native` and `replay` choices. Keep the CLI
and your program's Raster dependency on the same checkout/revision.

For a local checkout, the relevant parts of your program's `Cargo.toml` look
like this; merge these entries with your existing features and dependencies:

```toml
[features]
default = ["std"]
std = ["raster/std"]

[dependencies]
raster = { path = "../raster/crates/raster", default-features = false }
```

Adjust the dependency path to your checkout. This direct path dependency also
lets the current guest builder locate Raster when it compiles your program's
tiles. Keep the program's `std` feature enabled for native execution. You do not
need to add a feature named `profiling`: `--profile native` enables
`raster/profiling` automatically.

The program should already run with Raster's `#[sequence]` entry point. For
zkVM replay, expose its tiles from the library target (`src/lib.rs`) and make
that library compile with default features disabled for the guest. The guest
builder imports each macro-generated replay entry from the crate root. If tiles
live in a module, re-export that module's public items from `lib.rs` (for example,
`pub use tiles::*;`) so their replay entries are available too. A program with
tiles only in `src/main.rs` needs to expose them through a library first.

When your binary uses the library's tiles, import their generated call bindings
too. The usual pattern is `use my_program::*;` rather than importing only the
individual tile function names (replace `my_program` with your Rust crate name).

Guest compilation also needs the RISC Zero Rust toolchain. If it is missing,
Raster reports the installation command, `rzup install`. Use a toolchain
compatible with your Raster checkout. Native profiling itself does not compile
tile guests.

## 2. Profile the native run

Use your program's usual input and matching public input manifest:

```sh
cargo raster run \
  --input input.json --input-manifest input_manifest.json \
  --profile native
```

Omit the input flags if your program has no external inputs. Include your usual
`--features` flags if needed; profiling preserves them. The program is built in
release mode and executes once with normal authenticated storage and tracing.
Each tile invocation is timed as it happens, including recursive iterations.
There is no replay or repeated benchmarking in this step.

The terminal prints a `Profile Summary` with `Type: native`, time in tile
function bodies versus Raster bookkeeping, and `Hot Tiles`.
The current native hot list contains the top three tiles by **total native
duration, including Raster overhead**, with total time, average time, and call
count. It is not sorted by the slowest individual invocation. The summary also
has a `Program total` field; the existing runtime can leave that field `pending`
when it has not recorded a whole-program duration, even in a saved report. Tile
measurements are still available in that case.

The run prints the paths to `profile.json` and the live `profile.ndjson` stream
under `target/raster/runs/<run-id>/`. Use the actual path printed by your run:

```sh
cargo raster analyze target/raster/runs/<native-run-id>/profile.json
```

During a running native profile, another terminal can show the live report:

```sh
cargo raster analyze --follow target/raster/runs/<native-run-id>/profile.ndjson
```

Both profiling modes suppress the per-item trace printout by default. Add
`--verbose` if you need those coordinates in the terminal; the trace artifact
is still recorded either way.

## 3. Pick tiles to investigate

Start with names in `Hot Tiles`. A large total identifies a tile that dominates
the workload; a large average suggests expensive individual invocations. Check
the call count too: many small calls can produce a large native total. Replay
profiling measures only the first 128 invocations per selected tile ID.

To inspect more than three tiles, or distinguish tile body time from Raster
overhead for each tile, export the native analysis:

```sh
cargo raster analyze target/raster/runs/<native-run-id>/profile.json \
  --format json > native-metrics.json
```

The `tile_metrics` map includes every executed tile. Its fields include
`invocations`, `total_duration_ns`, `avg_duration_ns`, `total_user_duration_ns`,
`avg_user_duration_ns`, and `total_raster_overhead_ns`. For example, with `jq`
installed, print ten candidates ranked by time in their Rust function bodies:

```sh
jq '.tile_metrics | to_entries
    | sort_by(.value.total_user_duration_ns) | reverse | .[:10]
    | map({tile: .key, calls: .value.invocations,
           total_body_ns: .value.total_user_duration_ns,
           average_body_ns: .value.avg_user_duration_ns,
           raster_overhead_ns: .value.total_raster_overhead_ns})' native-metrics.json
```

Use the exact reported tile names. `cargo raster list` also lists the project's
available tiles. Native elapsed times are clues for choosing candidates, not
hardware cycle counts or estimates of zkVM cycles; native and guest costs can
rank differently.

## 4. Profile the selected tile replays

Replace `tile_a,tile_b` with the names you selected. Use the same workload and
relevant program feature flags as the native discovery run:

```sh
cargo raster run \
  --input input.json --input-manifest input_manifest.json \
  --profile replay --tile tile_a,tile_b
```

You can repeat `--tile`, for example `--tile tile_a --tile tile_b`. Duplicate
names are ignored. Unknown names fail before the program is built or executed.
`--profile replay` requires explicit tile names; `--profile native` profiles all
native invocations and does not take `--tile`.

This is a **new native run**, followed by one RISC Zero execution of each of the
**first 128 recorded invocations per selected tile ID**, or all invocations if
there are fewer. It does not replay the earlier
native run's saved profile. Each guest receives that invocation's recorded
input, and its output and input commitment are checked against the native run.
No proofs are generated.

A selected recursive tile shares the same **128-invocation budget across every
call site and iteration**. For 1,000 iterations, only the first 128 are replayed.
The cap applies independently to each selected tile ID, including ordinary
tiles. Its enclosing recursive site is not an extra replay. Each selected
tile's executable is prepared once per run and reused; cached builds can avoid
compiling it again on subsequent runs.

The `Profile Summary` now says `Type: replay` and ranks the selected tiles by
**maximum measured guest cycles per invocation**. Each row includes maximum cycles,
total cycles, successful/recorded invocation counts, and the heaviest
invocation's trace coordinate. A capped tile shows `calls 128/1000` and
`capped (872 beyond limit)`; reaching the cap counts as successful completion.
An unused selected tile says `not executed`. Cycles include guest wrapper work.
Each executed tile also shows **sample transition overhead**, measured once from
its first replayed invocation, with that sample's coordinate. The tile is not
replayed again for this measurement. Overhead is separate from replay cycles
and uses a representative continuation context; it is not a complete fault
window measurement.
Totals and maxima cover only the profiled invocations, without extrapolating
to later invocations or the whole program.

The printed artifact is `replay-profile.json`:

```sh
cargo raster analyze target/raster/runs/<replay-run-id>/replay-profile.json
cargo raster analyze target/raster/runs/<replay-run-id>/replay-profile.json \
  --format json > replay-metrics.json
```

Use the maximum to assess the heaviest replay among the measured invocations.
Later invocations may be heavier. Use the count to see coverage and the cycle
total to assess the measured portion of the workload. After
changing tile boundaries, repeat this workflow on the same input. These
measurements describe execution cycles, not proving latency.

For a rough estimate of replay plus transition checking for 128 measured calls:

```text
rough cost for 128 measured invocations
  = replay total + 128 × sampled transition overhead
```

Use the actual measured count when it is below 128. This excludes window
initialization, authorization execution, non-tile boundary transitions, and
cryptographic proving/composition. The sample uses real recorded witnesses and
recursive state with modeled preceding proof-chain state. Overhead can vary
between invocations and with accumulated state, so this is not a worst-case
bound. The selected calls also need not form a contiguous fault window.

## Runtime and compatibility

Native profiling adds timing and record-keeping overhead to the native run.
The existing native profiler retains individual records, so its memory and
report size grow with invocation count; it is not an aggregate-only mode.

Replay profiling adds guest preparation and up to 128 replays per selected
tile ID, one transition execution per executed selected tile, one shared
authorization execution, and host witness preparation. A tile called a million
times causes only 128 tile replays plus one transition execution. The native
program still executes fully, and its entire trace is recorded and counted.
Progress counts the planned replays within the cap and is printed at most once
per second between replays; replay profiles have no live NDJSON follow mode.

Both `--profile` modes require authenticated execution and reject `--no-auth`.
Replay profiling also rejects `--audit`. Both support `--commit` with its required
`--fraud-proof-window-size`; a saved trace commitment is optional for profiling.
The profiling cap stays at 128 per tile ID regardless of that window setting.
If replay, authorization, or transition-overhead execution fails or mismatches, the command exits nonzero and saves a
clearly marked partial report with the failing tile and coordinate. Partial
totals cover only previously successful replays.

Existing commands still work: `--features raster/profiling` enables native
profiling. Both `--profile zkvm --tile tile_a,tile_b` and
`--zkvm-profile tile_a,tile_b` are compatibility spellings of
`--profile replay --tile tile_a,tile_b`. Existing `zkvm-profile.json` reports
still open with `cargo raster analyze`. Projects that already forward a
`profiling` feature can continue using `--features profiling` too.

New replay reports use version 3 with `invocation_limit: 128` and a per-tile
`transition_overhead` measurement. Versions 1 and 2 remain readable with overhead
shown as `not measured`; version 1 retains its original uncapped measurements.
A failure records its phase, tile, and coordinate. If the replay succeeded before
an overhead failure, its cycles remain in the clearly marked partial report.

When updating Raster, rebuild both the CLI and the consuming program against the
same revision. In particular, older native builds assigned incorrect storage
coordinates to child tiles inside recursive sequences. Those traces can fail
transition-overhead preparation even when individual tile replays succeed; they
need a fresh native run after rebuilding.

See [replay profiling details](replay-profiling.md) for the artifact fields and
executor validation boundaries.
