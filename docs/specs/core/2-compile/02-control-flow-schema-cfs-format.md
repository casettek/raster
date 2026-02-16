## Control Flow Schema (CFS) format

The Control Flow Schema (CFS) is a serialized description of the tiles and sequences in a Raster project, plus the data-flow bindings between calls inside each sequence.

In the current implementation, the CFS is emitted as **pretty-printed JSON** by the Raster CLI and is consumed by compiler/backends as an input to later compilation and execution steps.

## Code audit tasks (where to look)

- **Schema struct definitions and serde encoding**
  - `crates/raster-core/src/cfs.rs`
    - `ControlFlowSchema`
    - `TileDef`
    - `SequenceDef`
    - `SequenceItem`
    - `InputBinding`
    - `InputSource` (externally tagged enum via `#[serde(tag = "type")]`)
- **CFS construction (project scanning + assembly)**
  - `crates/raster-compiler/src/cfs_builder.rs`
    - `CfsBuilder::build`
    - `CfsBuilder::build_sequence_def`
- **Source discovery and call parsing (inputs, bindings)**
  - `crates/raster-compiler/src/ast.rs`
    - `ProjectAst::new(...)` (walks `src/**/*.rs`, parses with `syn`)
    - `CallVisitor` (extracts `CallInfo { callee, arguments, result_binding }`)
  - `crates/raster-compiler/src/tile.rs`
    - `TileDiscovery::new(...)` (filters `#[tile]` fns; reads `kind = ...` and other kv pairs)
  - `crates/raster-compiler/src/sequence.rs`
    - `SequenceDiscovery::new(...)` (filters `#[sequence]` fns; resolves steps to tiles/sequences)
- **Data-flow binding resolution**
  - `crates/raster-compiler/src/flow_resolver.rs`
    - `FlowResolver::resolve` / `resolve_argument`
    - Binding model: `SeqInput` vs `ItemOutput` vs fallback `External`
    - Tests: `test_resolve_simple_sequence`
- **JSON serialization and file emission**
  - `crates/raster-cli/src/commands.rs`
    - `cfs(...)` command uses `serde_json::to_string_pretty(&cfs)` and writes `cfs.json`
- **Recursion authoring signal (macro emission)**
  - `crates/raster-macros/src/lib.rs`
    - `#[tile(kind = recur)]` emits a `macro_rules! <tile_name> { ... }` so authors can write `tile_name!(...)`
    - **Note**: the current compiler-side call extraction does not treat `tile_name!(...)` macro invocations as calls, so this `!` form does not appear in the CFS today.

## JSON file format

### Document type

A CFS document MUST be a single JSON object with the following required fields:

- `version` (string)
- `project` (string)
- `encoding` (string)
- `tiles` (array of `TileDef`)
- `sequences` (array of `SequenceDef`)

The Raster CLI currently produces the JSON using Serde defaults (no custom field ordering beyond struct field order).

### Binary container encoding (not implemented)

The current toolchain does not implement a binary CFS container format. In practice:

- Producers MUST serialize the CFS as UTF-8 JSON.
- Consumers MUST accept UTF-8 JSON as the CFS container encoding.

Any future binary container format would require an explicit spec update and an explicit producer/consumer implementation; it does not exist today.

### Compatibility and `version`

- Producers MUST set `version` to `"1.0"` (current implementation hard-codes this).
- Consumers SHOULD reject unknown major versions.
- Consumers MAY accept newer minor versions if all required fields they rely on are present and well-typed.

### `encoding`

The `encoding` field identifies the byte-level serialization used by tile ABI wrappers and runtime-provided values referenced by bindings.

- Producers MUST set `encoding` to `"postcard"` (current implementation hard-codes this).
- Consumers MUST interpret `"postcard"` to mean the [Postcard](https://docs.rs/postcard/) encoding used by macro-generated tile entrypoints.

Note: the **CFS file itself is JSON**; `encoding` does not describe the CFS container encoding.

## Types

### `ControlFlowSchema` (root)

```json
{
  "version": "1.0",
  "project": "my-project",
  "encoding": "postcard",
  "tiles": [ /* TileDef */ ],
  "sequences": [ /* SequenceDef */ ]
}
```

- `project` MUST be the project name string determined by the CLI.
  - The current implementation tries to parse the first `name = "..."` it sees in `Cargo.toml`, and falls back to the root directory name.

### `TileDef`

Each element of `tiles` MUST be an object with:

- `id` (string): tile identifier (currently the Rust function name)
- `type` (string): tile kind
- `inputs` (integer): number of inputs (arity)
- `outputs` (integer): number of outputs (arity)

Example:

```json
{
  "id": "greet",
  "type": "iter",
  "inputs": 1,
  "outputs": 1
}
```

#### Tile kind strings (`type`)

Producers MUST emit the discovered tile kind string.

In the current implementation:

- `type` is usually `"iter"` or `"recur"` (derived from `#[tile(kind = iter)]` / `#[tile(kind = recur)]`; defaults to `"iter"` when omitted).
- If the discovery parser cannot recognize the kind, it defaults to `"iter"`.

### `SequenceDef`

Each element of `sequences` MUST be an object with:

- `id` (string): sequence identifier (currently the Rust function name)
- `input_sources` (array of `InputBinding`)
- `items` (array of `SequenceItem`)

Example:

```json
{
  "id": "main",
  "input_sources": [ /* one per sequence parameter */ ],
  "items": [ /* ordered calls */ ]
}
```

#### `SequenceDef.input_sources`

`input_sources[i]` describes where the \(i\)-th sequence input value comes from.

- Producers MUST emit one `InputBinding` per sequence parameter (0-based index).
- In the current implementation, **all sequence inputs are marked as `external`**.

### `SequenceItem`

Each element of `SequenceDef.items` MUST be an object with:

- `item_type` (string): `"tile"` or `"sequence"`
- `item_id` (string): identifier of the callee
- `input_sources` (array of `InputBinding`): one per argument

Example:

```json
{
  "item_type": "tile",
  "item_id": "exclaim",
  "input_sources": [
    { "source": { "type": "item_output", "item_index": 0, "output_index": 0 } }
  ]
}
```

#### How `item_type` is chosen today

The current builder chooses `item_type` by checking whether `item_id` matches a discovered tile id or a discovered sequence id:

- If it matches a discovered tile: `item_type = "tile"`.
- Else if it matches a discovered sequence: `item_type = "sequence"`.
- Else: `item_type = "tile"` (fallback).

Consumers MUST NOT assume that `item_type` is fully validated or correct in the presence of missing/partial discovery.

### `InputBinding`

An `InputBinding` MUST be an object with:

- `source` (object): an `InputSource`

Example:

```json
{ "source": { "type": "seq_input", "input_index": 0 } }
```

### `InputSource`

`InputSource` is encoded as an object with a string tag field `type`.

Supported variants in the current schema:

#### External

```json
{ "type": "external" }
```

Meaning:

- The value is provided externally at runtime (e.g., as a sequence input), or
- The compiler could not resolve the argument to a sequence parameter or prior binding and fell back to `external`.

#### Sequence input reference

```json
{ "type": "seq_input", "input_index": 0 }
```

Meaning:

- The value is the `input_index`-th input to the surrounding sequence (0-based).

#### Prior item output reference

```json
{ "type": "item_output", "item_index": 3, "output_index": 0 }
```

Meaning:

- The value is `output_index` from the item at position `item_index` within the same `SequenceDef.items` array (both 0-based).

## Binding resolution behavior (as produced today)

The compiler-side resolver constructs bindings as follows:

- If an argument token exactly matches a sequence parameter name, it MUST be encoded as `seq_input` with that parameter’s index.
- Else if it matches a previously bound variable name, it MUST be encoded as `item_output` pointing at the producing item and output index.
- Else it MUST be encoded as `external` (fallback).

The current resolver binds only a single result name per call:

- If a call is of the form `let x = callee(...);`, then `x` is recorded as coming from `item_output { item_index = <this call>, output_index = 0 }`.

## Recursion representation

Raster’s current CFS has **no representation** for recursive execution intent.

Current behavior:

- The compiler-side call extractor records only normal function calls like `callee(args)`.
- Macro invocations like `callee!(args)` are not recorded as calls and therefore do not appear in the CFS.

## Control-flow expressiveness (what CFS can represent today)

In the current `raster_core::cfs` schema, sequence structure is represented as a **linear list** of items (`SequenceDef.items`) plus data-flow bindings.

- Producers MUST interpret `SequenceDef.items` as ordered steps.
- The CFS format does not include any representation for conditional branching, looping, or early exit within a sequence.

Note: `crates/raster-core/src/schema.rs` defines a separate `ControlFlow` enum (`Linear`, `Conditional`, `Loop`) as future-facing schema, but the current compiler/CLI path does not emit or consume it for CFS.

## Schema invariants

The following invariants describe the CFS produced by the current implementation and the constraints consumers should enforce if they validate a CFS.

### Structural invariants

- `tiles` MUST be an array of unique tile ids.
- `sequences` MUST be an array of unique sequence ids.
- `TileDef.inputs` and `TileDef.outputs` MUST be non-negative integers.
- `SequenceDef.items` MUST preserve author order (the order in the sequence body).

Note: uniqueness and ordering are not currently validated or canonicalized by the implementation.

### Index invariants for `InputSource`

If a consumer validates bindings, it SHOULD enforce:

- For `seq_input`:
  - `input_index` MUST be `< SequenceDef.input_sources.len()`.
- For `item_output`:
  - `item_index` MUST be `< current_item_index` (must refer to an earlier item).
  - `output_index` MUST be `< outputs(item_index)` where `outputs(item_index)` is the callee’s output arity.

Implementation notes:

- The current resolver only produces `item_output` references to previously-seen bindings, so `item_index < current_item_index` holds for produced CFS.
- The current resolver always uses `output_index = 0` for bindings, regardless of actual multi-output arity (see “Known gaps”).

## Known gaps and mismatches (implementation vs intended)

These are places where the current code either does not encode the desired information or does not enforce the natural invariants of the schema:

- **No recursive-call intent is encoded**
  - There is no `is_recursive` (or similar) field in `SequenceItem`, and the compiler does not currently extract `callee!(...)` macro invocations as calls.
- **Multi-output bindings are not modeled**
  - Tiles can report `outputs > 1` via signature parsing, but the binding resolver records only a single named binding per call and always maps it to `output_index = 0`.
- **Call extraction and binding resolution are intentionally narrow**
  - Source parsing uses `syn`, but the call extractor only records calls whose callee is a bare identifier (no `::` paths, no method calls, no macro invocations).
  - Binding resolution is based on exact string equality between an argument’s stringified token form and known parameter/binding names; complex expressions and literals typically become `external` bindings.
- **Discovery scope**
  - The CLI discovery scans only `root/src/**.rs` under the selected project root directory.
- **Ordering is not canonicalized**
  - Discovery walks the filesystem; the emitted array order is dependent on directory iteration order and source scan order.
  - Consumers MUST NOT treat array order as stable across machines unless the producer adds sorting (not implemented today).
- **No structured control-flow constructs**
  - Although `raster_core::schema::ControlFlow` exists, the emitted CFS does not encode branching/loops; only linear item lists are representable.

## Error cases

### Producer-side errors (CFS generation)

When generating a CFS by scanning a project directory, the producer MUST fail with an error in at least the following cases:

- Output file cannot be created or written due to I/O errors.

**Implementation note (current code)**:

- Missing/empty `src/` typically results in “no functions discovered” (and therefore an empty CFS), not a structured error.
- Some parsing failures currently surface as panics (`unwrap`) during AST construction rather than as `raster_core::Error` values.

### Consumer-side errors (CFS consumption)

If a consumer validates or deserializes a CFS, it SHOULD reject inputs that are not well-formed or not meaningful, including:

- Invalid JSON (syntax errors, non-UTF-8 bytes).
- Missing required fields (`version`, `project`, `encoding`, `tiles`, `sequences`).
- Wrong JSON types for required fields.
- Unknown `InputSource.type` variants.
- Out-of-range indices in `seq_input` / `item_output` references.

## End-to-end example

Example CFS JSON for a simple sequence:

```json
{
  "version": "1.0",
  "project": "hello-tiles",
  "encoding": "postcard",
  "tiles": [
    { "id": "greet", "type": "iter", "inputs": 1, "outputs": 1 },
    { "id": "exclaim", "type": "iter", "inputs": 1, "outputs": 1 }
  ],
  "sequences": [
    {
      "id": "main",
      "input_sources": [
        { "source": { "type": "external" } }
      ],
      "items": [
        {
          "item_type": "tile",
          "item_id": "greet",
          "input_sources": [
            { "source": { "type": "seq_input", "input_index": 0 } }
          ]
        },
        {
          "item_type": "tile",
          "item_id": "exclaim",
          "input_sources": [
            { "source": { "type": "item_output", "item_index": 0, "output_index": 0 } }
          ]
        }
      ]
    }
  ]
}
```