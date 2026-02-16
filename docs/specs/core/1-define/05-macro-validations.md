## Macro Validations

This document specifies the compile-time validation behavior of Raster’s procedural macros as implemented today. It is intended for implementers who need to understand which authoring constraints are enforced by the macros themselves versus deferred to standard Rust compilation errors.

### Code audit tasks (where to look)

- **Macro implementations**: `crates/raster-macros/src/lib.rs`
  - `#[tile]`: `TileAttrs::parse` and `pub fn tile(...)`
  - `#[sequence]`: `SequenceAttrs::parse`, `TileCallExtractor`, and `pub fn sequence(...)`
- **Macro failure notes**: `specs/Core/0. Conventions/04. Errors and Diagnostics.md` (“Procedural macro failures” section)
- **Authoring docs that currently diverge from implementation (see “Known gaps”)**
  - `docs/tile-authoring.md` (uses `#[tile]` and `#[tile(estimated_cycles = ...)]`)
  - `crates/raster-cli/src/commands.rs` (`init` scaffolds `#[tile(description = ...)]`)

### What “compile-time validation” means here

Raster currently relies on three layers of enforcement:

- **Proc-macro parsing/expansion failures**: the macro panics or fails to parse the annotated item.
- **Rust type-checking of generated code**: the macro expands successfully, but the generated wrapper/registration fails to compile (trait bounds, name collisions, signature mismatches, etc.).
- **No validation (silent acceptance)**: attributes/constructs are accepted and either ignored or later interpreted by other pipeline components.

### `#[tile(...)]` validation checklist (as implemented)

#### Enforced by the proc-macro (hard failures during expansion)

- **The macro MUST be applied to a function item**.
  - Condition: `#[tile(...)]` is attached to a non-`fn` item.
  - Diagnostic: emitted by the Rust compiler/proc-macro parser (from `syn`); message text is not stable and is not controlled by Raster.

- **If `kind = ...` is provided, it MUST be exactly `iter` or `recur`**.
  - Condition: `#[tile(kind = foo)]`.
  - Diagnostic (emitted via `panic!`; message text is not stable for tooling, but currently includes):
    - `Unknown tile kind 'foo'. Valid kinds: iter, recur`

**Important (current behavior)**:

- `#[tile]` and `#[tile()]` are accepted and default `kind` to `iter`.
- Positional forms like `#[tile(recur)]` do **not** set `kind` in the current macro; use `#[tile(kind = recur)]`.

#### Enforced indirectly (macro expands, but Rust compilation can fail)

These are real authoring constraints, but Raster does not emit a dedicated diagnostic for them today; failures surface as ordinary Rust compilation errors.

- **Tile inputs MUST be deserializable via `postcard`**.
  - Condition: any input type does not implement the required `postcard`/`serde` bounds for `postcard::from_bytes`.
  - Effect: compilation fails in the generated wrapper at the `postcard::from_bytes(input)` call.

- **Tile outputs MUST be serializable via `postcard`**.
  - Condition: the return type (or the `Ok(T)` type when returning a `Result`) is not serializable for `postcard::to_allocvec(&result)`.
  - Effect: compilation fails in the generated wrapper at `postcard::to_allocvec(&result)`.

- **If the tile is detected as returning `Result`, the error type MUST be convertible into `raster::core::Error`**.
  - Detection rule (current implementation): the macro checks whether the *last path segment* of the return type is named `Result` (string/identifier match).
  - Condition: tile returns `Result<T, E>` but `raster::core::Error: From<E>` is not satisfied.
  - Effect: compilation fails at the generated `let result = tile_fn(...) ?;`.

- **Tile function parameters SHOULD use simple identifier patterns** (e.g., `x: T`, not destructuring patterns).
  - Current behavior: the macro only records parameter names when the pattern is `Pat::Ident`.
  - Condition: a parameter pattern is not an identifier (e.g., `(_, x): (u64, u64)`).
  - Effect: the proc-macro is likely to panic internally (e.g., due to indexing `input_names[0]`) or generate code that fails to compile.
  - **GAP**: there is no targeted, span-aware diagnostic for this; the resulting message is not stable.

- **Tiles SHOULD be free functions (not methods with a receiver)**.
  - Current behavior: `self`/receiver arguments are ignored when generating wrapper inputs, and the wrapper invokes the function name as `fn_name(...)`.
  - Condition: `#[tile(...)]` is applied to an `impl` method using `&self`/`self`.
  - Effect: compilation fails due to an invalid call form and/or mismatched arguments.
  - **GAP**: not validated by the macro.

- **Macro-generated symbols MUST NOT collide**.
  - Generated symbols:
    - wrapper function: `pub fn __raster_tile_entry_<fn_name>(...)`
    - registration static: `static __RASTER_TILE_REGISTRATION_<FN_NAME_UPPERCASE>: ...`
    - for `recur`: exported macro `macro_rules! <fn_name> { ... }`
  - Condition: duplicate definitions in the same module/crate (or collisions with existing items).
  - Effect: compilation fails with standard Rust “defined multiple times”/name collision errors.

#### Accepted but not validated (silent behavior)

- **Unknown key/value attributes are silently ignored**.
  - Only these keys are interpreted: `kind`, `estimated_cycles`, `max_memory`, `description`.
  - Any other `key = value` pairs currently have no effect and produce no diagnostic.

- **Invalid numeric attribute values are silently treated as “unset”**.
  - Condition: `estimated_cycles = "not_a_number"` or any value that fails `u64` parsing.
  - Effect: the field is treated as `None` with no diagnostic.

### `#[sequence(...)]` validation checklist (as implemented)

#### Enforced by the proc-macro

- **The macro MUST be applied to a function item**.
  - Condition: `#[sequence]` attached to a non-`fn` item.
  - Diagnostic: emitted by the Rust compiler/proc-macro parser (from `syn`); message text is not stable and is not controlled by Raster.

#### Accepted but not validated (silent behavior)

- **Only `description = "..."` is parsed from attributes; all other keys are ignored**.
- **The macro does not validate that extracted calls correspond to tiles**.
  - It records simple function calls `foo(...)` in the function body (it ignores method calls like `obj.foo(...)`).
  - It excludes a hardcoded list of common non-tile function names (e.g., `println`, `format`, `Some`, `Ok`, etc.).
  - It does not check whether the referenced functions are annotated with `#[tile]`.

### Diagnostics reference (messages you can match in CI logs)

#### `#[tile]` panics (exact strings)

The current macro uses `panic!` only when `kind = ...` is present and invalid. The exact string is not guaranteed stable, but currently looks like:

- `Unknown tile kind 'X'. Valid kinds: iter, recur`

#### Serialization error strings produced at runtime by generated wrappers (not compile-time)

These are not macro validation errors, but they are stable strings introduced by macro-generated code:

- `Failed to deserialize input: ...` (wrapped as `Error::Serialization(...)`)
- `Failed to serialize output: ...` (wrapped as `Error::Serialization(...)`)

### Examples

Valid (iterative tile):

```rust
use raster::prelude::*;

#[tile]
fn double(x: u64) -> u64 {
    x * 2
}
```

Valid (recursive tile; also defines a `step!()`-style macro with the same name when `kind = recur` is used):

```rust
use raster::prelude::*;

#[tile(kind = recur)]
fn step(state: u64) -> Result<u64, raster::core::Error> {
    Ok(state + 1)
}
```

Invalid (`kind` value; will panic during macro expansion):

```rust
use raster::prelude::*;

#[tile(kind = foo)]
fn bad(x: u64) -> u64 { x }
```

Expected diagnostic contains:

- `Unknown tile kind 'foo'. Valid kinds: iter, recur`

### Known gaps / divergences from desired UX and from other docs

- **Macro ergonomics**: invalid `#[tile(...)]` usage currently panics instead of producing span-aware compiler diagnostics. This makes errors less precise and less stable for tooling.
- **Documentation divergence**:
  - Some older docs/examples use positional forms like `#[tile(recur)]`. In the current macro implementation, this does not set `kind`; authors should use `#[tile(kind = recur)]`.
- **Attribute validation gaps**:
  - unknown keys are silently ignored;
  - invalid `u64` values are silently dropped (treated as unset);
  - non-identifier parameter patterns can trigger internal macro panics instead of targeted diagnostics.

