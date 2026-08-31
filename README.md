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
# Any raster payload: a program's output, a chain stage's artifact, a `*.rastered` input
cargo raster show target/raster/runs/latest/output.bin

# Or print it as part of the run itself
cargo raster run --show-output
cargo raster chain run --show-output          # the final stage's value
```

Both report whether what they are showing matches the artifact's own commitment. `--format json`
composes with `jq`; `--max-bytes` / `--max-list` / `--depth` bound what a large artifact prints.
See `docs/proposals/artifact-inspection.md`.

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
