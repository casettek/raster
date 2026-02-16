## Compile: Compilation Pipeline

This document specifies the **current Raster compilation pipeline as implemented today**, including its stage boundaries, on-disk outputs, caching/incremental behavior, and the failure modes surfaced by the compiler and CLI.

Where the desired pipeline differs from what exists (e.g., a richer IR, stronger validation, and a program-bundle writer), this document explicitly calls out those gaps.

---

## Code audit tasks (where to look)

### Pipeline entry points (CLI)

- `crates/raster-cli/src/main.rs`
  - `Commands::Build` → `commands::build(...)`
  - `Commands::Cfs` → `commands::cfs(...)`
- `crates/raster-cli/src/commands.rs`
  - `output_dir()` selects `./target/raster` as artifact root
  - `build(...)` drives tile discovery + compilation and prints artifact locations
  - `cfs(...)` drives CFS generation and writes `./target/raster/cfs.json`

### Parse/collect (source discovery) stage

- `crates/raster-compiler/src/ast.rs`
  - `ProjectAst::new(...)` parses `./src/**/*.rs` with `syn` and extracts functions/macros/calls
- `crates/raster-compiler/src/tile.rs`
  - `TileDiscovery::new(...)` selects `#[tile]` functions and reads `kind`/metadata
- `crates/raster-compiler/src/sequence.rs`
  - `SequenceDiscovery::new(...)` selects `#[sequence]` functions and resolves steps from extracted call infos
  - **Important limitations** of call extraction (only bare-identifier calls; no method calls, no `::` paths, no macro invocations) that impact what the pipeline can “see”

### “IR” (in-memory structures) stage

- `crates/raster-compiler/src/ast.rs`
  - `ProjectAst`, `FunctionAstItem`, `CallInfo`
- `crates/raster-compiler/src/tile.rs` / `crates/raster-compiler/src/sequence.rs`
  - `Tile<'ast>`, `Sequence<'ast>`, and `SequenceStep<'ast>`
- `crates/raster-compiler/src/flow_resolver.rs`
  - `FlowResolver` maps variable names → `InputBinding` sources

### CFS stage

- `crates/raster-compiler/src/cfs_builder.rs`
  - `CfsBuilder::build()` orchestrates tile+sequence discovery and produces `ControlFlowSchema`
- `crates/raster-core/src/cfs.rs`
  - `ControlFlowSchema`, `TileDef`, `SequenceDef`, `SequenceItem`, `InputBinding`, `InputSource`

### Tile compilation + artifact write stage

- `crates/raster-compiler/src/builder.rs`
  - `Builder::build_from_source()` compiles all discovered tiles (with caching)
  - `Builder::build_tile_with_cache_info(tile_id)` compiles one tile (with caching)
  - `write_tile_artifacts(...)` writes `guest.elf` (if any), `method_id`, `manifest.json`
  - Cache logic: `needs_compilation(...)`, `compute_source_hash(...)`, `load_cached_compilation(...)`
- `crates/raster-backend/src/backend.rs`
  - `trait Backend` and `CompilationOutput`
- `crates/raster-backend/src/native.rs`
  - Native backend “compilation” placeholder behavior
- `crates/raster-backend-risc0/src/risc0.rs`
  - RISC0 backend: guest build → ELF → image id (“method id”)
- `crates/raster-backend-risc0/src/guest_builder.rs`
  - Guest crate generation (main wrapper) and building using the RISC0 toolchain

### Program bundle / manifests (current behavior vs desired)

- `specs/Core/2. Compile/05. Program Bundle and Manifests.md`
  - Documents the artifact directory tree produced today (and notes the absence of a single-file bundle format)
- `crates/raster-core/src/manifest.rs`
  - `Manifest { name, version, tiles, sequences }` exists as an in-memory type, but is not written by the current compiler/CLI pipeline.
- `crates/raster-compiler/src/schema_gen.rs`
  - Sequence schema generation is **not implemented** (`todo!()`), which constrains any “bundle” writer today.

---

## Spec output

### Pipeline overview (stages and their contracts)

Raster’s implemented “compile” functionality is split into two top-level pipelines that share a project root:

- **Tile compilation pipeline**: produces per-tile artifacts under the artifact root.
- **CFS generation pipeline**: produces a `ControlFlowSchema` JSON file describing discovered tiles/sequences and an approximate dataflow wiring.

In the idealized pipeline vocabulary (parse/collect → validate → IR → CFS → build guest artifacts → bundle), the implementation currently maps as follows:

- **Parse/collect**: `TileDiscovery` + `SequenceDiscovery` (string scanning)
- **Validate**: partial and mostly implicit (see “Validation gaps”)
- **IR**: `ProjectAst`/`FunctionAstItem`/`CallInfo` plus derived `Tile<'ast>`/`Sequence<'ast>` discovery views (and `FlowResolver`’s in-memory bindings)
- **CFS**: `ControlFlowSchema` (produced by `CfsBuilder`)
- **Build guest artifacts**: `Builder` + chosen `Backend` + artifact writer
- **Bundle**: not implemented as a project-level bundle writer (artifact tree exists; see below)

### Common inputs

An implementation of these pipelines MUST be given:

- **`project_root`**: a directory containing a Rust crate with a `src/` directory.
- **`artifact_root`**: a directory where artifacts are written.

The Raster CLI currently uses:

- `project_root = current working directory`
- `artifact_root = <project_root>/target/raster`

### Stage A: Parse/collect tiles (source discovery)

#### Inputs

- `project_root`

#### Process

The tool scans `project_root/src/` recursively and parses Rust source files with `syn`.

A function is treated as a tile definition when it has a `#[tile(...)]` (or `#[raster::tile(...)]`) attribute.

The discovery step extracts:

- `tile_id`: the Rust function name
- `input_count`: the number of parameters in the parsed signature (`syn`)
- `output_count`: `0` if there is no return type, otherwise `1` (current limitation: no tuple/multi-output arity detection)
- `tile_type`: parsed from `kind = iter|recur` if present, otherwise defaults to `"iter"`
- optional metadata: `description`, `estimated_cycles`, `max_memory` (if present as key/value pairs in `#[tile(...)]`)

#### Outputs

- A list of discovered tiles (compiler-side `Tile<'ast>` records), suitable for lowering into CFS `TileDef`s.

#### Failure cases

- Output file creation/writes (e.g., CFS emission) can fail with I/O errors.
- Some parse/AST construction failures currently surface as panics (`unwrap`) rather than structured errors.

#### Implementation gaps / caveats

- Tile kind is read only from the `kind = ...` key/value form. Positional forms like `#[tile(recur)]` are not interpreted as setting the kind.
- Output arity is currently modeled only as `0` vs `1` (no tuple/multi-output arity detection).

### Stage B: Parse/collect sequences (source discovery)

#### Inputs

- `project_root`

#### Process

Sequences are discovered from parsed Rust source (`syn`) by selecting function items with a `#[sequence(...)]` (or `#[raster::sequence(...)]`) attribute.

The discovery step extracts:

- `sequence_id`: the function name
- `param_names`: parameter names from `syn` patterns (best-effort; complex patterns may not be meaningful for binding resolution)
- `calls`: a linear list of recorded call expressions whose callee is a bare identifier (no `::` paths, no method calls, no macro invocations)

#### Outputs

- A list of discovered sequences (compiler-side `Sequence<'ast>` records), suitable for lowering into CFS `SequenceDef`s.

#### Failure cases

- Output file creation/writes (e.g., CFS emission) can fail with I/O errors.
- Some parse/AST construction failures currently surface as panics (`unwrap`) rather than structured errors.

#### Implementation gaps / caveats

- Call extraction is intentionally narrow (bare-identifier calls only). This means many valid Rust call forms will be invisible to the discovered call list.

### Stage C: “Validation” (current behavior)

Raster’s current pipelines perform limited explicit validation.

An implementation matching today’s behavior MUST apply at most the following checks:

- File-system presence and readability checks as described in discovery stages.
- Backend compilation errors are handled differently depending on API entry point:
  - `Builder::build_from_source()` MUST continue compiling other tiles after a tile compilation error and MAY print a warning for failed tiles.
  - `Builder::build_tile_with_cache_info(tile_id)` MUST fail the operation if the requested tile cannot be discovered or compiled.

#### Validation gaps (important)

Today, the compiler/CFS builder does **not** fully validate:

- that every referenced `callee` in a sequence is a known tile or sequence,
- that argument counts match tile/sequence input arity,
- that result bindings correctly handle multi-output tiles (currently treated as single-output),
- that unresolved expressions are representable (unresolved args are treated as “external” inputs).

Tools consuming the CFS MUST treat it as a best-effort, compiler-produced description rather than a fully typechecked program.

### Stage D: IR (implemented in-memory representation)

There is no separate, typed “compiler IR” module today. The effective IR is:

- `ProjectAst` / `FunctionAstItem` / `CallInfo` (AST-derived function and call information)
- `Tile<'ast>` and `Sequence<'ast>` (discovery views over the AST)
- `FlowResolver`’s produced `raster_core::cfs::SequenceItem` list (resolved/bound call arguments)

This IR is consumed by:

- tile compilation (to locate tiles and supply metadata), and
- `FlowResolver` (to infer approximate dataflow bindings when building a CFS).

### Stage E: CFS generation

#### Inputs

- `project_root`
- a `project_name` string (usually derived from `Cargo.toml`)

#### Process

The builder MUST:

- discover tiles and sequences from source (Stages A and B),
- emit a `ControlFlowSchema` with:
  - `version = "1.0"`
  - `encoding = "postcard"`
  - `project = project_name`
  - `tiles = discovered_tiles.map(TileDef { id, type, inputs, outputs })`
  - `sequences = discovered_sequences.map(SequenceDef { id, input_sources, items })`

For each discovered sequence:

- `SequenceDef.input_sources` MUST contain exactly `input_count` entries, and each entry MUST be `external`.
- The builder MUST compute `SequenceDef.items` by applying `FlowResolver`:
  - For each call, inputs are resolved as:
    - `seq_input(i)` if the argument string matches a parameter name
    - `item_output(j, 0)` if the argument string matches a previously bound variable (single-output assumption)
    - otherwise `external`
  - `SequenceItem.item_type` MUST be `"tile"` if the callee matches a discovered tile, `"sequence"` if it matches a discovered sequence, otherwise `"tile"` (fallback).

#### Outputs

- An in-memory `ControlFlowSchema`.
- The CLI’s `cfs` command MUST serialize this CFS as JSON and write:
  - `artifact_root/cfs.json` by default.

#### Failure cases

- If tiles/sequences cannot be discovered (I/O or missing `src/`), CFS generation MUST fail.
- If `Cargo.toml` cannot be read when deriving `project_name`, the CLI’s `cfs` command MUST fail.

#### Implementation gaps / caveats

- Sequence inputs are currently modeled as `external` even when a parameter name is known; the parameter→index mapping is only used when binding item input sources.
- Recursive execution markers are not currently represented in the emitted `ControlFlowSchema`. The compiler-side call extractor does not treat `callee!(...)` macro invocations as calls, so there is no discovery-level recursion marker to propagate into the CFS today.

### Stage F: Tile compilation (backend compilation)

#### Inputs

- `project_root`
- `artifact_root`
- chosen backend name (currently `"native"` or `"risc0"`)
- tiles discovered from source (Stage A)

#### Process

The compiler MUST compile tiles individually through a `Backend`:

- `Backend::compile_tile(metadata, source_path)` produces a `CompilationOutput { elf, method_id, artifact_dir }`.

Backend-specific behavior:

- **Native backend**:
  - `elf` is empty,
  - `method_id` is currently a placeholder derived from the tile id string bytes,
  - the backend does not produce a guest ELF.
- **RISC0 backend**:
  - a temporary guest crate is generated per tile,
  - the guest reads an input length (u32) and then raw bytes, calls the tile’s ABI wrapper, and commits output bytes to the journal,
  - an ELF is built for `riscv32im-risc0-zkvm-elf` and the image id is computed from the ELF bytes,
  - artifacts are written under the artifact root.

#### Outputs

For each successfully compiled tile, the compiler MUST write the tile artifact directory as described in:

- `specs/Core/2. Compile/05. Program Bundle and Manifests.md`

Concretely:

- `artifact_root/tiles/<tile_id>/<backend_name>/manifest.json`
- `artifact_root/tiles/<tile_id>/<backend_name>/method_id`
- `artifact_root/tiles/<tile_id>/<backend_name>/guest.elf` (if `elf` is non-empty)

#### Failure cases

- If backend compilation fails for a specific tile:
  - `build_from_source()` MUST continue compiling the remaining tiles, and MAY emit warnings.
  - `build_tile_with_cache_info(tile_id)` MUST return an error.
- If artifact directory creation or writes fail, compilation MUST fail for that tile (and follow the continuation semantics above).

### Caching and incremental build behavior

Raster implements **per-tile incremental compilation** based on a cache key derived from a source file’s content.

#### Cache key: `source_hash`

The compiler writes `source_hash` into each tile’s `manifest.json`. The hash is computed from:

- the raw bytes of a single “source path” associated with the tile, using a fast, non-cryptographic checksum-like function plus the file length.

#### Cache validation

A tile is considered cached (no rebuild needed) if and only if:

- `artifact_root/tiles/<tile_id>/<backend_name>/manifest.json` exists and can be parsed as JSON
- `manifest.json.source_hash` exists
- the current source hash for the tile’s associated source path matches the stored `source_hash`
- and `guest.elf` and `method_id` can be read successfully (the compiler loads cached outputs from these files directly)

If any of these conditions fails, the compiler MUST rebuild the tile.

#### Incremental semantics

- The compiler MUST evaluate caching independently for each tile.
- The compiler does not currently track dependencies between source files/modules.

#### Important caveat (current gap)

Because the cache key is derived from **a single source file path**, changes outside that file (e.g., in `src/other_module.rs` that the tile depends on) MAY not invalidate the cache. Implementations that require correctness across Rust module dependencies MUST implement a stronger invalidation strategy than the current one.

### “Bundle” output (current state)

Raster currently produces:

- a **directory tree** of tile artifacts under `artifact_root/tiles/`, and
- a CFS JSON file at `artifact_root/cfs.json` (when invoked).

There is no implemented pipeline stage that writes a single project-level bundle manifest or container format that includes tiles + CFS + schemas as a distributable unit. Tools that require such a bundle MUST add packaging logic beyond what exists today.

---

## Examples

### Example: end-to-end outputs for a project

After running:

- `cargo raster build --backend risc0`
- `cargo raster cfs`

an implementation matching today’s CLI behavior SHOULD produce:

```text
target/raster/
  cfs.json
  tiles/
    <tile_id>/
      risc0/
        guest.elf
        method_id
        manifest.json
    <tile_id_2>/
      risc0/
        guest.elf
        method_id
        manifest.json
```

### Example: CFS dataflow binding behavior (illustrative)

Given a sequence:

```rust
#[sequence]
fn main(name: String) -> String {
    let greeting = greet(name);
    exclaim(greeting)
}
```

the generated CFS items for `main` will bind:

- `greet`’s input to `seq_input(0)` (because `name` matches the first parameter name)
- `exclaim`’s input to `item_output(0, 0)` (because `greeting` is bound to the first call’s output)

If an argument cannot be resolved (e.g., a literal or expression), it will be treated as `external`.
