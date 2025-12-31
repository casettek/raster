# Raster Architecture

## Overview

Raster is designed as a compiler toolchain with clear separation between compile-time and runtime concerns.

## Core Concepts

### Tiles

Tiles are isolated compute units marked with the `#[tile]` attribute. Each tile:

- Is compiled into a standalone binary
- Has explicit inputs and outputs
- Can be executed in isolation (native or zkVM)
- Generates trace events when executed

### Sequences

Sequences describe control flow between tiles using the `#[sequence]` attribute. Each sequence:

- Compiles into a control-flow schema (not an executable)
- Describes tile ordering and branching
- Can be validated against actual execution traces
- Serves as documentation and verification artifact

### Backends

Backends implement the `Backend` trait and provide:

- Tile compilation (source → binary)
- Tile execution (binary + input → output)
- Resource estimation (cycles, memory)

The native backend executes tiles in-process. Future zkVM backends will produce equivalent functionality for different execution environments.

### Traces

Execution traces are first-class artifacts that record:

- Every tile invocation (ID, timestamp, depth)
- Input/output data (or hashes)
- Execution duration and cycle estimates
- Sequence schema compliance

Traces enable analysis, cost estimation, and debugging.

## Dependency Flow

```
raster-core (types only)
    ↓
    ├─ raster-macros (proc macros)
    ├─ raster-backend (abstraction + native impl)
    │       ↓
    │       ├─ raster-compiler (build orchestration)
    │       └─ raster-runtime (execution + tracing)
    │               ↓
    │               └─ raster-analysis (metrics)
    │                       ↓
    └───────────────────────┴─ raster-cli (user interface)
```

## Build Process

1. Developer writes tiles and sequences
2. `cargo raster build` invokes the compiler:
   - Discovers tiles via macro-generated metadata
   - Compiles each tile to a standalone binary
   - Generates sequence schemas from control flow
   - Writes artifacts to `target/raster/`

## Execution Process

1. `cargo raster run` invokes the runtime:
   - Loads sequence schema
   - Executes tiles in order (native backend)
   - Records trace events (optional but default)
   - Writes trace to `target/raster/traces/`

## Analysis Process

1. `cargo raster analyze` reads traces:
   - Extracts per-tile metrics (invocations, duration, cycles)
   - Identifies bottlenecks
   - Estimates zkVM costs
   - Suggests optimizations (tile splits/merges)

## Extension Points

### New Backends

Implement the `Backend` trait in a new crate (e.g., `raster-backend-risc0`):

```rust
impl Backend for Risc0Backend {
    fn compile_tile(&self, ...) -> Result<Vec<u8>> { ... }
    fn execute_tile(&self, ...) -> Result<Vec<u8>> { ... }
    fn estimate_resources(&self, ...) -> Result<ResourceEstimate> { ... }
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
