# Program Bundle and Manifests

This document describes the on-disk artifact layout produced by the Raster toolchain during compilation, and the JSON manifests used to describe compiled tile artifacts.

Raster currently produces **an artifact directory tree**, not a single-file “bundle” container. Where this spec describes a “bundle”, it refers to that directory tree.

## Code audit tasks (where to look)

- **Artifact root (output directory selection)**
  - `crates/raster-cli/src/commands.rs`: `output_dir()` returns `./target/raster`
  - `crates/raster-cli/src/commands.rs`: `cfs()` writes `./target/raster/cfs.json`
- **Artifact layout & manifest write/read**
  - `crates/raster-compiler/src/builder.rs`
    - `write_tile_artifacts()` writes `guest.elf`, `method_id`, and `manifest.json`
    - `needs_compilation()` reads `manifest.json` and compares `source_hash`
    - `load_cached_compilation()` reads `guest.elf` + `method_id` (does not read `manifest.json`)
  - `crates/raster-compiler/src/builder.rs`: `TileManifest` (the manifest schema written by the compiler)
- **Backend naming and compilation outputs**
  - `crates/raster-backend/src/backend.rs`: `trait Backend::name()`, `CompilationOutput`
  - `crates/raster-backend/src/native.rs`: native backend produces empty ELF and a placeholder method id
  - `crates/raster-backend-risc0/src/risc0.rs`: RISC0 backend computes image id from ELF and writes artifacts
  - `crates/raster-backend-risc0/src/guest_builder.rs`: also writes `guest.elf`, `method_id`, `manifest.json` (a smaller schema) when used directly
- **Project-level manifests (in-memory types; currently not written as a bundle file)**
  - `crates/raster-core/src/manifest.rs`: `Manifest { name, version, tiles, sequences }`
  - Consumers may also need `crates/raster-core/src/tile.rs` (`TileMetadata`) and `crates/raster-core/src/schema.rs` (`SequenceSchema`)

## Artifact root (“bundle” root)

An implementation producing Raster compilation artifacts MUST choose an **artifact root directory**.

The Raster CLI chooses:

- `artifact_root = <project_root>/target/raster`

Implementations MAY allow the artifact root to be configured, but the internal layout relative to the artifact root MUST remain consistent with this specification if interoperability is desired.

## Bundle layout (directory tree)

The artifact root directory MUST contain a `tiles/` directory. Each tile’s artifacts are stored under:

`tiles/<tile_id>/<backend_name>/`

Where:

- `tile_id` is the tile’s string identifier (as used by discovery/metadata).
- `backend_name` is `Backend::name()` (e.g. `"native"`, `"risc0"`).

### Tile artifact directory contents

Within `tiles/<tile_id>/<backend_name>/`, an implementation that writes artifacts via the compiler MUST produce:

- **`method_id`** (file, UTF-8 text): hex-encoded method identifier bytes
- **`manifest.json`** (file, UTF-8 JSON): tile artifact manifest (schema below)

Additionally, the directory MAY contain:

- **`guest.elf`** (file, binary): compiled guest ELF. This SHOULD exist for zkVM backends (e.g. RISC0). For the native backend, it is typically absent because the compiler emits an empty ELF.

If `guest.elf` is absent, `manifest.json.elf_size` MUST be `0`.

## Tile artifact manifest (`manifest.json`)

The tile artifact manifest is a JSON object produced by the compiler. Its schema matches `raster_compiler::TileManifest`.

### Required fields

An implementation writing a tile manifest MUST include:

- **`tile_id`** (string): tile id for the artifact directory
- **`backend`** (string): backend name (e.g. `"native"`, `"risc0"`)
- **`method_id`** (string): hex-encoded method id bytes (same value written to `method_id`)
- **`elf_size`** (number): size in bytes of `guest.elf`, or `0` if absent

### Optional fields

- **`source_hash`** (string, optional): a hash used for cache invalidation by the compiler

If present, `source_hash` MUST be treated as a cache key only. It MUST NOT be treated as a cryptographic commitment, nor as an artifact identity.

### `source_hash` algorithm (cache invalidation)

The compiler computes `source_hash` from the bytes of a source file path it associates with the tile. The algorithm is a fast, non-cryptographic checksum-like function and includes the file length.

As a result:

- `source_hash` SHOULD be used only to decide whether a tile needs recompilation.
- If the tool cannot compute a source hash, it MAY omit `source_hash`; consumers MUST treat missing `source_hash` as “needs recompilation” if they rely on this field.

## `method_id` file

The `method_id` file MUST contain a hex string encoding of the method id bytes.

Consumers reading `method_id` SHOULD ignore leading/trailing ASCII whitespace (e.g. by trimming) prior to hex decoding.

### Backend-specific meaning

- For the RISC0 backend, `method_id` is the RISC0 image id computed from the ELF.
- For the native backend, the backend currently returns a placeholder method id derived from the tile id string bytes. Consumers MUST NOT assume this value is a zkVM image id.

## Locating artifacts (how runners/verifiers find things)

Given:

- `artifact_root`
- `tile_id`
- `backend_name`

Consumers MUST locate artifacts at:

- `artifact_dir = <artifact_root>/tiles/<tile_id>/<backend_name>/`
- `manifest_path = <artifact_dir>/manifest.json`
- `method_id_path = <artifact_dir>/method_id`
- `elf_path = <artifact_dir>/guest.elf` (if present)

Current Raster CLI execution for the RISC0 backend does not discover artifacts by scanning the artifact root. Instead, it uses the compiler to (re)build a tile and obtains `TileArtifact { elf_path, method_id, artifact_dir }` directly from the compiler.

## Project-level manifest (`raster_core::manifest::Manifest`)

Raster defines a project-level `Manifest` type:

- `name` (string)
- `version` (string)
- `tiles` (array of `TileMetadata`)
- `sequences` (array of `SequenceSchema`)

### Gap: no project bundle manifest is written today

The current CLI/compiler pipeline does **not** write a project-level manifest file into `artifact_root` (e.g. `manifest.json` at the root), and there is no implemented “program bundle” file format that packages tiles, schemas, and metadata as a single distributable unit.

Consumers that require a single-file or single-directory “program bundle” format MUST implement additional packaging on top of the artifact tree described in this document.

## Compatibility notes and implementation gaps

- **Two different `manifest.json` writers exist for RISC0 artifacts**:
  - The RISC0 backend’s internal guest builder writes a minimal manifest (`tile_id`, `method_id`, `elf_size`).
  - The compiler’s builder writes the canonical manifest (`tile_id`, `backend`, `method_id`, `elf_size`, optional `source_hash`).
  - Tools reading manifests SHOULD accept both forms, treating missing fields as absent (not an error) where possible.
- **No verifier-side artifact discovery layer exists yet**:
  - There is no implemented API today that, given `(artifact_root, tile_id, backend)`, loads `CompilationOutput` by reading `manifest.json`; the compiler currently reloads cached outputs by reading `guest.elf` and `method_id` directly.
- **Native execution does not consume compiled artifacts**:
  - The native backend does not produce a meaningful ELF and currently does not execute tiles via the registry.

## Examples

### Example directory layout

Assume:

- `artifact_root = ./target/raster`
- `tile_id = "double"`

RISC0:

```text
target/raster/
  tiles/
    double/
      risc0/
        guest.elf
        method_id
        manifest.json
```

Native:

```text
target/raster/
  tiles/
    double/
      native/
        method_id
        manifest.json
```

### Example `manifest.json` (canonical compiler-written form)

```json
{
  "tile_id": "double",
  "backend": "risc0",
  "method_id": "0123abcd...",
  "elf_size": 123456,
  "source_hash": "4d3c2b1a00000000ffffffff00000000"
}
```
