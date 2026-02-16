## Tiles

Tiles are the smallest executable units in Raster. A tile is authored as a Rust free function annotated with `#[tile(...)]`. Raster tooling generates a stable byte-level ABI wrapper for each tile, and (in host builds) registers tile metadata in a global registry for discovery and invocation.

This document describes the tile authoring rules and the tile ABI contract as implemented today, including known gaps where the current code does not yet enforce or represent the intended behavior.

## Code audit tasks (where to look)

- **Tile macro implementation**
  - `crates/raster-macros/src/lib.rs`
    - `TileAttrs::parse` (attribute parsing rules; required `iter`/`recur`)
    - `tile` proc-macro (ABI wrapper generation, `Result` handling, and optional recursion macro emission)
    - Naming: `__raster_tile_entry_<fn_name>` wrapper convention
- **Core tile types**
  - `crates/raster-core/src/tile.rs` (`TileId`, `TileMetadata`, and their `*_Static` forms)
  - `crates/raster-core/src/error.rs` (`Error::Serialization` and propagation behavior)
  - `crates/raster-core/src/lib.rs` (re-export of `postcard` and `no_std`/`alloc` posture)
- **Host-side registry (std-only, non-RISC-V)**
  - `crates/raster-core/src/registry.rs`
    - `TileEntryFn` signature (`fn(&[u8]) -> Result<Vec<u8>>`)
    - `TileRegistration::execute`
    - `TILE_REGISTRY` distributed slice + `find_tile_by_str`
- **Compiler-side discovery and representation**
  - `crates/raster-compiler/src/ast.rs`
    - `ProjectAst` + `CallInfo` extraction (parsed via `syn`)
  - `crates/raster-compiler/src/tile.rs`
    - `TileDiscovery` (discovers `#[tile]`; reads `kind`, defaults to `"iter"`)
  - `crates/raster-compiler/src/sequence.rs`
    - `SequenceDiscovery` (discovers `#[sequence]`; records only bare-identifier calls like `foo(...)`)
  - `crates/raster-compiler/src/cfs_builder.rs` and `crates/raster-core/src/cfs.rs`
    - `TileDef { type: "iter" | "recur", inputs, outputs }`
    - Note: `outputs` is currently only `0` or `1` (no tuple/multi-output arity detection), and there is no per-call recursion marker in CFS today.
- **zkVM integration (RISC0)**
  - `crates/raster-backend-risc0/src/guest_builder.rs`
    - Guest `main` reads `u32` length then raw input bytes
    - Guest calls the tile ABI wrapper directly and commits the raw output bytes
  - `crates/raster-backend-risc0/src/risc0.rs`
    - Host writes `u32` length + raw bytes into the environment
    - Host reads output bytes from the journal

## Tile definition rules

### Declaring a tile

A tile MUST be declared by applying the `#[tile(...)]` attribute to a Rust free function:

- The tile attribute MAY specify a tile kind via a **named argument**:
  - Valid: `#[tile(kind = iter)]`, `#[tile(kind = recur)]`
  - Default: if `kind` is omitted, the kind defaults to `iter`.
  - **Important**: the current proc-macro and compiler-side parser only recognize **key/value pairs**. A positional form like `#[tile(recur)]` is **not interpreted as setting the kind** (it is effectively ignored, and the kind remains the default `iter`).
- The macro MAY accept optional key/value metadata:
  - `description = "..."` (string)
  - `estimated_cycles = N` (u64)
  - `max_memory = N` (u64 bytes)

The tile’s **tile id** is the Rust function identifier string, as written in source (e.g., `double`, `hash_leaf`). The macro uses this string for both `TileMetadataStatic::id` and `TileMetadataStatic::name`.

Tiles SHOULD use globally-unique function names within the compilation unit where they are linked, because the host registry is keyed only by this id string.

### Signature restrictions

To be ABI-compatible with the current macro-generated wrapper, a tile function MUST satisfy all of the following:

- **Free function**: The function MUST NOT have a `self` receiver.
- **Non-generic**: The function MUST NOT be generic (type parameters and/or `where` clauses that introduce generics). The wrapper is monomorphic and cannot supply generic arguments.
- **Serde-compatible I/O types**:
  - Each input type MUST be deserializable via `postcard::from_bytes`.
  - The return type MUST be serializable via `postcard::to_allocvec`.
  - Practically, this means the input and output types SHOULD derive `serde::Deserialize`/`serde::Serialize`.
- **`Result` returns (optional)**:
  - A tile MAY return a `Result<T, raster_core::Error>` (or the `raster::core::Result<T>` alias).
  - If the macro detects the return type name as `Result`, the wrapper uses `?` to propagate errors.
  - To compile successfully, the tile’s error type MUST match `raster_core::Error` (or be convertible into it via `From`, which is not broadly implemented today).

### `no_std` and determinism constraints

- Tiles intended to run in the RISC0 guest MUST compile in a `#![no_std]` context with `alloc` available (the guest builder depends on the user crate with `default-features = false`).
- Tiles intended to be proven/verified SHOULD be deterministic and free of host-side nondeterminism (I/O, system time, randomness, threads, environment variables). Raster does not currently enforce “purity”; this is an implementer responsibility.

## Tile ABI wrapper and encoding contract

For every `#[tile(...)] fn <name>(...) -> ...` declaration, the macro generates a public ABI wrapper function:

- Name: `__raster_tile_entry_<name>`
- Signature: `pub fn __raster_tile_entry_<name>(input: &[u8]) -> raster_core::Result<alloc::vec::Vec<u8>>`

This wrapper is the cross-backend execution entry point:

- Host registry execution (`TileRegistration::execute`) calls this wrapper.
- RISC0 guest programs call this wrapper directly.

### Input encoding

The `input: &[u8]` MUST be a postcard encoding of the tile’s arguments, determined by arity:

- **0 arguments**: `input` MUST be `postcard`-encoded `()`.
- **1 argument**: `input` MUST be `postcard`-encoded value of that argument type.
- **2+ arguments**: `input` MUST be `postcard`-encoded tuple of all arguments in declaration order.

Examples:

- If the tile is `fn f() -> u64`, `input` is `postcard::to_allocvec(&())`.
- If the tile is `fn f(x: u64) -> u64`, `input` is `postcard::to_allocvec(&x)`.
- If the tile is `fn f(x: u64, y: u64) -> u64`, `input` is `postcard::to_allocvec(&(x, y))`.

### Output encoding

On success, the wrapper MUST return `Ok(output_bytes)` where `output_bytes` is the postcard encoding of the tile’s return value.

- If the tile returns `()`, the output is the postcard encoding of `()`.
- If the tile returns a tuple, the output is the postcard encoding of that tuple.

### Error behavior

- If input deserialization fails, the wrapper MUST return `Err(raster_core::Error::Serialization(...))`.
- If output serialization fails, the wrapper MUST return `Err(raster_core::Error::Serialization(...))`.
- If the tile returns `raster_core::Result<T>` and returns an `Err(e)`, the wrapper MUST propagate that error via `?` (no wrapping).

## Tile kinds: iterative vs recursive

Raster distinguishes tile “kinds” via the `kind = ...` argument to `#[tile(...)]`:

- **Iterative tiles (`iter`)**: standard tiles that execute once per call. This is the default.
- **Recursive tiles (`recur`)**: tiles intended (in the broader design) to be executed repeatedly until a termination condition is reached. In the current implementation, this affects metadata (e.g., CFS `TileDef.type`) and macro-generated helper code, but no runtime/executor implements recursive looping yet.

Today, tile kind is represented as a string value (`"iter"` or `"recur"`) in compiler-discovered metadata and in the generated CFS `TileDef.type`. The runtime ABI wrapper is identical for both kinds.

### Recursive invocation marker (`!`) and ABI wrapper

For `#[tile(kind = recur)]` tiles, the macro also emits a `macro_rules!` macro of the same name so user code can write `tile_name!(args)`:

- In native Rust semantics, `tile_name!(args)` expands to `tile_name(args)`.
- **Current compiler/CFS behavior**: the compiler’s AST-based call extraction records only normal function calls like `tile_name(args)`. Macro invocations like `tile_name!(args)` are **not recorded as calls** and therefore do not appear in the emitted CFS.

### Recursion step contract (intended; partially implemented)

When a recursive tile is invoked in “recursive mode”, the system is expected to repeatedly execute the tile as long as it indicates it is not finished. The current macro documentation implies:

- The recursive tile’s **first output value** indicates termination (“run until its first output returns true”).

To make that interoperable at the byte level, recursive tiles SHOULD follow this convention:

- A recursive tile SHOULD return a tuple whose first element is a `bool` named conceptually `done`.
- The remaining element(s) SHOULD contain the next-step state, and MUST be sufficient to re-invoke the tile again.

Example convention:

- Input type: `State`
- Output type: `(bool, State)` where:
  - `done == false` indicates “continue”, and the next invocation uses the returned `State`
  - `done == true` indicates “stop”, and the returned `State` is the final state

Known gaps (current behavior):

- The compiler’s CFS representation does not currently encode recursion steps, termination, or re-invocation wiring, and no executor implements recursive looping yet.
- The macro does not validate that a `#[tile(kind = recur)]` function returns a tuple whose first element is `bool`.
- As a result, “recursive execution” is currently a syntactic marker and metadata label, not an enforced runtime behavior.

## Examples

### Iterative tile example (single input)

```rust
use raster::prelude::*;
use serde::{Deserialize, Serialize};

#[tile(kind = iter, description = "Doubles a number", estimated_cycles = 1000)]
fn double(x: u64) -> u64 {
    x * 2
}

fn host_call_via_abi() -> raster::core::Result<u64> {
    // Encode input as postcard(u64) because this tile takes exactly one argument.
    let input = raster::core::postcard::to_allocvec(&21u64).unwrap();

    // Call the generated ABI wrapper.
    let output = __raster_tile_entry_double(&input)?;

    // Decode output as postcard(u64).
    let y: u64 = raster::core::postcard::from_bytes(&output).unwrap();
    Ok(y)
}
```

### Recursive tile example (state machine)

This example follows the intended `(done, next_state)` convention. Note that recursive looping is not yet implemented end-to-end; this shows the authoring pattern and ABI shape.

```rust
use raster::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Counter {
    current: u64,
    goal: u64,
}

#[tile(kind = recur, description = "Counts up to a goal")]
fn count_to(state: Counter) -> (bool, Counter) {
    if state.current >= state.goal {
        return (true, state);
    }

    (
        false,
        Counter {
            current: state.current + 1,
            goal: state.goal,
        },
    )
}

fn one_step_via_abi() -> raster::core::Result<(bool, Counter)> {
    let state = Counter { current: 0, goal: 3 };
    let input = raster::core::postcard::to_allocvec(&state).unwrap();
    let output = __raster_tile_entry_count_to(&input)?;
    let step: (bool, Counter) = raster::core::postcard::from_bytes(&output).unwrap();
    Ok(step)
}

// Note: the compiler/CFS generator currently does not treat `count_to!(...)` macro
// invocations as calls; use normal function calls for discoverability.
fn authoring_marker_example(state: Counter) -> (bool, Counter) {
    count_to(state)
}
```

## Known gaps and divergences (code vs desired behavior)

- **`#[tile]` without arguments**
  - The proc macro accepts empty attributes and defaults `kind` to `iter`.
  - The compiler’s source discovery also defaults the discovered kind to `"iter"` when `kind` is omitted.
  - **However**: positional forms like `#[tile(recur)]` are currently ignored by both the macro parser and the compiler parser; authors should use `#[tile(kind = recur)]`.
- **Encoding comments that mention bincode**
  - Several host-side comments (e.g., in `raster_core::registry` and `raster_backend::Backend`) mention “bincode”, but the tile ABI wrapper uses `postcard` for input/output encoding.
  - Implementations MUST treat the tile ABI encoding as postcard unless and until a versioned encoding switch is introduced.
- **Recursive execution**
  - Tile kind can be marked as `"recur"` via `#[tile(kind = recur)]`, but no executor implements recursive looping today.
  - Macro invocations like `count_to!(...)` are not discovered by the compiler’s AST call extraction and therefore do not appear in the CFS.
  - Any “recursive step contract” described above is therefore a convention for future compatibility rather than behavior enforced today.
