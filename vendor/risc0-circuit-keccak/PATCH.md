# Windows generated-path patch

Source: crates.io `risc0-circuit-keccak` **4.0.6**, with upstream provenance in
`.cargo_vcs_info.json`. The package version, dependencies, circuit sources, and
embedded `.zkr.xz` programs are unchanged.

`build.rs` now formats the canonical path using Rust's debug formatter when
emitting an `include_bytes!` string literal. The original display formatter
inserted raw backslashes from Windows paths (including the `\\?\` prefix),
which Rust interpreted as invalid escape sequences. Debug formatting emits
the quotes and escapes required by a Rust string literal.

Validation: compiling the `prove` feature on Windows compiles the generated
`zkr_table.rs`; all embedded program files are compared with the original
registry package using SHA-256. This does not change the program data or prove
that native Windows proof generation is supported.
