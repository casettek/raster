pub mod authorization;
pub mod encode;

/// Whether the CLI should print raw toolchain output — child `cargo build`
/// progress, dependency warnings, guest-build chatter — alongside its own
/// results.
///
/// Off by default: those lines belong to the toolchain, not to the answer the
/// command was run for, and they bury it. Build output is still shown in full
/// when a build actually fails. `RASTER_VERBOSE=1` restores everything.
pub fn verbose_output() -> bool {
    std::env::var_os("RASTER_VERBOSE").is_some_and(|value| !value.is_empty() && value != "0")
}

/// Silence the guest-build progress that `risc0-build` writes *directly to the
/// terminal*, bypassing our capture of the child's stdio.
///
/// Its `tty_println` opens `/dev/tty` on purpose ("HACK: Attempt to bypass the
/// parent cargo output capture"), so piping the child is not enough — the only
/// lever is `RISC0_GUEST_LOGFILE`, which redirects that stream to a file of our
/// choosing. Pointing it at `/dev/null` is what keeps `transition-guest: …`
/// blocks out of every command that happens to rebuild `raster-prover`.
///
/// The cost: if a *guest* build fails, its rustc errors go to the same stream
/// and are lost — hence the `RASTER_VERBOSE=1` hint on build failures.
pub fn quiet_guest_build(command: &mut std::process::Command) {
    if !verbose_output() {
        command.env("RISC0_GUEST_LOGFILE", "/dev/null");
    }
}

/// Appended to build-failure errors: the captured cargo output is shown, but
/// guest-build detail needs the env var.
pub const VERBOSE_HINT: &str = "re-run with RASTER_VERBOSE=1 for full toolchain output";
