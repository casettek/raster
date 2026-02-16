## Overview

This document specifies how Raster authoring constructs in Rust source code map to the internal intermediate representations used by the current Raster toolchain, and how that representation lowers into the Control Flow Schema (CFS).

This spec is written to match the code as it exists today. Where the implementation is incomplete or fails to preserve important information (e.g., determinism, recursion markers), those gaps are explicitly called out.

## Code audit tasks (where to look)

- **Macro authoring surface (compile-time registration)**:
  - `crates/raster-macros/src/lib.rs`
    - `#[tile(...)]` macro expands:
      - ABI wrapper `__raster_tile_entry_<fn>`
      - `linkme` registration in `raster_core::registry::TILE_REGISTRY` (host-only cfg gate)
      - (for `recur`) a `macro_rules! <tile_name> { ... }` wrapper enabling `<tile_name>!(...)` syntax
    - `#[sequence(...)]` macro expands:
      - `linkme` registration in `raster_core::registry::SEQUENCE_REGISTRY`
      - extracts a *flat list of call identifiers* using a `syn` visitor (no bindings, no dataflow)

- **Actual IR used by the CLI/compiler today (AST-based, not macro output)**:
  - `crates/raster-compiler/src/ast.rs`
    - `ProjectAst`, `FunctionAstItem`, `MacroAstItem`
    - `CallInfo` (callee, arguments, optional `let` binding)
  - `crates/raster-compiler/src/tile.rs`
    - `TileDiscovery` and `Tile<'ast>` (reads `#[tile(...)]` kv args like `kind`, `description`, etc.)
  - `crates/raster-compiler/src/sequence.rs`
    - `SequenceDiscovery` and `Sequence<'ast>` (resolves calls into tile/sequence steps)
  - `crates/raster-compiler/src/flow_resolver.rs`
    - `FlowResolver` (binds argument strings to `InputSource`)
  - `crates/raster-compiler/src/cfs_builder.rs`
    - `CfsBuilder` (lowers discovered+resolved structures into `raster_core::cfs::ControlFlowSchema`)

- **CFS types and serialization shape**:
  - `crates/raster-core/src/cfs.rs`
    - `ControlFlowSchema`, `TileDef`, `SequenceDef`, `SequenceItem`, `InputBinding`, `InputSource`
  - `crates/raster-cli/src/commands.rs`
    - `cfs()` command: serializes CFS to JSON via `serde_json::to_string_pretty`

## Scope and terms

- **Authoring**: User-authored Rust code in their project’s `src/` tree.
- **Tile**: A Rust function annotated with `#[tile(...)]`.
- **Sequence**: A Rust function annotated with `#[sequence(...)]`.
- **Discovery IR**: The in-memory structures obtained by parsing Rust source into an AST and extracting functions/macros/calls (notably `ProjectAst`, `FunctionAstItem`, and `CallInfo`), plus the derived `TileDiscovery`/`SequenceDiscovery` views.
- **Resolved IR**: The per-sequence items where each call argument is mapped to an input source (`raster_core::cfs::SequenceItem` + `InputBinding`) as produced by `FlowResolver`.
- **CFS**: The final schema object (`raster_core::cfs::ControlFlowSchema`) emitted by the compiler/CLI.

## Authoring constructs

### Tiles (`#[tile(...)]`)

A tile is authored as a Rust function annotated with `#[tile(...)]`.

- The implementation recognizes these key/value attributes:
  - `kind = iter | recur` (defaults to `iter` when omitted)
  - `estimated_cycles = <u64>`
  - `max_memory = <u64>`
  - `description = "<string>"`

**Current implementation note**: the CLI/compiler does not evaluate macro expansions for discovery. It parses Rust source with `syn` and discovers tiles by finding function items with a `#[tile(...)]` (or `#[raster::tile(...)]`) attribute.

### Sequences (`#[sequence(...)]`)

A sequence is authored as a Rust function annotated with `#[sequence(...)]`.

- The implementation optionally supports:
  - `description = "<string>"`

The current toolchain models sequences as an ordered list of calls found in the function body, but only records call expressions whose callee is a bare identifier (no `::` paths, no method calls, and no macro invocations).

## Intermediate representations

### Discovery IR (compiler/CLI)

Discovery IR is produced by parsing the project’s `src/` directory recursively into a `syn` AST (`ProjectAst`) and extracting:

- **Functions** (`FunctionAstItem`): name, path, inputs, optional output type, and extracted `call_infos`
- **Macros/attributes** (`MacroAstItem`): macro name plus key/value args (best-effort)
- **Calls** (`CallInfo`): bare-identifier callsites with:
  - `callee: String`
  - `arguments: Vec<String>` (stringified tokens per argument expression)
  - `result_binding: Option<String>` (only for `let name = callee(...);` with identifier patterns)

From that AST, the compiler derives two “discovery views”:

- **Tiles**: `TileDiscovery` produces `Tile<'ast>` records with:
  - `id`/name = function name
  - `tile_type` from `#[tile(kind = ...)]` (defaults to `"iter"`)
  - optional metadata (`description`, `estimated_cycles`, `max_memory`)
- **Sequences**: `SequenceDiscovery` produces `Sequence<'ast>` records with:
  - `id`/name = function name
  - optional `description`
  - a linear list of steps resolved to either a discovered tile or a discovered sequence

### Resolved IR (dataflow resolution)

For each discovered sequence, the compiler resolves call arguments into input sources, producing a list of `raster_core::cfs::SequenceItem` records (implementation: `crates/raster-compiler/src/flow_resolver.rs`).

Each item has:

- **item_type**: `"tile"` or `"sequence"`.
- **item_id**: callee identifier.
- **input_sources**: list of `InputBinding`, one per argument, where each binding wraps an `InputSource`:
  - `external`
  - `seq_input { input_index }`
  - `item_output { item_index, output_index }`

## Lowering rules (authoring → discovery IR)

### Source traversal

- The compiler MUST scan the project’s `src/` directory.
- It MUST consider files with the `.rs` extension.
- It MUST traverse into subdirectories recursively.

**Gap (determinism)**: the current implementation uses an unsorted filesystem walk (`walkdir::WalkDir`). The resulting traversal order is platform/filesystem dependent, so the order of discovered tiles/sequences MAY differ across machines or runs.

### Tile discovery

When scanning a file, the compiler parses Rust source into a `syn` AST and discovers tiles by finding function items with a `#[tile(...)]` (or `#[raster::tile(...)]`) attribute.

As implemented today:

- The tile ID is the Rust function name.
- `tile_type` is read from the attribute’s `kind = ...` key (defaults to `"iter"` when omitted).
- `input_count` is `function.inputs.len()` from the parsed signature (AST-based, not line-based).
- `output_count` is currently:
  - `0` if the function has no return type (`-> ...` absent), else
  - `1` (there is no tuple-arity detection today; tuples and other return types are all counted as a single output).

### Sequence discovery

Sequences are discovered via the same parsed AST approach: function items with a `#[sequence(...)]` (or `#[raster::sequence(...)]`) attribute.

As implemented today:

- The sequence ID is the Rust function name.
- Parameter names are extracted from `syn` patterns (best-effort; complex patterns may be stringified and are generally not useful for binding resolution).
- Calls are extracted from the function body by walking the AST and recording call expressions whose callee is a bare identifier.

### Call extraction rules inside sequences

The compiler currently extracts calls by visiting the parsed AST:

- It records a call only when the callee is a bare identifier (e.g., `greet(name)`).
  - Path-qualified calls (e.g., `foo::bar(x)`) and method calls (e.g., `obj.bar(x)`) are not recorded.
  - Macro invocations (e.g., `bar!(x)`) are not recorded.
- It records `result_binding = Some(name)` only when a call is the direct initializer in `let name = callee(...);` (identifier patterns only; destructuring is not supported).
- Arguments are captured as stringified tokens for each argument expression (not by comma-splitting source text).

## Lowering rules (discovery/resolved IR → CFS)

### Tile lowering

Each discovered tile (`raster_compiler::tile::Tile<'ast>`) lowers to one `TileDef`:

- `TileDef.id` MUST be the tile function name.
- `TileDef.type` MUST be the discovered `tile_type` string (from `kind`, default `"iter"`).
- `TileDef.inputs` MUST be the number of parameters in the function signature.
- `TileDef.outputs` MUST be:
  - `0` when the function has no return type, else
  - `1` (current limitation: no tuple/multi-output arity detection).

### Sequence lowering

Each discovered sequence (`raster_compiler::sequence::Sequence<'ast>`) lowers to one `SequenceDef`:

- `SequenceDef.id` MUST be the sequence function name.
- `SequenceDef.input_sources` MUST contain exactly `input_count` bindings, each `external`
- `SequenceDef.items` MUST be the ordered list of resolved `SequenceItem` values produced by dataflow resolution

### Item typing

For each recorded call (from `FunctionAstItem.call_infos`):

- If `callee` matches a known discovered tile ID, `item_type` MUST be `"tile"`.
- Else if `callee` matches a known discovered sequence ID, `item_type` MUST be `"sequence"`.
- Else `item_type` defaults to `"tile"`.

### Dataflow resolution (argument binding)

For a given sequence:

- A mapping `param_indices` is constructed from `param_names[i] -> i`.
- A mapping `bindings` is constructed as the sequence is processed left-to-right:
  - if a call has `result_binding = Some(name)`, then `bindings[name] = (item_index, 0)`

For each argument string `arg` in a call’s `arguments`, the compiler binds it as:

- If `arg` matches a parameter name in `param_indices`, it MUST produce `seq_input { input_index }`.
- Else if `arg` matches a previous `result_binding` in `bindings`, it MUST produce `item_output { item_index, output_index }`.
- Else it MUST produce `external`.

**Gaps (dataflow fidelity)**:

- Only output index `0` is used for bindings, even if a tile returns a tuple.
- Literals and complex expressions are treated as `external` rather than “constant” inputs.

### Recursive calls

The current compiler-side call extractor does not record macro invocations like `callee!(...)` as calls. As a result:

- There is no discovery-level “is_recursive” flag today.
- The emitted CFS cannot represent per-call recursive intent.

## Determinism, ordering, and canonicalization

### Ordering rules that are intended

To ensure reproducible outputs, a canonicalized CFS SHOULD be derived with:

- `cfs.tiles` sorted lexicographically by `TileDef.id`
- `cfs.sequences` sorted lexicographically by `SequenceDef.id`
- within each sequence, `SequenceDef.items` MUST preserve the authoring order (left-to-right within the sequence body)
- within each item, `input_sources` MUST preserve the argument order of the call

### Current implementation behavior (non-canonical)

- Discovery order depends on:
  - filesystem traversal order (`read_dir` without sorting)
  - file ordering encountered by traversal
- `cfs.tiles` is emitted in discovery order.
- `cfs.sequences` is emitted in discovery order.
- The CLI writes JSON using pretty-printing (`serde_json::to_string_pretty`), which is not a canonical byte encoding.

If a consumer needs stable hashing or identity over CFS content, it MUST apply a deterministic normalization (e.g., sorting as above and then using a canonical encoding as specified elsewhere).

## Error cases

Discovery/building may fail with:

- **Output write failures**: writing `cfs.json` can fail due to I/O errors.
- **Parse failures**: parts of the current AST construction path use `unwrap` and may panic on malformed Rust source or malformed `Cargo.toml` rather than returning a structured `raster_core::Error`.
- **Empty discovery**: missing/empty `src/` typically results in “no functions discovered” (empty tiles/sequences) rather than a structured error.

## Examples

### Example authoring

```rust
use raster::prelude::*;

#[tile(description = "Greets")]
fn greet(name: String) -> String {
    format!("Hello, {name}!")
}

#[tile(kind = recur, description = "Counts until done")]
fn count_to(state: (u64, u64)) -> (u64, u64) {
    state
}

#[sequence(description = "Main flow")]
fn main(name: String) -> String {
    let greeting = greet(name);
    let _state = count_to((0, 10));
    greeting
}
```

### Expected extracted information (current behavior)

- Tile discovery will record:
  - `greet`: `type = "iter"` (default), `inputs = 1`, `outputs = 1`
  - `count_to`: `type = "recur"`, `inputs = 1`, `outputs = 1` (current limitation: tuple return types are still counted as a single output)
- Sequence discovery + call extraction will record callsites only for bare-identifier calls:
  - `greet(name)` is recorded, and `name` can resolve to `seq_input(0)`.
  - `count_to((0, 10))` is recorded, but the argument string `"(0, 10)"` does not match a parameter/binding name, so it falls back to `external` in the CFS binding model.

### Expected CFS (informative JSON shape)

The CLI emits JSON for `ControlFlowSchema`. The relevant structure looks like:

```json
{
  "version": "1.0",
  "project": "<from Cargo.toml>",
  "encoding": "postcard",
  "tiles": [
    { "id": "greet", "type": "iter", "inputs": 1, "outputs": 1 },
    { "id": "count_to", "type": "recur", "inputs": 1, "outputs": 1 }
  ],
  "sequences": [
    {
      "id": "main",
      "input_sources": [{ "source": { "type": "external" } }],
      "items": [
        {
          "item_type": "tile",
          "item_id": "greet",
          "input_sources": [{ "source": { "type": "seq_input", "input_index": 0 } }]
        },
        {
          "item_type": "tile",
          "item_id": "count_to",
          "input_sources": [{ "source": { "type": "external" } }]
        }
      ]
    }
  ]
}
```

**Gap reminder**: macro invocations like `count_to!(...)` are not recorded as calls by the compiler today, complex argument expressions fall back to `external`, and multi-output/tuple-return arity is not modeled (outputs are `0` or `1`).

## Known gaps and follow-ups

- Implement deterministic discovery ordering (sort directory entries and/or sort final `tiles`/`sequences` lists before emitting CFS).
- Improve call extraction to handle more callee forms (e.g., `foo::bar(...)`, method calls, and/or selected macro invocations) or explicitly document them as out of scope.
- Preserve recursive intent in the resolved IR and CFS (would require both extracting a recursion authoring signal and extending `raster_core::cfs` types and the lowering).
- Extend dataflow bindings to support tuple outputs (`output_index > 0`) and destructuring bindings.
