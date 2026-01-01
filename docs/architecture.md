# Raster Architecture

## Overview

Raster is designed as a compiler toolchain with clear separation between compile-time and runtime concerns. It supports multiple execution backends, including native execution and RISC0 zkVM for generating zero-knowledge proofs.

## Core Concepts

### Tiles

Tiles are isolated compute units marked with the `#[tile]` attribute. Each tile:

- Is compiled into a standalone binary (ELF for zkVM backends)
- Has explicit inputs and outputs serialized via bincode
- Can be executed in isolation (native or zkVM)
- Generates trace events when executed
- Is automatically registered in a global tile registry

Example:
```rust
#[tile(estimated_cycles = 1000, description = "Greets a user")]
fn greet(name: String) -> String {
    format!("Hello, {}!", name)
}
```

### Tile Registry

The tile registry enables runtime discovery of all tiles in a project:

- Uses `linkme` distributed slices for zero-overhead registration
- Each `#[tile]` macro generates a `TileRegistration` entry
- Supports lookup by ID: `find_tile_by_str("greet")`
- Works in both host and RISC0 guest builds

### Tile ABI

All tiles use a stable ABI based on bincode serialization:

- Inputs are serialized as bincode (single value or tuple for multiple args)
- Outputs are serialized as bincode
- The generated wrapper `__raster_tile_entry_<name>` handles serialization
- This enables execution via registry without compile-time type knowledge

### Sequences

Sequences describe control flow between tiles using the `#[sequence]` attribute. Each sequence:

- Compiles into a control-flow schema (not an executable)
- Describes tile ordering and branching
- Can be validated against actual execution traces
- Serves as documentation and verification artifact

### Backends

Backends implement the `Backend` trait and provide:

- `compile_tile` - Compile source to binary/ELF
- `execute_tile` - Execute with optional proving
- `estimate_resources` - Cycle and memory estimation
- `verify_receipt` - Verify a proof receipt

Available backends:
- **Native**: In-process execution, no proofs
- **RISC0**: zkVM execution with optional proving

### Execution Modes

The backend supports different execution modes:

- **Estimate**: Execute without proof, return cycle count
- **Prove**: Execute and generate a proof receipt
- **Prove+Verify**: Generate and verify the proof

### Traces

Execution traces are first-class artifacts that record:

- Every tile invocation (ID, timestamp, depth)
- Input/output data (or hashes)
- Execution duration and cycle estimates
- Sequence schema compliance

## RISC0 Backend

### Overview

The RISC0 backend (`raster-backend-risc0`) compiles tiles to RISC0 guest programs and executes them in the zkVM.

### Artifact Format

Artifacts are written to `target/raster/tiles/<tile_id>/risc0/`:

```
target/raster/tiles/greet/risc0/
├── guest.elf       # Compiled RISC0 guest binary
├── method_id       # Hex-encoded image ID
└── manifest.json   # Tile metadata and build info
```

### Guest Program Structure

Each tile is wrapped in a minimal guest program:

```rust
#![no_main]
#![no_std]

use risc0_zkvm::guest::env;

risc0_zkvm::guest::entry!(main);

fn main() {
    // Read input bytes from host
    let input: Vec<u8> = env::read();

    // Execute tile via registry
    let tile = find_tile_by_str("greet").unwrap();
    let output = tile.execute(&input).unwrap();

    // Commit output to journal
    env::commit_slice(&output);
}
```

### Execution Pipeline

1. **Build**: Generate guest crate → Compile to RISC0 target → Extract ELF
2. **Estimate**: Run in executor without proving → Get cycle count
3. **Prove**: Run prover → Generate receipt with journal
4. **Verify**: Verify receipt against image ID

### CLI Usage

```bash
# Build tiles for RISC0
cargo raster build --backend risc0

# Run in estimate mode (default, no proof)
cargo raster run --backend risc0 --tile greet --input '"World"'

# Run with proof generation
cargo raster run --backend risc0 --tile greet --input '"World"' --prove

# Run with proof and verification
cargo raster run --backend risc0 --tile greet --input '"World"' --prove --verify
```

## Dependency Flow

```
raster-core (types, registry, ABI)
    ↓
    ├─ raster-macros (proc macros for #[tile])
    ├─ raster-backend (Backend trait)
    │       ↓
    │       ├─ raster-backend-risc0 (RISC0 implementation)
    │       ├─ raster-compiler (build orchestration)
    │       └─ raster-runtime (execution + tracing)
    │               ↓
    │               └─ raster-analysis (metrics)
    │                       ↓
    └───────────────────────┴─ raster-cli (cargo raster)
```

## Build Process

1. Developer writes tiles and sequences
2. `cargo raster build [--backend risc0]` invokes the compiler:
   - Discovers tiles via macro-generated registry
   - For each tile, generates a guest crate (RISC0)
   - Compiles to target architecture
   - Computes method ID from ELF
   - Writes artifacts to `target/raster/tiles/<id>/<backend>/`

## Execution Process

1. `cargo raster run --tile <id> [--prove] [--verify]`:
   - Loads compiled artifacts
   - Prepares input via bincode serialization
   - Executes in selected mode (estimate/prove)
   - Reports cycles, output, and proof status
   - Optionally verifies the generated receipt

## Analysis Process

1. `cargo raster analyze` reads traces:
   - Extracts per-tile metrics (invocations, duration, cycles)
   - Identifies bottlenecks
   - Estimates zkVM costs
   - Suggests optimizations (tile splits/merges)

## Extension Points

### New Backends

Implement the `Backend` trait in a new crate:

```rust
impl Backend for CustomBackend {
    fn name(&self) -> &'static str { "custom" }

    fn compile_tile(&self, metadata: &TileMetadata, source: &str)
        -> Result<CompilationOutput> { ... }

    fn execute_tile(&self, compilation: &CompilationOutput, input: &[u8], mode: ExecutionMode)
        -> Result<TileExecution> { ... }

    fn estimate_resources(&self, metadata: &TileMetadata)
        -> Result<ResourceEstimate> { ... }

    fn verify_receipt(&self, compilation: &CompilationOutput, receipt: &[u8])
        -> Result<bool> { ... }
}
```

### Custom Tracers

Implement the `Tracer` trait for specialized tracing:

```rust
impl Tracer for CustomTracer {
    fn record_event(&mut self, event: TraceEvent) -> Result<()> { ... }
    fn finalize(self) -> Result<Option<Trace>> { ... }
}
```

### Analysis Extensions

Build on `raster-analysis` to add:

- Custom cost models
- Visualization tools
- Optimization suggestions
- Schema validators

## Security Considerations

- Tiles execute in isolation (sandboxed in zkVM)
- Inputs/outputs are explicitly serialized
- Proofs are cryptographically verified
- No ambient capabilities in guest programs
