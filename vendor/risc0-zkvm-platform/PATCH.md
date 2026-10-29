# Windows host linkage patch

Source: crates.io `risc0-zkvm-platform` **2.2.3**, as recorded in
`.cargo_vcs_info.json`. The package version and dependency requirements are
unchanged. Upstream source files retain their Apache-2.0 copyright headers.
The upstream crate archive checksum is
`2eb37a97ff7e8e4ee1b2a1c43ec143b4887759883c343507af9e4787a57914cd`.

The only runtime source change is in `src/syscall.rs`: on Windows,
`sys_alloc_words` explicitly rejects execution instead of referencing the zkVM
allocator symbol. MSVC otherwise reports LNK2019 for `sys_alloc_aligned` when
linking the syscall object, even when the deprecated allocator wrapper is never
called. This is consistent with the existing unsupported host syscall stubs;
it does not supply a host allocator or simulate a guest syscall.

The original function body remains active on every other target, including Linux
and `target_os = "zkvm"`. The patch does not alter guest allocation logic or
commitment algorithms. Native host tests do not establish proof generation
support or guest image identity; those require separate proof-path validation.

`tests/host_link.rs` retains a syscall function pointer without calling a guest
syscall. The equivalent standalone program reproduces the Windows link failure
with the unpatched crate and links successfully with this patch, including with
`export-syscalls` enabled. The regression test also runs on Linux.

Both the Raster and Iris workspace roots need `[patch.crates-io]` entries:
Cargo only reads patches from the workspace root of the build being invoked.
