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
- `docs/replay-profiling.md`: measure what a run's tiles cost in the RISC Zero zkVM (executor only, no proving)
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

> **Note:** the generated `Cargo.toml` depends on `raster` through
> `path = "../raster/crates/raster"`, which assumes the project is created next
> to the raster checkout (`<dir>/raster` and `<dir>/my-project`). Run
> `cargo raster init` from that parent directory, or edit the path. cargo
> resolves it, so any dependency form works, including one inherited from a
> workspace or reached through a symlink. Tile image ids do not depend on where
> the checkouts live or what their folders are called. They do depend on the
> project's position relative to raster, on the `Cargo.lock`, and on the guest
> compiler (see [path-independent builds](docs/proposals/path-independent-builds.md)).

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

# Measure a run's tiles in the RISC Zero executor (no proving); see docs/replay-profiling.md
cargo raster replay profile target/raster/runs/<run-id>
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

The recording runs on its own thread, alongside the program, for live commits,
live audits and recorded traces. `RASTER_INSTRUMENTATION_THREAD=0` keeps it on
the program's thread, and `RASTER_INSTRUMENTATION_QUEUE` sets how many events
may wait between the two (default 1024). Profiling and selective capture always
record on the program's thread.

Add `--save-trace` to retain `trace.bin` (or `--trace-format json` for
`trace.ndjson`) for debugging, `trace commit`, or `trace audit`. Recording a trace
still builds the commitment live. Plain `run` and deliberate fraud injection retain their recorded-execution paths. Without a saved
trace, later proof generation requires executing the program again.

### Live audit and fault proof

Commitment generation and audit must use matching Raster versions. The compact
coordinate index and explicit fallible-result selections adopted from main change
execution commitments and proof inputs. Regenerate commitments and program locks
from source when upgrading from the earlier index/implicit-selection implementation;
do not reuse its disputed commitments with the new prover. Existing receipts must
be verified with their matching proof-program version.

Live recording and proof preparation use main's compact in-memory coordinate
index directly. Saved-trace workflows may still use disk storage for other data,
but their coordinate indexes also reside in RAM.

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

### Compress a saved fault proof

Convert an already-produced fault receipt into a locally verified Groth16 receipt:

```bash
cargo raster proof compress \
  --receipt /path/to/fault.receipt \
  --commitment /path/to/disputed-commit.bin \
  --output /path/to/new-compression-directory
```

This command works outside a Raster project. It does not execute the program,
rerun audit, or regenerate fault-window proofs. It verifies the input against the
transition verifier embedded in the installed Raster version, checks the finished
fault verdict against the disputed commitment's canonical header digest, compresses
locally, then independently verifies the output and unchanged claim and public
journal. Receipts from incompatible verifier versions are rejected; use their
matching Raster build. Development mode and fake receipts are forbidden.

For composite or succinct inputs, the Groth16 wrapping step requires a working
local Docker engine: `docker info` must succeed. Build Raster with
`--features cuda` to accelerate STARK proof compression on the GPU. RISC Zero's
`cuda` feature also switches the final Groth16 wrapper from Docker to its native
CUDA prover, which hit an illegal CUDA memory access on an RTX 5080. The first
run may download proving assets/container images. Remote proving settings are
not used.
Already-Groth16 inputs are verified and exported without recompression or Docker.


The output parent must exist and the destination must be new. The command writes
`fault.groth16.receipt` and `compression.json` into a unique temporary sibling,
then publishes the directory atomically without replacing an existing path.
Neither input is modified. Cancellation before publication produces no final
output; abrupt process termination may leave a temporary directory to clean up.
Compression can require substantial memory, disk space, and time; no runtime or
memory bound is promised.

The report distinguishes proof-only seal bytes from serialized receipt bytes,
records file hashes, verifier/program/commitment identities, and separates input
verification, compression, and output verification timings. The receipt remains
Raster's Postcard receipt format: it is **not** encoded contract calldata. This
command does not deploy contracts, submit transactions, update Iris/backend/UI
metrics.

`run --audit` and `trace audit` now automatically compress and verify completed
fault proofs before publishing `fault.receipt`. Docker must be available before
fault proving starts. The receipt path and Postcard encoding remain unchanged;
the receipt now contains a Groth16 proof. Compression failures publish no new
fault receipt. Audit reports include `receipt_format` and `proof_seal_bytes`,
and `proof_duration_ms` includes compression and verification. Compression
progress stays within the existing `proving` phase. Intermediate transition
receipts and chain evidence construction are unchanged.
