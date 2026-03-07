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
- `call!` on a `recur` tile expands to a plain function call — there is no orchestration-driven recursive loop. A dedicated follow-up initiative will add recursive execution semantics.
- Treat recursion as an annotation/forward-compatible convention, not an enforced runtime behavior.

## Call primitives (`call!` and `call_seq!`)

Use `call!` and `call_seq!` inside sequences to invoke tiles and sub-sequences explicitly.

```rust
#[sequence]
fn greet_sequence(name: String) -> String {
    let greeting = call!(greet, name);        // invoke a tile
    let e1 = call!(exclaim, greeting);         // invoke a tile, chain results
    let result = call_seq!(wish_seq, e1);      // invoke a sub-sequence
    result
}
```

**Why use these instead of bare calls:**

- `call!(tile_fn, args...)`: the canonical tile step boundary. The compiler reliably extracts tile call sites from `call!` invocations without guessing by name-matching.
- `call_seq!(seq_fn, args...)`: the canonical sequence call boundary. The callee's `#[sequence]` wrapper handles `SequenceStart`/`SequenceEnd` trace events automatically.

Both macros:

- Return the callee's return value transparently — `let x = call!(foo, bar)` works as expected.
- Work in `std` and `no_std` contexts with no overhead on `no_std` / riscv32 targets.
- Are available via `use raster::prelude::*`.

**Nested calls must be decomposed:** calls like `exclaim(greet(name))` must be rewritten as:

```rust
let greeting = call!(greet, name);
let exclaimed = call!(exclaim, greeting);
```

This makes the dataflow explicit and CFS-derivable.

**Bare function calls are soft-deprecated.** If you use `greet(name)` directly in a sequence body, the compiler will emit a deprecation warning during CFS generation:

```
warning[raster]: bare call to tile `greet` in sequence `my_seq` is deprecated.
                 Use `call!(greet, ...)` instead.
```

## Sequences (`#[sequence]`)

Sequences are a discovery/annotation surface:

- They are registered on host targets.
- Tooling extracts an ordered list of tile/sequence calls for CFS derivation.
- Prefer `call!`/`call_seq!` inside sequences for reliable extraction.
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
- Always use `call!` for tile invocations and `call_seq!` for sequence invocations in sequence bodies.
- Decompose nested calls (`f(g(x))`) into sequential bindings (`let y = call!(g, x); let z = call!(f, y);`) for explicit dataflow and reliable CFS derivation.
- Keep sequence examples linear unless you are explicitly documenting current limitations.
