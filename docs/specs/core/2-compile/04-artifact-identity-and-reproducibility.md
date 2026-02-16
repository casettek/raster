# Artifact Identity and Reproducibility

This document specifies how Raster defines the identity of compiled tile artifacts, what changes are considered identity-breaking, and how to approach reproducible builds in the current implementation.

Raster currently has a **backend-defined** notion of artifact identity:

- For the **RISC0** backend, identity is derived from the compiled guest ELF via RISC0’s image-id computation.
- For the **native** backend, the current “method id” value is a placeholder and is not derived from compiled code.

Where the current implementation does not fully support the desired properties (stable identity across machines/time, strong toolchain pinning, reproducible builds), this document calls that out as a **GAP**.

---

## Code audit tasks (where to look)

### Identity / “method id” derivation

- **RISC0 image id computation**
  - `crates/raster-backend-risc0/src/risc0.rs`: `compile_tile()` calls `risc0_zkvm::compute_image_id(&elf)` and persists the result as `CompilationOutput.method_id`.
  - `crates/raster-backend-risc0/Cargo.toml`: pins the host-side `risc0-zkvm` crate dependency (currently `version = "1.2"`).
- **Native backend “method id” placeholder**
  - `crates/raster-backend/src/native.rs`: `compile_tile()` returns `method_id = tile_id.as_bytes().to_vec()` and an empty ELF.
- **Persistence format (hex file + manifest)**
  - `crates/raster-compiler/src/builder.rs`: `write_tile_artifacts()` writes `guest.elf`, `method_id` (hex), and `manifest.json`.
  - `crates/raster-backend-risc0/src/guest_builder.rs`: `write_artifacts()` also writes `guest.elf`, `method_id`, and a minimal `manifest.json` (note: schema differs from the compiler’s).

### What inputs are (and are not) hashed

- **ELF → identity**
  - `crates/raster-backend-risc0/src/risc0.rs`: identity is computed from the **ELF byte string** (not from source files directly).
- **Source file hash used for caching (not identity)**
  - `crates/raster-compiler/src/builder.rs`: `compute_source_hash()` and `TileManifest.source_hash` are used only to decide whether to reuse cached artifacts.

### Toolchain selection / version pinning

- **RISC0 toolchain discovery**
  - `crates/raster-backend-risc0/src/guest_builder.rs`: `find_risc0_cargo()`
    - Uses `RISC0_RUST_TOOLCHAIN_PATH` if set, otherwise selects the “latest” toolchain under `~/.risc0/toolchains` by sorting directory names.
  - `crates/raster-backend-risc0/src/guest_builder.rs`: `build_guest()`
    - Calls RISC0 toolchain `cargo build --release --target riscv32im-risc0-zkvm-elf` and sets `RUSTC` to the toolchain’s `bin/rustc`.
- **Guest crate dependency versions**
  - `crates/raster-backend-risc0/src/guest_builder.rs`: `generate_guest_cargo_toml()`
    - Declares `risc0-zkvm = { version = "1.2", default-features = false }` and `raster = { path = "...", default-features = false }`.
    - Adds the user crate as a path dependency with `default-features = false`.
  - **GAP**: the generated guest crate does not include a checked-in `Cargo.lock`, and the build does not use `cargo --locked`.

---

## Identity: definition and persistence

### 1. Artifact identity (`method_id`)

Each compiled tile artifact MUST have an associated **method id** (stored as raw bytes in memory, and hex-encoded on disk).

- **Definition (opaque bytes)**: `method_id` is an opaque byte string (`Vec<u8>`) supplied by the backend.
- **Persistence format (hex)**:
  - Tools that persist artifacts MUST write `method_id` as a lowercase hex string (two hex digits per byte, no `0x` prefix) to:
    - `tiles/<tile_id>/<backend>/method_id` (text file), and
    - `tiles/<tile_id>/<backend>/manifest.json` under key `"method_id"` (JSON string).
  - Consumers SHOULD trim ASCII whitespace when reading the `method_id` file prior to hex decoding.

### 2. RISC0 backend identity (implemented)

For the RISC0 backend, the method id MUST be computed as:

- **Input**: the compiled guest ELF bytes, as loaded from disk (a single byte string).
- **Algorithm**: `method_id = risc0_zkvm::compute_image_id(elf).as_bytes()`.
- **Stability guarantee**:
  - The method id MUST change if the ELF byte string changes.
  - Raster makes no stronger claim (e.g., “same source implies same method id”) because the guest build is not currently specified or enforced to be reproducible.

### 3. Native backend identity (implemented; placeholder)

For the native backend, Raster currently sets:

- `method_id = tile_id.as_bytes()`
- `elf = []` (empty)

Consumers MUST treat this value as a placeholder identifier only:

- It MUST NOT be treated as a cryptographic commitment.
- It MUST NOT be treated as an identifier of compiled code.

**GAP (identity for native execution)**: The native backend does not currently define an artifact identity derived from compiled code or a canonical program representation.

---

## What changes break identity (RISC0 backend)

Because the RISC0 method id is derived from the guest ELF bytes, any change that changes the produced ELF MUST be treated as identity-breaking. This includes (non-exhaustive):

- **Tile code changes**: changes to the tile function body, signature, or any code transitively referenced by the guest.
- **Macro / ABI wrapper changes**: changes to the `#[tile]`-generated wrapper code that the guest calls.
- **Raster crate changes**: the guest depends on `raster` via a path dependency; changes to that code can affect the guest binary.
- **Dependency resolution changes**:
  - changes to the resolved version of `risc0-zkvm` used by the guest build,
  - changes to transitive dependencies of the guest crate (including registries updating).
- **Toolchain changes**:
  - changes to the RISC0 toolchain (cargo/rustc), target linker, LLVM version, or codegen flags,
  - changes to build environment variables that affect compilation.
- **Profile/flags changes**: changes to `opt-level`, `lto`, panic strategy, debug info, or linker arguments.

Conversely, changes that do not change the produced ELF MAY leave the method id unchanged (e.g., some comment-only source changes), but tools MUST NOT assume this.

---

## Reproducible builds: current behavior and guidance

### 1. Current state (what Raster does today)

- Raster computes RISC0 identity from the **ELF bytes** and persists it as `method_id`.
- Raster does **not** currently implement a stable “artifact hash” independent of the backend (e.g., `sha256(guest.elf)`), nor does it hash a set of input files as part of the identity.
- Raster’s builder caches artifacts using a `source_hash` computed from a **single source file path** associated with the tile.

### 2. Guidance to achieve stable identities in practice (implementer responsibilities)

To make repeated builds likely to produce the same `method_id` for a given tile, implementers SHOULD:

- **Pin the RISC0 toolchain selection**
  - Set `RISC0_RUST_TOOLCHAIN_PATH` to a specific toolchain directory and ensure it is used consistently across builds/machines.
  - Record the exact toolchain identifier out-of-band (e.g., in CI logs or a build metadata file).
- **Pin Rust crate dependency resolution for the guest build**
  - Ensure the guest build resolves the same crate versions every time (ideally via a lockfile + `--locked`).
  - Avoid relying on semver-ranged dependencies without a lockfile for reproducibility over time.
- **Build in a controlled environment**
  - Use a fixed OS/container image and deterministic build environment when possible.
  - Avoid build-time inputs that can vary (e.g., uncontrolled environment variables, toolchain auto-updates).

### 3. Cache key vs identity (important distinction)

`source_hash` in `tiles/<tile_id>/<backend>/manifest.json` is a cache key only.

- Build tooling MUST NOT treat `source_hash` as an artifact identity.
- Build tooling MUST NOT treat `source_hash` as a security commitment.

**GAP (cache correctness)**: The current cache invalidation hashes only one source file (the discovered file containing the `#[tile]` attribute) and does not incorporate:

- transitive Rust module files,
- Cargo feature flags,
- `Cargo.toml`/`Cargo.lock` changes,
- toolchain versions.

As a result, the builder MAY reuse stale artifacts (and therefore stale `method_id`) even if the true compiled guest program would differ after a full rebuild.

---

## Examples

### Example: artifact identity files for a RISC0 tile

For a tile id `double` built with the RISC0 backend under `./target/raster`:

```text
target/raster/
  tiles/
    double/
      risc0/
        guest.elf
        method_id
        manifest.json
```

`method_id` contents are a lowercase hex string, e.g.:

```text
4f3a9c... (two hex chars per byte; no 0x prefix)
```

### Example: why identity changes

If the guest ELF changes because:

- you upgrade the RISC0 toolchain selected from `~/.risc0/toolchains`, or
- the guest’s `risc0-zkvm = "1.2"` dependency resolves to a different patch release over time,

then the produced `method_id` will change accordingly.
