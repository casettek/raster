//! Build script for raster-prover.
//!
//! This compiles the transition guest using the RISC0 toolchain directly
//! (same approach as guest_builder in raster-backend-risc0). Key difference:
//! - Tile guests (guest_builder): built at **runtime** when you run/compile a
//!   tile; if the build fails you get an immediate error.
//! - Transition guest (here): built at **cargo build time**; if the build
//!   fails we emit a stub so the crate still compiles, and you get a runtime
//!   error when using transition proving ("ELF was not built").

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=guests/transition/src/main.rs");
    println!("cargo:rerun-if-changed=guests/transition/Cargo.toml");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let guest_dir = manifest_dir.join("guests").join("transition");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let methods_rs = out_dir.join("methods.rs");

    // Skip guest build if requested (for faster iteration)
    let built = if env::var("RISC0_SKIP_BUILD").is_ok() {
        println!("cargo:warning=Skipping transition guest build (RISC0_SKIP_BUILD set)");
        false
    } else {
        match build_transition_guest(&guest_dir, &out_dir) {
            Ok(()) => true,
            Err(e) => {
                println!("cargo:warning=Failed to build transition guest: {}", e);
                false
            }
        }
    };

    // Always write methods.rs so include!() in guest.rs succeeds.
    // When the guest wasn't built, emit a stub so the crate still compiles.
    if built {
        let dest_elf = out_dir.join("transition_guest.elf");
        fs::write(
            &methods_rs,
            format!(
                r#"/// Path to the transition guest ELF (compiled at build time).
pub const TRANSITION_GUEST_ELF_PATH: &str = "{}";

/// The transition guest ELF bytes (embedded at compile time).
pub const TRANSITION_GUEST_ELF: &[u8] = include_bytes!("{}");
"#,
                dest_elf.display(),
                dest_elf.display()
            ),
        )
        .expect("Failed to write methods.rs");
    } else {
        fs::write(
            &methods_rs,
            r#"/// Path to the transition guest ELF (empty when guest was not built).
pub const TRANSITION_GUEST_ELF_PATH: &str = "";

/// The transition guest ELF bytes (empty when guest was not built).
pub const TRANSITION_GUEST_ELF: &[u8] = &[];
"#,
        )
        .expect("Failed to write methods.rs stub");
    }
}

/// Find the RISC0 toolchain's cargo binary.
fn find_risc0_cargo() -> Option<PathBuf> {
    // Check for RISC0_RUST_TOOLCHAIN_PATH env var first (but skip if it points at stable)
    if let Ok(path) = env::var("RISC0_RUST_TOOLCHAIN_PATH") {
        let path_lower = path.to_lowercase();
        if !path_lower.contains("stable") {
            let cargo = PathBuf::from(&path).join("bin").join("cargo");
            if cargo.exists() {
                return Some(cargo);
            }
        }
    }


    // Look in ~/.risc0/toolchains for the latest rust toolchain
    let home = env::var("HOME").ok()?;
    let toolchains_dir = PathBuf::from(&home).join(".risc0").join("toolchains");

    if !toolchains_dir.exists() {
        return None;
    }

    // Find the latest rust toolchain (sort by version)
    let mut rust_toolchains: Vec<_> = fs::read_dir(&toolchains_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("-rust-"))
        .collect();

    // Sort by name (versions are sortable as strings for semver-like versions)
    rust_toolchains.sort_by(|a, b| b.file_name().cmp(&a.file_name()));

    for entry in rust_toolchains {
        let cargo = entry.path().join("bin").join("cargo");
        if cargo.exists() {
            return Some(cargo);
        }
    }

    None
}

/// Build the transition guest and copy the ELF to OUT_DIR.
fn build_transition_guest(guest_dir: &Path, out_dir: &Path) -> Result<(), String> {
    let cargo_path = find_risc0_cargo()
        .ok_or_else(|| "RISC0 toolchain not found. Install with: rzup install".to_string())?;

    // Get the toolchain directory (parent of bin/)
    let toolchain_dir = cargo_path
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| "Invalid RISC0 toolchain path structure".to_string())?;

    let rustc_path = toolchain_dir.join("bin").join("rustc");

    println!("cargo:warning=Building transition guest with RISC0 toolchain...");

    // Build using RISC0's cargo for risc0 target
    let output = Command::new(&cargo_path)
        .current_dir(guest_dir)
        .args([
            "build",
            "--release",
            "--target",
            "riscv32im-risc0-zkvm-elf",
        ])
        .env("RUSTC", &rustc_path)
        .output()
        .map_err(|e| format!("Failed to run cargo: {}", e))?;

    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{}\n{}", stdout, stderr);
        // Emit each line so the full compiler output is visible
        eprintln!("--- transition guest build failed ---");
        for line in stdout.lines() {
            eprintln!("{}", line);
        }
        for line in stderr.lines() {
            eprintln!("{}", line);
        }
        // Detect "target unavailable for channel stable" and give clear instructions
        if combined.contains("is unavailable for download") && combined.contains("stable") {
            return Err(format!(
                "The riscv32im-risc0-zkvm-elf target is not available for the 'stable' toolchain. \
                 You must use the RISC0 toolchain. \
                 Install it with: curl -L https://risczero.com/install | bash && rzup install \
                 Then rebuild. If you set RISC0_RUST_TOOLCHAIN_PATH, ensure it points to the RISC0 \
                 toolchain (e.g. ~/.risc0/toolchains/<name>), not to stable.\n--- stdout ---\n{}\n--- stderr ---\n{}",
                stdout, stderr
            ));
        }
        return Err(format!(
            "Compilation failed. Run manually for full output: cd guests/transition && cargo build --release --target riscv32im-risc0-zkvm-elf (use the RISC0 toolchain's cargo from ~/.risc0/toolchains/...).\n--- stdout ---\n{}\n--- stderr ---\n{}",
            stdout, stderr
        ));
    }

    // Find the ELF file
    let elf_path = guest_dir
        .join("target")
        .join("riscv32im-risc0-zkvm-elf")
        .join("release")
        .join("transition-guest");

    if !elf_path.exists() {
        return Err(format!("ELF not found at: {}", elf_path.display()));
    }

    // Copy ELF to OUT_DIR
    let dest_elf = out_dir.join("transition_guest.elf");
    fs::copy(&elf_path, &dest_elf)
        .map_err(|e| format!("Failed to copy ELF: {}", e))?;

    println!("cargo:warning=Transition guest built successfully");

    Ok(())
}
