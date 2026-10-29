# Windows host module selection patch

Source: crates.io `risc0-zkvm` **3.0.6**, with upstream provenance in
`.cargo_vcs_info.json`. Its version and dependency requirements are unchanged.

The actor prover in `src/host/client/prove/default.rs` imports Unix-only
`OwnedFd` and `UnixStream` APIs. It was compiled unconditionally by the client
module, preventing Windows builds even when only the in-process prover was used.

`src/host/client/prove/mod.rs` now includes that module, import, and actor branch
only on Unix, with the corresponding public re-export in `src/lib.rs` also gated.
Explicitly selecting `RISC0_PROVER=actor` on other platforms fails
with an explanatory message. The existing local prover, external prover, and
default selection logic are retained; there is no silent backend substitution.
Native Windows actor transport is not implemented by this patch.

Guest sources and proof verification logic are unchanged. Compiling native host
libraries and running inference tests does not establish end-to-end native
Windows proof generation support.
