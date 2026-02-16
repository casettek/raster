## Compile: Overview

This document defines what “compilation” produces in Raster today, where the compile entrypoints live in the codebase, and the determinism/reproducibility expectations implied by the current implementation.

Where the desired behavior differs from what exists (for example: stronger reproducibility guarantees, a single program bundle writer, and richer validation), this document calls out those gaps explicitly.

---

## Code audit tasks (where to look)

### Compile entrypoints (CLI)

- `crates/raster-cli/src/main.rs`
  - `Commands::Build` → `commands::build(backend, tile)` (this is the “compile tiles” user-facing entrypoint)
  - `Commands::Cfs` → `commands::cfs(output)` (generates a CFS JSON file)
- `crates/raster-cli/src/commands.rs`
  - `output_dir()` selects `./target/raster` as the artifact root
  - `build(...)` performs source discovery → backend compilation → on-disk artifact writes (via `raster-compiler::Builder`)
  - `cfs(...)` performs source discovery → flow resolution → CFS JSON write (via `raster-compiler::CfsBuilder`)

### Compile entrypoints (library APIs)

- `crates/raster-compiler/src/lib.rs`
  - Exposes `Builder`, `CfsBuilder`, `TileDiscovery`, `SequenceDiscovery`, `FlowResolver`
- `crates/raster-compiler/src/builder.rs`
  - `Builder::build_from_source()`: compile all tiles discovered from `./src`
  - `Builder::build_tile_with_cache_info(tile_id)`: compile one tile (with cache check)
- `crates/raster-compiler/src/cfs_builder.rs`
  - `CfsBuilder::build(project_root)`: build an in-memory `ControlFlowSchema` from source discovery
- `crates/raster-backend/src/backend.rs`
  - `trait Backend::compile_tile(...) -> CompilationOutput` (backend-specific compilation)
- `crates/raster-backend/src/native.rs`
  - “native compilation” is a placeholder (no standalone guest artifact)
- `crates/raster-backend-risc0/src/risc0.rs`
  - RISC0 compilation: build guest ELF → compute image id (“method id”) → write artifacts
- `crates/raster-backend-risc0/src/guest_builder.rs`
  - Guest crate generation + invocation of the RISC0 toolchain (`cargo build --release --target riscv32im-risc0-zkvm-elf`)

### Where outputs are defined (types + formats)

- `crates/raster-core/src/cfs.rs`
  - CFS structure (`ControlFlowSchema`, `TileDef`, `SequenceDef`, `SequenceItem`, `InputBinding`, `InputSource`)
- `specs/Core/2. Compile/02. Control Flow Schema (CFS) Format.md`
  - The CFS JSON format spec (this overview only summarizes)
- `specs/Core/2. Compile/03. Tile Artifact Generation (ELFs).md`
  - Artifact file set + their meaning (this overview only summarizes)

---

## Spec output

### What “compilation” produces (top-level)

Given a Raster project root directory (a Rust crate with `./src`), compilation produces:

- **Tile build artifacts** under an artifact root directory (by default `./target/raster`).
- **A Control Flow Schema (CFS)** JSON file describing the discovered tiles/sequences and an approximate dataflow wiring.

These are produced by separate user-facing commands today:

- `cargo raster build` produces **tile artifacts**.
- `cargo raster cfs` produces **CFS JSON**.

The implementation does not currently produce a single “program bundle” artifact that contains both CFS and all tile artifacts in a single file.

### Artifact root directory

The CLI MUST treat the artifact root as `./target/raster` relative to the current working directory.

- In code, this is implemented by `crates/raster-cli/src/commands.rs::output_dir()`.

Implementations MAY allow overriding the artifact root (for example via a CLI flag), but the current CLI does not.

### Tile compilation outputs (per-tile, per-backend)

For each compiled tile id `T` and backend name `B`, the compiler writes artifacts under:

- `./target/raster/tiles/<T>/<B>/`

When a backend returns a non-empty ELF (`CompilationOutput.elf.len() > 0`), the compiler MUST write:

- `guest.elf`: the compiled guest program ELF bytes.

The compiler MUST write:

- `method_id`: hex-encoded bytes of `CompilationOutput.method_id`.
- `manifest.json`: a JSON object containing at least:
  - `tile_id` (string)
  - `backend` (string)
  - `method_id` (string, lowercase hex)
  - `elf_size` (number)
  - `source_hash` (string or null/absent; used only for cache invalidation)

#### Backend-specific notes (as implemented)

- **`native` backend**
  - `CompilationOutput.elf` is empty; therefore `guest.elf` is not written.
  - `CompilationOutput.method_id` is the UTF-8 bytes of the tile id string (it is not a cryptographic hash and is not tied to codegen).
  - This backend’s “compile” step exists mostly to fit the same interface as zkVM backends.

- **`risc0` backend**
  - `CompilationOutput.elf` is a RISC0 guest ELF built via the RISC0 toolchain.
  - `CompilationOutput.method_id` is `risc0_zkvm::compute_image_id(elf)` (the image id derived from the ELF).
  - The backend itself also writes artifacts as part of `compile_tile`, and `raster-compiler::Builder` writes/overwrites artifacts again. Consumers SHOULD treat the `raster-compiler::Builder` outputs as authoritative when using the Raster CLI pipeline.

#### Example: expected artifact tree (RISC0)

```text
target/raster/
  tiles/
    greet/
      risc0/
        guest.elf
        method_id
        manifest.json
```

#### Example: `manifest.json` produced by `raster-compiler::Builder`

```json
{
  "tile_id": "greet",
  "backend": "risc0",
  "method_id": "…hex…",
  "elf_size": 123456,
  "source_hash": "…hex…"
}
```

### CFS generation output

The CFS generation pipeline MUST produce a UTF-8 JSON file containing a serialized `raster_core::cfs::ControlFlowSchema`.

- Default output path (CLI): `./target/raster/cfs.json`
- Override: `cargo raster cfs --output <path>`

The CFS JSON MUST include:

- `version`: currently `"1.0"` (a string)
- `project`: a project name derived from `Cargo.toml` (best-effort)
- `encoding`: currently `"postcard"` (a string)
- `tiles`: list of tile definitions (id, type, input/output arity)
- `sequences`: list of sequences with ordered items and `InputBinding` wiring

#### Example: minimal CFS shape

```json
{
  "version": "1.0",
  "project": "my_project",
  "encoding": "postcard",
  "tiles": [
    { "id": "greet", "type": "iter", "inputs": 1, "outputs": 1 }
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
        }
      ]
    }
  ]
}
```

### Determinism and reproducibility expectations

Compilation is expected to be deterministic at the “definition level” (same declared tiles and sequences) and reproducible at the “artifact level” (same `guest.elf` bytes and therefore same `method_id`) when inputs and toolchains are held constant.

#### Expected determinism (what callers should rely on)

- The compiler MUST treat `method_id` as the stable identifier of a compiled guest program **within a single backend’s semantics**:
  - For `risc0`, `method_id` MUST be derivable from the guest ELF (it is the RISC0 image id).
  - For `native`, `method_id` is currently not tied to codegen and MUST NOT be treated as a content hash.
- If two builds produce identical guest ELFs for the same tile and backend, they MUST produce identical `method_id` bytes.
- The CFS generator MUST preserve the order of items within a sequence as discovered from the source file’s linear call extraction.

#### Current reproducibility gaps (implementation limitations)

The current implementation does not enforce strong, cross-machine reproducibility. In particular:

- **File traversal order is not stabilized**
  - Source discovery uses `std::fs::read_dir` without sorting, so the order of discovered `tiles` and `sequences` MAY vary across filesystems and runs. This can reorder arrays in `cfs.json`.
- **Tile compilation caching is a heuristic**
  - Cache invalidation uses a non-cryptographic “source hash” computed only from the tile’s source file contents and length.
  - Changes to dependencies, toolchain versions, backend versions, environment variables, or build flags MAY not invalidate the cache and can yield stale artifacts.
  - `native` backend compilation produces no `guest.elf`, so the cache loader cannot reload a “compiled” artifact; caching is effectively non-functional for native builds.
- **RISC0 guest builds are not pinned**
  - Guest builds invoke `cargo build` without `--locked`, and the generated guest `Cargo.toml` uses absolute paths for local dependencies. Toolchain and environment differences MAY change resulting ELF bytes (and therefore `method_id`).

Implementations that require reproducibility SHOULD pin toolchains, pin dependency resolution, and stabilize discovery ordering before relying on artifact identity in consensus/security-critical contexts.

