# Raster

A Rust-based developer toolchain for building and analyzing tile-based execution systems.

## Overview

Raster enables developers to:

- **Write large Rust programs** composed of **tiles** (isolated compute units)
- **Group tiles into sequences** (control-flow descriptions)
- **Automatically compile**:
  - Standalone tile binaries (for zkVM / isolated execution)
  - Control-flow schemas describing tile ordering
  - Native execution binaries with optional tracing
- **Analyze execution characteristics** to tune:
  - Tile sizing (cycle counts, zkVM cost)
  - Trace sizes
  - Smart contract parameters

## Architecture

Raster is organized as a Rust workspace with the following crates:

- **`raster-core`**: Foundation types (tile IDs, schemas, manifests, traces)
- **`raster-macros`**: Procedural macros (`#[tile]`, `#[sequence]`)
- **`raster-backend`**: Backend abstraction layer (native + future zkVM backends)
- **`raster-compiler`**: Build orchestration (tile compilation, schema generation)
- **`raster-runtime`**: Native execution with optional tracing
- **`raster-analysis`**: Profiling and metrics extraction
- **`raster-cli`**: Command-line tool (`cargo raster`)
- **`raster`**: Convenience re-export crate for user applications

## Documentation

- `docs/architecture.md`: technical architecture notes
- `docs/tile-authoring.md`: authoring guide for tiles/sequences
- `docs/specs/README.md`: canonical, code-adjacent specs

## Installation

```bash
cargo install raster-cli
```

Native builds launched by `cargo raster run` and `cargo raster chain run`
automatically configure Windows MSVC with `/DNOMINMAX /std:c++20 /EHsc` for
the RISC Zero host dependencies. Existing compiler flags are retained, with
these required flags appended. Native Windows builds also default
`RISC0_SKIP_BUILD` to `1` (unless explicitly set), since replay does not need
RISC-V proof guests. Executable names use the host's suffix (`.exe` on Windows).
macOS and Linux retain their existing build environment. Windows still requires
the Visual Studio C++ build tools. This setup applies to child builds launched
by the installed CLI, not to bootstrapping the CLI itself with Cargo.

## Quick Start

### 1. Initialize a new project

```bash
cargo raster init my-project
cd my-project
```

### 2. Write tiles

```rust
use raster::prelude::*;

#[tile]
fn compute(input: u64) -> u64 {
    input * 2
}

#[tile]
fn process(value: u64) -> u64 {
    value + 10
}
```

### 3. Define a sequence

```rust
#[sequence]
fn my_workflow() {
    let x = compute(5);
    let y = process(x);
}
```

### 4. Build and run

```bash
# Build tiles and schemas
cargo raster build

# Run with tracing (default)
cargo raster run

# Run without tracing
cargo raster run --no-trace

# Analyze execution
cargo raster analyze
```

### 5. Read what it produced

A run's result is an artifact on disk (`output.bin` plus its `.rindex`), not printed output.
`show` decodes one back into a typed, structured value:

```bash
# Any raster payload: a program's output, a chain stage's artifact, a `*.rastered` input.
# `run` prints the run directory it wrote; a chain run also links `latest`.
cargo raster show target/raster/runs/<run-id>/output.bin
cargo raster show target/raster/chains/latest/<stage>/output.bin

# Or print it as part of the run itself, so there is no path to look up
cargo raster run --show-output
cargo raster chain run --show-output          # the final stage's value
```

Both report whether what they are showing matches the artifact's own commitment; `show` renders
the value either way but exits non-zero when it does not match, so a script can tell. `--format
json` composes with `jq` — the document goes to stdout and any integrity warning to stderr.
`--max-bytes` / `--max-list` / `--max-fields` / `--depth` bound what a large artifact prints, and
truncation is always stated rather than silent. See `docs/proposals/artifact-inspection.md`.

## Design Principles

- **Intentionally simple**: No cryptography or zkVM logic in v0
- **No automatic partitioning**: Tiles are explicitly defined by developers
- **Trace-first execution**: Native runs produce detailed execution traces
- **Backend pluggability**: Future zkVM backends integrate via the `Backend` trait

## Status

v0 - Initial implementation in progress. Core functionality includes:

- ✅ Repository structure
- ⏳ Tile and sequence macros
- ⏳ Native backend
- ⏳ Build orchestration
- ⏳ Execution tracing
- ⏳ Analysis tools
- ⏳ CLI tooling


at your option.

### Live commitments and trace retention

`cargo raster run --commit commit.bin --fraud-proof-window-size 128` builds the
commitment as the program emits events. Authenticated `cargo raster chain run`
uses the same path for every stage. Add the usual `--input` and `--input-manifest`
arguments when the program needs them. General window defaults are unchanged.

These runs keep working objects, indexes, and fingerprint blocks in RAM; they do
not spill to disk. Memory use can grow with the program. The finished commitment
is published only after successful execution. Normal output artifacts are still
written. No raw trace is saved by default.

Add `--save-trace` to retain `trace.bin` (or `--trace-format json` for
`trace.ndjson`) for debugging, `trace commit`, or `trace audit`. Recording a trace
still builds the commitment live. Plain `run` and deliberate fraud injection retain their recorded-execution paths. Without a saved
trace, later proof generation requires executing the program again.

### Live audit and fault proof

From a Raster program directory, run:

```sh
cargo raster run --input input.json --input-manifest input_manifest.json \
  --audit disputed.bin --proof-output fault.receipt --report audit.json
```

The supplied commitment determines the window size. Execution compares each
step as it happens and stops at the first detected mismatch; Raster then proves
and verifies that window. It creates neither another commitment nor a raw trace.
The report distinguishes `proved` from `no-fault`. An honest audit runs through
the final boundary and checks the complete length. Failed or interrupted native
execution is never treated as divergence.

Audit working state stays in RAM and may grow with the program. A temporary
proof-evidence artifact connects native execution to the prover; proof generation
may use its normal working files. Proof and report destinations default to the
run artifact directory. Saved-trace commands remain available for existing files,
but live audit does not invoke them. Retrying a failed live proof reruns the audit.
