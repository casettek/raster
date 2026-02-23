## Developer API Surface (as implemented)

This document describes the Rust API surface area exposed to **application authors** (people writing tiles and sequences) and to **tooling authors** (people integrating compilation/execution backends), as it exists in the current codebase.

The primary entry point for applications is the `raster` crate. Other crates (`raster-core`, `raster-macros`, `raster-compiler`, `raster-backend`, `raster-runtime`, `raster-backend-risc0`) are also public, but several are incomplete and should be treated as unstable/implementation-facing unless explicitly listed as stable below.

---

## Code audit tasks (where to look)

- **Top-level user crate exports**
  - `crates/raster/src/lib.rs`: top-level re-exports and `raster::prelude`.
  - `crates/raster/Cargo.toml`: feature flags (`std`, `alloc`) and what they gate.

- **Macros (authoring surface)**
  - `crates/raster-macros/src/lib.rs`: `#[tile]` and `#[sequence]` proc macros; generated wrapper functions/macros; registry integration; ABI serialization via `postcard`.

- **Core data types and registries**
  - `crates/raster-core/src/lib.rs`: module gating by features/targets; re-exports (`Error`, `Result`, `postcard`, `linkme`).
  - `crates/raster-core/src/tile.rs`: `TileId`, `TileMetadata`, and static variants used for registration.
  - `crates/raster-core/src/registry.rs`: `TileRegistration`, `SequenceRegistration`, and discovery helpers (`iter_tiles`, `find_tile_*`, etc.).
  - `crates/raster-core/src/schema.rs`: `SequenceSchema` and `ControlFlow` (note: schema generation is currently not implemented).
  - `crates/raster-core/src/cfs.rs`: `ControlFlowSchema` (CFS) and its JSON shape.
  - `crates/raster-core/src/manifest.rs`: `Manifest` type.
  - `crates/raster-core/src/trace.rs`: trace model used by runtime tracers.

- **Compilation orchestration**
  - `crates/raster-compiler/src/lib.rs`: re-exported compiler APIs.
  - `crates/raster-compiler/src/builder.rs`: `Builder` and artifact/cache behavior.
  - `crates/raster-compiler/src/ast.rs`: AST parsing (`syn`) and `CallInfo` extraction.
  - `crates/raster-compiler/src/tile.rs` and `crates/raster-compiler/src/sequence.rs`: tile/sequence discovery views over the AST.
  - `crates/raster-compiler/src/cfs_builder.rs`: `CfsBuilder` (CFS assembly).
  - `crates/raster-compiler/src/flow_resolver.rs`: argument binding resolution for CFS construction.
  - `crates/raster-compiler/src/schema_gen.rs`: `SchemaGenerator` (currently `todo!()`).

- **Execution + backend surface**
  - `crates/raster-backend/src/backend.rs`: `Backend` trait, `ExecutionMode`, and execution result types.
  - `crates/raster-backend/src/native.rs`: `NativeBackend` (currently placeholder execution).
  - `crates/raster-runtime/src/{lib.rs,tracing.rs}` and `crates/raster-runtime/src/tracing/subscriber/*`: trace subscriber plumbing for tile I/O tracing and commitment checks.
  - `crates/raster-backend-risc0/src/lib.rs` and its modules: `Risc0Backend` and GPU availability helpers.

- **CLI workflow reference (non-library API, but reflects intended user journey)**
  - `crates/raster-cli/src/{main.rs,commands.rs}`: `cargo raster build/run/preview/cfs` behavior and gaps.

---

## Public APIs (inventory)

This section lists the public Rust surface area by crate, focusing on items expected to be used directly by developers.

### `raster` (primary application crate)

From `crates/raster/src/lib.rs`:

- **Modules**
  - `pub mod prelude`
  - `pub use raster_core as core`

- **Macros**
  - `pub use raster_macros::{tile, sequence}` (proc macro attributes)

- **Runtime (only when `raster` is built with feature `std`)**
  - `pub use raster_runtime::{init, init_with, finish, __emit_trace}`
  - `pub use raster_runtime::{JsonSubscriber, CommitSubscriber, AuditSubscriber, Subscriber}`

From `raster::prelude`:

- **Core types (always available)**
  - `raster::core::tile::{TileId, TileMetadata, TileIdStatic, TileMetadataStatic}`
  - `raster::core::{Result}`
  - `raster::{tile, sequence}` (macros)

- **Std-only types**
  - `raster::core::schema::{SequenceSchema, ControlFlow}`
  - `raster::core::manifest::Manifest`
  - `raster::core::trace::{Trace, TraceEvent}`

- **Registry (only when `std` and not `target_arch = "riscv32"`)**
  - `raster::core::registry::{TileRegistration, iter_tiles, find_tile, find_tile_by_str, tile_count, SequenceRegistration, SequenceMetadataStatic, iter_sequences, find_sequence, sequence_count}`

### `raster-macros` (proc macros; used via `raster`)

From `crates/raster-macros/src/lib.rs`:

- **Attribute macros**
  - `#[tile(...)]`
  - `#[sequence(...)]`

### `raster-core` (types, schemas, errors)

From `crates/raster-core/src/lib.rs` and modules:

- **Errors**
  - `pub enum Error`
  - `pub type Result<T> = core::result::Result<T, Error>`

- **Tile identity + metadata**
  - `pub struct TileId(pub String)`
  - `pub struct TileIdStatic(pub &'static str)`
  - `pub struct TileMetadata { ... }`
  - `pub struct TileMetadataStatic { ... }`

- **Std-only schemas**
  - `pub struct SequenceSchema`
  - `pub enum ControlFlow`
  - `pub struct Branch`
  - `pub struct Manifest`
  - `pub struct Trace`
  - `pub enum TraceEvent`

- **Std + non-RISC-V registry**
  - `pub type TileEntryFn`
  - `pub struct TileRegistration`
  - `pub fn iter_tiles() -> impl Iterator<Item = &'static TileRegistration>`
  - `pub fn find_tile(...) -> Option<&'static TileRegistration>`
  - `pub fn find_tile_static(...) -> Option<&'static TileRegistration>`
  - `pub fn find_tile_by_str(...) -> Option<&'static TileRegistration>`
  - `pub fn tile_count() -> usize`
  - `pub fn all_tile_ids() -> Vec<TileId>`
  - `pub fn all_tile_metadata() -> Vec<TileMetadata>`
  - `pub struct SequenceMetadataStatic`
  - `pub struct SequenceRegistration`
  - `pub fn iter_sequences() -> impl Iterator<Item = &'static SequenceRegistration>`
  - `pub fn find_sequence(id: &str) -> Option<&'static SequenceRegistration>`
  - `pub fn sequence_count() -> usize`

- **Control Flow Schema (CFS) types** (used by the compiler and CLI)
  - `pub struct ControlFlowSchema`
  - `pub struct TileDef`
  - `pub struct SequenceDef`
  - `pub struct SequenceItem`
  - `pub struct InputBinding`
  - `pub enum InputSource`

- **Re-exports used by macro-generated code**
  - `pub use postcard` (always)
  - `pub use bincode` (only with `std`)
  - `pub use linkme` (only when `std` and not `target_arch = "riscv32"`)

### `raster-compiler` (tooling/library compilation APIs)

From `crates/raster-compiler/src/lib.rs`:

- **Builders and outputs**
  - `pub struct Builder`
  - `pub struct BuildOutput`
  - `pub struct TileArtifact`
  - `pub struct TileManifest`

- **CFS**
  - `pub struct CfsBuilder`
  - `pub struct ProjectAst` (re-exported; parses project source to an AST)

- **Source discovery types**
  - `pub struct TileDiscovery` (in `raster_compiler::tile`)
  - `pub struct Tile` (in `raster_compiler::tile`)
  - `pub struct SequenceDiscovery` (in `raster_compiler::sequence`)
  - `pub struct Sequence` and `pub enum SequenceStep` (in `raster_compiler::sequence`)
  - `pub struct CallInfo` / `pub struct FunctionAstItem` (in `raster_compiler::ast`)

- **Dataflow resolution**
  - `pub struct FlowResolver`

- **Schema generation**
  - `pub struct SchemaGenerator` (currently not implemented)

### `raster-backend` (backend trait surface)

From `crates/raster-backend/src/lib.rs`:

- **Backend abstraction**
  - `pub trait Backend`
  - `pub struct CompilationOutput`
  - `pub enum ExecutionMode`
  - `pub struct TileExecution`
  - `pub struct ResourceEstimate`

- **Helpers/constants**
  - `pub const MIN_PROOF_SEGMENT_CYCLES: u64`
  - `pub fn calculate_proof_cycles(actual_cycles: u64) -> u64`

- **Built-in backend**
  - `pub struct NativeBackend`

### `raster-backend-risc0` (RISC Zero backend)

From `crates/raster-backend-risc0/src/lib.rs`:

- **Backend**
  - `pub struct Risc0Backend`

- **GPU feature detection**
  - `pub fn is_gpu_available() -> bool`
  - `pub fn is_metal_available() -> bool`
  - `pub fn is_cuda_available() -> bool`

### `raster-runtime` (std-only tracing helpers)

From `crates/raster-runtime/src/lib.rs`:

- **Tracing lifecycle**
  - `pub fn init()`
  - `pub fn init_with<S: Subscriber>(subscriber: S)`
  - `pub fn finish()`
  - `pub fn __emit_trace(...)` (used by macro-generated code; not intended as a user entrypoint)

- **Subscriber API**
  - `pub trait Subscriber`
  - `pub struct JsonSubscriber<W>`
  - `pub struct CommitSubscriber<W>`
  - `pub struct AuditSubscriber`

---

## Stable vs unstable surface area

This section classifies the developer-facing surface by intended stability **based on the current implementation quality and re-export choices**.

### Stable (recommended for app authors)

- **`raster` crate re-exports**
  - `raster::{tile, sequence}` macros
  - `raster::prelude` (for importing the common surface in one line)
  - `raster::core::tile::{TileId, TileMetadata, TileIdStatic, TileMetadataStatic}`
  - `raster::core::{Error, Result}`

### Public but currently incomplete / evolving (use with caution)

- **Sequences beyond “linear call listing”**
  - `#[sequence]` macro registration exists, but control-flow modeling is not implemented.
  - `raster-core::schema::{SequenceSchema, ControlFlow}` exists, but generation is not implemented (`SchemaGenerator::generate` is `todo!()`).

- **Runtime execution and tracing**
  - `raster-runtime` provides **trace subscribers** (stdout JSON, commitment file writing, and commitment auditing), but it does not provide a sequence/program executor.

- **Compilation orchestration APIs**
  - `raster-compiler::{Builder, CfsBuilder, ...}` are public and used by the CLI, but their artifact hashing/caching and compilation assumptions are still evolving.

- **Backend integration**
  - `raster-backend::Backend` is public and usable for integrators, but the serialization format and “native execution through registry” are not fully wired up.

### Internal / not part of the stable surface (apps should not rely on these)

- **Macro-generated symbols**
  - The generated wrapper function `__raster_tile_entry_<fn_name>` and registration statics are implementation details and MUST NOT be referenced by user code.

- **Direct registry/linker details**
  - `linkme`-specific distributed slice wiring is an implementation detail (even though registry helper functions are public on supported targets).

---

## Expected user workflow (define → compile → execute → verify)

### Define (author tiles and sequences)

- Developers MUST define tiles as free functions annotated with `#[tile(...)]`.
- Developers MAY define sequences as free functions annotated with `#[sequence(...)]` to support tooling (CFS generation and CLI introspection).

### Compile (produce artifacts / identities)

- Tooling SHOULD use `raster-compiler::Builder` when building tiles from source code (the CLI uses this path).
- Tooling MAY use `raster-compiler::CfsBuilder` to generate a `raster-core::cfs::ControlFlowSchema` (JSON) for downstream compilation/verification pipelines.

### Execute (native or zkVM)

- For zkVM execution/proving, tooling SHOULD use a backend implementing `raster-backend::Backend` (e.g., `raster-backend-risc0::Risc0Backend`).
- For native execution, direct calling of the tile function is currently the most reliable path.
- Registry-based “execute by id” is available only when `std` and not on `riscv32`, and backend-driven native execution is not fully implemented.

### Verify

- Backends that generate proofs MUST expose verification via `Backend::verify_receipt`.
- When `ExecutionMode::Prove { verify: true }` is used, the backend SHOULD verify during execution and return `TileExecution.verified = Some(true|false)`.

---

## Authoring APIs (what users write)

### `#[tile]` attribute macro

#### Syntax

- `#[tile(...optional_kv_pairs...)]`
- Optional key/value pairs MAY include:
  - `kind = iter | recur` (defaults to `iter` when omitted)
  - `estimated_cycles = <u64>`
  - `max_memory = <u64>`
  - `description = "<string>"`

**Important (current implementation)**: the macro/parser recognizes **key/value pairs**. A positional form like `#[tile(recur)]` does **not** set the kind; use `kind = recur`.

#### Tile function requirements

Given:

- `#[tile(kind = iter)] fn f(a: A, b: B) -> R { ... }`

Then:

- Each argument type (`A`, `B`, ...) MUST be deserializable from `postcard` in the tile wrapper.
- The return value `R` MUST be serializable via `postcard` in the tile wrapper.
- The function MAY return a plain value `R`, or it MAY return a `Result<R>` where the error type can be propagated into `raster::core::Result` via `?` in the generated wrapper (in practice, using `raster::core::Result<R>` is the safe choice).

#### ABI serialization rules (as implemented)

The macro-generated wrapper `__raster_tile_entry_<fn>` has signature:

- `fn(&[u8]) -> raster::core::Result<Vec<u8>>`

It interprets the input bytes as:

- **0 arguments**: `postcard` encoding of `()` (unit).
- **1 argument**: `postcard` encoding of the single argument value.
- **2+ arguments**: `postcard` encoding of a tuple `(A, B, ...)`.

It encodes the output bytes as:

- `postcard` encoding of the returned value (after unwrapping `Result` if applicable).

#### Registration behavior

- When compiled with `std` and not on `target_arch = "riscv32"`, `#[tile]` MUST register the tile into `raster::core::registry::TILE_REGISTRY`.
- Otherwise, registration MUST be omitted (the tile remains callable as a normal Rust function).

#### Recursive tiles (`recur`)

- When a tile is declared as `#[tile(kind = recur, ...)]`, the macro also exports a `macro_rules!` macro with the same name as the function.
- That macro supports `tile_name!(args...)` syntax and expands to `tile_name(args...)` for native compilation.
- **Current compiler/CFS behavior**: the compiler’s AST-based call extraction does not treat `tile_name!(...)` macro invocations as calls, so this `!` syntax is not reflected in the emitted CFS today.

### `#[sequence]` attribute macro

- `#[sequence]` MAY be applied to a free function to declare a sequence.
- On supported host targets (`std` and not `riscv32`), it MUST register the sequence in `raster::core::registry::SEQUENCE_REGISTRY` along with an ordered list of called identifiers extracted from the function body.
- The macro currently extracts **simple function call expressions** and does not model Rust control flow in a sound way; developers SHOULD treat it as a tooling annotation, not as a behavioral guarantee.

---

## Examples

### Minimal tile (iter)

```rust
use raster::prelude::*;

#[tile(kind = iter, description = "Double a number", estimated_cycles = 1000)]
fn double(x: u64) -> u64 {
    x * 2
}
```

### Execute via registry (host-only)

```rust
use raster::prelude::*;

#[tile]
fn double(x: u64) -> u64 { x * 2 }

fn main() {
    // Registry APIs require: feature `std` and not target_arch = "riscv32"
    let tile = find_tile_by_str("double").expect("tile registered");
    let input = raster::core::postcard::to_allocvec(&42u64).unwrap();
    let output = tile.execute(&input).unwrap();
    let result: u64 = raster::core::postcard::from_bytes(&output).unwrap();
    assert_eq!(result, 84);
}
```

### Sequence (tooling annotation)

```rust
use raster::prelude::*;

#[tile]
fn greet(name: String) -> String { format!("Hello, {name}!") }

#[tile]
fn exclaim(s: String) -> String { format!("{s}!!!") }

#[sequence(description = "Greeting pipeline")]
fn main_sequence(name: String) -> String {
    let greeting = greet(name);
    exclaim(greeting)
}
```

---

## Gaps / divergences in the current implementation (MUST be addressed or documented by tooling)

- **`#[tile]` attribute syntax mismatch with discovery and CLI template**
  - The proc macro accepts `#[tile]` and defaults `kind` to `iter`.
  - The compiler’s AST-based discovery also defaults the discovered kind to `"iter"` when `kind` is omitted.
  - **However**: positional forms like `#[tile(recur)]` are not interpreted as setting the kind; authors should use `#[tile(kind = recur)]`.

- **Serialization format inconsistencies in comments and tooling**
  - The tile wrapper generated by `#[tile]` uses `postcard` for both input and output.
  - Comments in `raster-core::registry` and docs in `raster-backend::Backend` mention “bincode” for inputs; those are not aligned with the wrapper implementation.
  - The CLI `run` command serializes a `serde_json::Value` via `postcard`, which generally will not match the concrete Rust argument type expected by the tile; tooling MUST serialize values in the ABI shape described above (unit / value / tuple) for the tile’s real Rust types.

- **Execution stack is not wired end-to-end**
  - `raster-backend::NativeBackend::execute_tile` is a stub and does not execute via the registry.
  - The CLI “whole program” runner (`cargo raster run`) executes the user binary as a subprocess; tracing/commitment capture is handled by the `#[sequence] fn main` entry point + `raster-runtime` subscribers rather than an `Executor`.

- **Sequence schema generation is not implemented**
  - `raster-compiler::SchemaGenerator::generate` is `todo!()`.
  - `raster-core::schema::{SequenceSchema, ControlFlow}` exists but is not produced by current tooling.
