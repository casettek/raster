# Tile Authoring Guide

## What a tile is

A tile is a Rust free function annotated with `#[tile]`. The macro generates:

- A byte-ABI wrapper (`__raster_tile_entry_<fn_name>`)
- Host registration metadata (on supported host targets)
- Runtime trace hook wiring

Tiles are the unit compiled and executed by backends.

## Minimal tile

```rust
use raster::prelude::*;

#[tile]
fn double(x: u64) -> u64 {
    x * 2
}
```

## Supported `#[tile(...)]` attributes

Use key/value syntax:

- `kind = iter | recur` (default: `iter`)
- `description = "..."` (optional)
- `estimated_cycles = <u64>` (optional)
- `max_memory = <u64>` (optional)

Example:

```rust
#[tile(
    kind = iter,
    description = "Doubles a number",
    estimated_cycles = 1000,
    max_memory = 4096
)]
fn double(x: u64) -> u64 {
    x * 2
}
```

Important:

- Use `kind = recur`, not `#[tile(recur)]`.
- Unknown attributes are ignored by the macro parser today.

## Signature and type requirements

Tiles should follow these constraints for reliable compilation/execution:

- Use free functions (no `self` receiver).
- Avoid generic tile signatures.
- Use serde-compatible input/output types.
- Prefer simple identifier parameters (avoid complex/destructured patterns).
- Use `raster::core::Result<T>` for fallible tiles when possible.

## Tile ABI contract (current implementation)

Tile wrappers use `postcard` for input/output bytes.

Input encoding by arity:

- 0 args: `postcard(())`
- 1 arg: `postcard(arg)`
- N>1 args: `postcard((arg1, arg2, ...))`

Output encoding:

- `postcard(return_value)` (or `postcard(ok_value)` for `Result`)

If decode/encode fails, the wrapper returns `raster_core::Error::Serialization`.

## Recursive tiles (`kind = recur`)

`recur` currently affects metadata and emits a helper macro (`tile_name!(...)`) for authoring ergonomics.

Current limitations:

- No runtime recursive execution loop exists yet.
- Compiler call extraction does not reliably capture `tile_name!(...)` macro calls.
- Treat recursion as an annotation/forward-compatible convention, not an enforced runtime behavior.

## Sequences (`#[sequence]`)

Sequences are currently a discovery/annotation surface:

- They are registered on host targets.
- Tooling extracts an ordered list of simple function calls.
- They are not a full control-flow program representation today.

Do not rely on sequence branching semantics being represented end-to-end.

## CLI author workflow

Common commands:

```bash
# List discovered tiles
cargo raster list

# Run one tile natively
cargo raster run-tile --backend native --tile double --input "42"

# Run one tile in RISC0 estimate mode
cargo raster run-tile --backend risc0 --tile double --input "42"

# Prove and verify
cargo raster run-tile --backend risc0 --tile double --input "42" --prove --verify
```

## Testing guidance

### Unit tests

Test tile functions like normal Rust functions.

### ABI-level tests

For backend interop, add tests that encode/decode inputs/outputs with `postcard` using the tile ABI shape (unit/value/tuple).

### Whole-program tests

Use `cargo raster run` for end-to-end native runs with optional `--commit`/`--audit` to validate trace commitment behavior.

## Recommended patterns

- Keep tiles focused and composable.
- Use explicit stable input/output types.
- Prefer deterministic logic for prove/verify workflows.
- Document resource hints (`estimated_cycles`, `max_memory`) when known.
- Keep sequence examples linear unless you are explicitly documenting current limitations.
