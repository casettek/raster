//! Build script for raster-prover.
//!
//! This script compiles the Init and Transition guest crates for RISC0 zkVM
//! and embeds their ELF binaries and method IDs into the crate.

fn main() {
    // Only build guests when risc0-build is available
    #[cfg(feature = "risc0-build")]
    {
        build_guests();
    }

    // Always rerun if guest sources change
    println!("cargo:rerun-if-changed=guests/init/src/main.rs");
    println!("cargo:rerun-if-changed=guests/init/Cargo.toml");
    println!("cargo:rerun-if-changed=guests/transition/src/main.rs");
    println!("cargo:rerun-if-changed=guests/transition/Cargo.toml");
}

#[cfg(feature = "risc0-build")]
fn build_guests() {
    use std::collections::HashMap;
    use std::env;
    use std::fs;
    use std::path::PathBuf;

    // Get the output directory
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));

    // Build the Init guest
    let init_guest = risc0_build::GuestOptions::default();
    let guests = risc0_build::embed_methods_with_options(HashMap::from([
        ("guests/init", init_guest.clone()),
        ("guests/transition", init_guest),
    ]));

    // Write the generated code to a file
    let methods_path = out_dir.join("methods.rs");
    fs::write(&methods_path, &guests).expect("Failed to write methods.rs");

    // Export the path for inclusion
    println!("cargo:rustc-env=RASTER_PROVER_METHODS={}", methods_path.display());
}
