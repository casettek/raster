# Pinned RISC Zero Windows compatibility patches

These crates are copied from the exact crates.io versions used by Iris. Each
directory includes upstream VCS provenance, Apache-2.0 licensing, and `PATCH.md`
with the local changes. Package versions remain unchanged.

The copies are selected by the cargo config `windows.toml`, never by the
workspace `Cargo.toml`. A root `[patch.crates-io]` is hashed into every
project's `guest_inputs` and replaces these crates' registry pins in
`Cargo.lock`; the guest stage, seeded from that lock, then resolves newer
releases and every tile image id moves, on every host. Through the config, only
Windows builds see the patches:

- `cargo raster run` and `cargo raster chain run` pass
  `--config <raster>/vendor/windows.toml` to their child builds on Windows,
  when the CLI was built from a raster checkout;
- build or install the CLI itself with the same flag, e.g.
  `cargo install --path crates/raster-cli --config vendor/windows.toml`.

A patched build rewrites the local `Cargo.lock` without those pins; don't commit
it.

| Crate | Version | Change |
| --- | --- | --- |
| `risc0-zkvm-platform` | 2.2.3 | Reject the guest allocator wrapper on Windows without requiring a guest linker symbol. |
| `risc0-circuit-keccak` | 4.0.6 | Escape Windows paths in generated Rust `include_bytes!` calls. |
| `risc0-zkvm` | 3.0.6 | Compile the Unix actor transport only on Unix; reject actor selection explicitly elsewhere. |

The embedded circuit programs account for most of this directory's size. They
are retained byte-for-byte so the path-formatting patch does not replace or
regenerate proving data.

For the tested MSVC configuration, see the sibling Iris checkout's
`scripts/build-windows.ps1` and `docs/workflows/windows.md`: Rust 1.91.1, an x64
Visual Studio C++ environment, and `/DNOMINMAX /std:c++20 /EHsc`. The inference
build skips generating proof guests with `RISC0_SKIP_BUILD=1` but still compiles
the host prover libraries. Native Windows proof generation remains unvalidated.
