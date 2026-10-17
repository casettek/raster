# Pinned RISC Zero Windows compatibility patches

These crates are copied from the exact crates.io versions used by Iris. Each
directory includes upstream VCS provenance, Apache-2.0 licensing, and `PATCH.md`
with the local changes. Both workspace roots select these copies through
`[patch.crates-io]`; package versions remain unchanged.

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
