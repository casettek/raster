## Tile Artifact Generation (ELFs)

This document specifies how Raster produces **tile artifacts** that are executable binaries (“guest programs”) for backends that require them, and how those artifacts are laid out on disk.

Today, the only backend that produces a real guest ELF is the **RISC0 backend**. The **native backend** does not produce a guest binary (and its execution path is currently a placeholder); this is documented as a gap below.

### Code audit tasks (where to look)

- **Top-level build orchestration / artifact writing / cache**:
  - `crates/raster-compiler/src/builder.rs`
    - `Builder::{build_from_source, build_tile_with_cache_info}`
    - `Builder::write_tile_artifacts` (final on-disk layout + manifest)
    - `compute_source_hash` + `needs_compilation` (cache invalidation)
- **Backend interface (what a “compilation output” is)**:
  - `crates/raster-backend/src/backend.rs`
    - `trait Backend`
    - `CompilationOutput { elf, method_id, artifact_dir }`
- **RISC0 backend compilation + execution (host-side I/O framing; journal output; method id)**:
  - `crates/raster-backend-risc0/src/risc0.rs`
    - `Risc0Backend::compile_tile` (build guest, read ELF, compute image id)
    - `Risc0Backend::execute_tile` (writes input length + bytes; reads journal)
- **Guest crate generation + toolchain selection (target triple; entrypoint; generated Cargo.toml)**:
  - `crates/raster-backend-risc0/src/guest_builder.rs`
    - `GuestBuilder::generate_guest_main` (guest entrypoint + I/O syscalls)
    - `GuestBuilder::generate_guest_cargo_toml` (guest dependencies + `no_std`)
    - `GuestBuilder::find_risc0_cargo` (toolchain discovery / `rzup`)
    - `GuestBuilder::build_guest` (invokes `cargo build --release --target …`)
    - `GuestBuilder::artifact_dir` + `write_artifacts` (prewrites artifacts)
- **Tile ABI wrapper naming + (de)serialization format**:
  - `crates/raster-macros/src/lib.rs`
    - `#[tile]` macro expansion: wrapper `__raster_tile_entry_<fn_ident>`
    - wrapper uses `::raster::core::postcard::{from_bytes,to_allocvec}`
- **CLI output directory + expected user-facing artifact locations**:
  - `crates/raster-cli/src/commands.rs`
    - `output_dir()` (defaults to `./target/raster`)
    - `build` and `run` commands (how artifacts are surfaced / loaded)

### Tile artifact model

- A **tile artifact** is the backend-specific output of compiling a tile.
- A backend **MAY** produce a guest binary (ELF) and associated identifiers (e.g. method/image IDs).
- Raster tooling **MUST** place artifacts under a deterministic directory rooted at the project-local output directory (see “Artifact layout on disk”).

### Backends and current behavior

#### RISC0 backend (produces a guest ELF)

- The RISC0 backend **MUST** compile each tile into a standalone RISC-V ELF suitable for execution in the RISC0 zkVM.
- The RISC0 backend **MUST** compute a **method ID** as the RISC0 “image ID” derived from the ELF (via `risc0_zkvm::compute_image_id`).
- The RISC0 backend **MUST** treat the bytes committed to the zkVM **journal** as the tile’s public output for that execution.

#### Native backend (does not produce a guest ELF) — gap

- The native backend currently returns an empty `elf` in `CompilationOutput` and does not compile a standalone guest program.
- The native backend’s `execute_tile` currently returns a placeholder result and does not dispatch through the tile registry.

This document still specifies the expected artifact layout for the native backend (because the builder writes manifests/method IDs), but there is currently **no ELF** to execute.

### Toolchain and target requirements (RISC0)

#### Target triple

- Guest programs built for the RISC0 backend **MUST** be compiled for the target triple:
  - `riscv32im-risc0-zkvm-elf`

#### Toolchain discovery

When compiling guest programs, tooling selects a `cargo`/`rustc` pair from the RISC0 toolchain:

- If the environment variable `RISC0_RUST_TOOLCHAIN_PATH` is set, tooling **MUST** look for `cargo` at:
  - `${RISC0_RUST_TOOLCHAIN_PATH}/bin/cargo`
- Otherwise, tooling **MUST** search for an installed RISC0 Rust toolchain under:
  - `${HOME}/.risc0/toolchains/`
  - and pick the “latest” toolchain by directory name ordering.

If no RISC0 toolchain is found, compilation **MUST** fail with a diagnostic instructing the user to install the toolchain (e.g. “install it with: `rzup install`”).

#### Guest crate shape (generated)

For each tile `<tile_id>`, tooling **MUST** generate a temporary Rust crate with:

- A `src/main.rs` that is `#![no_std]` and uses `extern crate alloc`.
- A `Cargo.toml` declaring:
  - `risc0-zkvm = { default-features = false, version = "1.2", … }`
  - `raster = { path = "<absolute-or-fallback-path>", default-features = false }`
  - the user crate as a path dependency with `default-features = false`
  - `[workspace]` (empty) to avoid inheriting a parent workspace.
- A release profile with at least `opt-level = 3`, and `lto = true`.

### Guest entrypoint and ABI (RISC0)

#### Entrypoint

- The generated guest program **MUST** use the RISC0 guest entry macro to define its entrypoint.
- The entrypoint function **MUST** be called `main`.

#### Tile wrapper linkage

- Each tile function annotated with `#[tile(...)]` produces a public ABI wrapper function:
  - `__raster_tile_entry_<tile_fn_ident>(input: &[u8]) -> raster::core::Result<Vec<u8>>`
- The RISC0 guest program **MUST** call this wrapper and **MUST NOT** call the user’s tile function directly.

Note: guest generation currently derives the wrapper symbol name from the tile ID by replacing `-` with `_`. In practice, current tile IDs come from Rust identifiers and therefore do not contain `-`; if that changes, wrapper symbol naming rules will need to be made explicit and consistent across discovery and macro expansion.

#### Input framing (host → guest)

The guest program expects a two-part input stream:

- First: a 32-bit unsigned integer `input_len`.
- Second: `input_len` raw bytes (`input`).

Accordingly:

- The host **MUST** write `input_len` (as a `u32`) into the executor environment before the raw bytes.
- The host **MUST** then write exactly `input_len` bytes.
- The guest **MUST** read `input_len` as `u32`, allocate a buffer of that length, then read exactly that many bytes into the buffer.

The bytes in `input` are interpreted by the tile ABI wrapper as a `postcard`-encoded value:

- If the tile has no arguments, the wrapper expects a `postcard` encoding of `()`.
- If the tile has one argument of type `T`, the wrapper expects a `postcard` encoding of `T`.
- If the tile has multiple arguments `(A, B, ...)`, the wrapper expects a `postcard` encoding of the tuple `(A, B, ...)`.

#### Output framing (guest → host)

- The tile ABI wrapper returns `output_bytes`, which are a `postcard` encoding of the tile’s return value (or `Ok(inner)` if the tile itself returns `Result<_, _>` and is successful).
- The guest **MUST** commit `output_bytes` to the zkVM journal as a single contiguous slice.
- The host **MUST** treat the journal bytes as the tile execution output.

#### Error behavior — current behavior and gap

Current guest code calls the tile wrapper using `expect("Tile execution failed")`.

- If wrapper returns an error (e.g. deserialization failure, tile returned an error, or serialization failure), the guest **will panic**, and host execution will fail.

Gap:

- There is currently no structured error commitment path (e.g. committing an error enum to the journal). If/when Raster introduces structured errors, this section must be updated to specify what (if anything) is committed publicly on failure.

### What is committed as public output (RISC0)

For a successful execution:

- The public output committed to the zkVM journal **MUST** be exactly the byte vector returned by the tile ABI wrapper.
- That byte vector **MUST** be a `postcard` encoding of the tile’s declared return value.

No other implicit commitments are currently made by Raster in the guest entrypoint.

### Artifact layout on disk

#### Output directory root

Raster CLI and builder code place artifacts under a project-local output directory:

- Output directory root: `./target/raster/`

#### Per-tile artifact directory

For each tile `<tile_id>` and backend `<backend_name>`, Raster tooling **MUST** write artifacts under:

- `./target/raster/tiles/<tile_id>/<backend_name>/`

Examples:

- RISC0: `./target/raster/tiles/my_tile/risc0/`
- Native: `./target/raster/tiles/my_tile/native/`

#### Required files

Within `./target/raster/tiles/<tile_id>/<backend>/`, tooling **MUST** produce:

- `method_id`
  - UTF-8 text containing the method ID hex-encoded (lowercase hex as produced by `hex::encode`).
- `manifest.json`
  - a JSON object describing the produced artifacts (see below).

Tooling **MAY** additionally produce:

- `guest.elf`
  - the guest executable in ELF format, if the backend produces one.

#### `manifest.json` format (current)

The builder writes a manifest with the following keys:

- `tile_id` (string): tile identifier.
- `backend` (string): backend name (e.g. `"risc0"`, `"native"`).
- `method_id` (string): hex string matching the contents of `method_id`.
- `elf_size` (integer): byte length of `guest.elf` if present; `0` if no ELF.
- `source_hash` (string or null/absent): optional source hash used for cache invalidation.

Notes:

- The RISC0 backend also writes a minimal manifest as part of guest building; the builder subsequently writes/overwrites the final manifest described above. Implementations should treat the builder’s `manifest.json` as authoritative.

#### Cache invalidation hash — gap

The `source_hash` field is used only for cache invalidation today:

- It is computed from the tile’s source file using a simple checksum-like routine and the file length.
- It is not specified as cryptographically secure and is not suitable as a stable, collision-resistant identifier.

Gap:

- Reproducible builds and stable artifact identity are not fully specified here; see “Artifact Identity and Reproducibility” for the intended direction once implemented.

### Worked examples

#### Example: directory layout after building a tile with RISC0 backend

Assuming tile ID `greet`:

- `target/raster/tiles/greet/risc0/guest.elf`
- `target/raster/tiles/greet/risc0/method_id`
- `target/raster/tiles/greet/risc0/manifest.json`

`method_id` contains the hex image ID computed from `guest.elf`.

#### Example: RISC0 guest I/O sequence (conceptual)

Host:

- serialize tile input to `input_bytes` using `postcard`
- write `u32(input_bytes.len())`
- write `input_bytes`

Guest:

- read `u32` length
- read that many raw bytes
- call `__raster_tile_entry_<tile>(raw_bytes)`
- commit returned bytes to journal

