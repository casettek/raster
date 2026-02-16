## Sequences

This document defines **sequences**: author-authored, ordered compositions of tiles (and optionally other sequences) that the Raster toolchain can discover and compile into a Control Flow Schema (CFS).

### Code audit tasks (where to look)

- **Authoring surface (`#[sequence]`)**
  - `crates/raster-macros/src/lib.rs`
    - `#[proc_macro_attribute] pub fn sequence(...)`
    - `TileCallExtractor` and `is_excluded_function(...)`
    - What it registers: a `SequenceRegistration` containing **only** an ordered `&[&str]` list of call names.
- **Runtime registry shape**
  - `crates/raster-core/src/registry.rs`
    - `SequenceMetadataStatic`, `SequenceRegistration`, `SEQUENCE_REGISTRY`
    - `iter_sequences()`, `find_sequence(...)`
- **Compiler-time sequence discovery (bindings + args)**
  - `crates/raster-compiler/src/ast.rs`
    - `ProjectAst::new(...)` (walks `src/**/*.rs`, parses with `syn`)
    - `CallVisitor` (extracts `CallInfo { callee, arguments, result_binding }`)
  - `crates/raster-compiler/src/sequence.rs`
    - `SequenceDiscovery::new(...)` (filters `#[sequence]` fns and resolves steps to tiles/sequences)
- **Compiler-time dataflow binding resolution**
  - `crates/raster-compiler/src/flow_resolver.rs`
    - `FlowResolver::resolve(...)`
    - `resolve_argument(...)` mapping to `InputSource::{SeqInput, ItemOutput, External}`
    - Item classification: `item_type` becomes `"tile"` vs `"sequence"` based on discovered IDs.
- **CFS representation of sequences**
  - `crates/raster-core/src/cfs.rs`
    - `SequenceDef`, `SequenceItem`
    - `InputBinding`, `InputSource`
- **Gaps / unused or future-facing schema**
  - `crates/raster-core/src/schema.rs`
    - `SequenceSchema` and `ControlFlow` enum exist, but the current compiler path emits `raster_core::cfs::SequenceDef` and does not populate `schema::SequenceSchema` control-flow constructs (see “Gaps” below).
  - `crates/raster-compiler/src/schema_gen.rs`
    - `SchemaGenerator::generate(...)` is currently `todo!()` and does not produce `SequenceSchema`.

---

## Spec output

### 1) What a sequence is

A **sequence** is a Rust function annotated with `#[sequence]` that the Raster toolchain treats as an ordered list of invocations (steps). A step is intended to be either:

- a call to a `#[tile]` function, or
- a call to another `#[sequence]` function (a “nested sequence”).

A sequence has:

- **An ID**: the Rust function name as a string (e.g. `main`).
- **Inputs**: the sequence function parameters, in source order.
- **Items**: a linear, ordered list of calls found in the function body.

### 2) Authoring surface and discovery model

#### 2.1 Sequence declaration

A sequence MUST be declared as a Rust function with a `#[sequence]` attribute.

The toolchain recognizes an optional attribute:

- `#[sequence(description = "...")]`

#### 2.2 Calls that form the sequence items

At compiler time (CFS generation), the current implementation discovers sequence items by **parsing Rust source with `syn`** and extracting call expressions.

As implemented today, a call contributes a `CallInfo` (and therefore can become a CFS item) only when:

- The callee is a **bare identifier** (e.g., `greet(name)`), not:
  - a path-qualified call (e.g., `foo::bar(x)`),
  - a method call (e.g., `obj.bar(x)`),
  - or a macro invocation (e.g., `bar!(x)`).
- The call occurs anywhere inside the function body (nested calls are recorded too), and:
  - If the call is the direct initializer of a `let` binding like `let x = foo(...);`, then `result_binding = Some("x")`.
  - Otherwise `result_binding = None`.

Calls are collected in source order and become a linear list of items after filtering to only those callees that match a discovered tile or sequence.

#### 2.3 Runtime registry vs compiler discovery (important distinction)

The `#[sequence]` proc-macro registers sequences in the runtime `SEQUENCE_REGISTRY` with an ordered list of callee names. This registry list:

- MUST be treated as **best-effort metadata** suitable for host-side introspection and “preview” workflows.
- MUST NOT be treated as the source of truth for compilation, because it does not include bindings, argument sources, or nested-sequence structure.

The compiler’s CFS builder derives sequence items from the parsed project AST and extracted `CallInfo`s, and then resolves dataflow in `crates/raster-compiler/src/flow_resolver.rs`.

In the current codebase, this corresponds to:

- AST construction and call extraction: `crates/raster-compiler/src/ast.rs`
- Sequence discovery: `crates/raster-compiler/src/sequence.rs`
- CFS assembly: `crates/raster-compiler/src/cfs_builder.rs`
- Dataflow resolution: `crates/raster-compiler/src/flow_resolver.rs`

### 3) Binding model and dataflow

This section defines how sequence parameters and intermediate results are bound into the CFS. It matches the current compiler implementation.

#### 3.1 Sequence inputs (parameters)

Sequence input parameters are discovered from the sequence function signature using `syn` patterns.

As implemented today:

- Simple identifiers are supported (e.g., `x: T` or `mut x: T` ⇒ parameter name `"x"`).
- Wildcards are represented as `"_"` (e.g., `_: T`).
- More complex patterns may be stringified (implementation-defined) and will generally not be useful for binding resolution.

In the emitted CFS:

- `SequenceDef.input_sources` MUST contain one `InputBinding::external()` per sequence parameter, in parameter order.
- References to sequence parameters inside item arguments MUST resolve to `InputSource::SeqInput { input_index }` (0-based parameter index).

#### 3.2 Result bindings from `let` statements

For a statement of the form:

```rust
let name = callee(args...);
```

the discovery phase records `result_binding = Some("name")`.

Current constraints (as implemented today):

- The binding MUST be a simple identifier (e.g. `greeting`).
- Destructuring patterns (e.g. `let (a, b) = ...`) are not supported.
- `mut` on `let` bindings is supported (e.g. `let mut x = ...` still records `result_binding = Some("x")`).

#### 3.3 Argument parsing and input-source resolution

Each call’s arguments are recorded as a list of strings captured from the parsed AST (each argument expression is stringified via token printing).

For each argument string `arg`, input-source resolution proceeds as follows:

- If `arg` exactly matches a sequence parameter name, it MUST resolve to `InputSource::SeqInput { input_index }`.
- Else if `arg` exactly matches a previously-recorded `let` binding name, it MUST resolve to `InputSource::ItemOutput { item_index, output_index }`.
  - `output_index` is currently always `0` (single-output assumption; see “Gaps and divergences”).
- Otherwise, it MUST resolve to `InputSource::External`.

Implications:

- Arguments SHOULD be written as bare identifiers that refer either to a sequence parameter or to a prior `let` binding.
- Literal arguments (e.g. `0`, `"hi"`) and complex expressions (e.g. `x + 1`) are currently treated as `External` inputs in the CFS.
- Arguments that are macro expressions (e.g., `vec![1, 2]`) are captured as strings but will typically not resolve to `seq_input`/`item_output`, so they also fall back to `External`.

### 4) Item typing: tile vs nested sequence

Each discovered call becomes a `SequenceItem` in the CFS. The `item_type` is assigned as follows:

- If the callee matches a discovered tile ID, `item_type` MUST be `"tile"`.
- Else if the callee matches a discovered sequence ID, `item_type` MUST be `"sequence"`.
- Otherwise, it defaults to `"tile"`.

### 5) Macro-call / “bang” syntax

The current compiler-side call extraction records only normal function calls like `callee(args)`. Macro invocations like `callee!(args)` are not recorded as calls and do not appear in the emitted CFS.

### 6) Current semantics: linear sequences only

The compiler-emitted sequence semantics are **linear**:

- Items MUST be executed in the order they appear in `SequenceDef.items`.
- There is no compiler-emitted conditional branching, looping, or early-exit structure in CFS for sequences today.

### 7) Examples

#### 7.1 Simple linear sequence with a binding

```rust
use raster::{sequence, tile};

#[tile]
fn greet(name: String) -> String {
    format!("Hello, {name}")
}

#[tile]
fn exclaim(s: String) -> String {
    format!("{s}!")
}

#[sequence(description = "Greet and add punctuation")]
fn main(name: String) -> String {
    let greeting = greet(name);
    exclaim(greeting)
}
```

Expected CFS-level dataflow:

- `greet(name)` consumes `SeqInput[0]`
- `exclaim(greeting)` consumes `ItemOutput(item_index=0, output_index=0)`

#### 7.2 Nested sequences (sequence calling another sequence)

```rust
use raster::{sequence, tile};

#[tile]
fn add_one(x: u64) -> u64 { x + 1 }

#[tile]
fn double(x: u64) -> u64 { x * 2 }

#[sequence]
fn inner(x: u64) -> u64 {
    add_one(x)
}

#[sequence]
fn outer(x: u64) -> u64 {
    let y = inner(x);
    double(y)
}
```

Expected CFS-level item typing:

- In `outer`, `inner(x)` SHOULD become a `SequenceItem` with `item_type = "sequence"` when `inner` is also discovered as a sequence.
- `double(y)` becomes a `SequenceItem` with `item_type = "tile"`.

#### 7.3 Unsupported binding patterns (current limitation)

```rust
#[sequence]
fn bad(x: u64) -> u64 {
    // Destructuring patterns are not recorded as a single binding name, so `y`
    // won't resolve to the prior output in the current flow resolver:
    let (y, _z) = split(x);
    double(y) // `y` will fall back to External.
}
```

This SHOULD be avoided until the compiler’s binding parser is strengthened.

---

## Gaps and divergences (code vs intended design)

- **No structured control flow in CFS sequences**: `raster_core::schema::ControlFlow` contains variants like `Conditional` and `Loop`, but the current compiler path emits only a linear `cfs::SequenceDef.items` list and does not encode conditionals/loops.
- **`SequenceSchema` generation is not implemented**: `crates/raster-compiler/src/schema_gen.rs` is stubbed and does not currently generate `raster_core::schema::SequenceSchema` from source.
- **Multi-output tiles are not supported in sequence bindings**: the flow resolver currently assumes a single output (`output_index = 0`) for any bound result and does not model destructuring or selecting specific tuple elements.
- **No “bang call” / recursion marker in schemas**: the compiler call extractor does not treat `callee!(...)` macro invocations as calls, so there is no recursion marker to propagate into the CFS today.
- **`cargo raster preview` is not CFS execution**: the preview command walks a discovered sequence (expanding nested sequences inline) and executes tiles in that flattened order, but it does not use CFS bindings as an execution plan and currently feeds the same CLI `--input` bytes to each tile runner.
