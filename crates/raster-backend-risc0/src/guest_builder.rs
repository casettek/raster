//! Guest crate builder for RISC0 zkVM.
//!
//! This module generates temporary guest crates that wrap tile functions
//! for execution in the RISC0 zkVM.

use raster_core::tile::TileMetadata;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Configuration for building guest crates.
pub struct GuestBuilder {
    /// Output directory for artifacts.
    output_dir: PathBuf,
    /// Path to the user's crate that contains the tiles.
    user_crate_path: Option<PathBuf>,
}

impl GuestBuilder {
    /// Create a new guest builder with the given output directory.
    pub fn new(output_dir: PathBuf) -> Self {
        Self {
            output_dir,
            user_crate_path: None,
        }
    }

    /// Set the path to the user's crate containing tiles.
    pub fn with_user_crate(mut self, path: PathBuf) -> Self {
        self.user_crate_path = Some(path);
        self
    }

    /// Get the artifact directory for a tile.
    pub fn artifact_dir(&self, tile_id: &str) -> PathBuf {
        self.output_dir.join("tiles").join(tile_id).join("risc0")
    }

    /// Generate the guest crate source for a tile.
    ///
    /// The generated guest program:
    /// 1. Reads input bytes from the zkVM environment
    /// 2. Looks up the tile by ID in the registry
    /// 3. Executes the tile's entry function
    /// 4. Commits the output bytes to the journal
    pub fn generate_guest_main(&self, tile_id: &str) -> String {
        format!(
            r##"//! Auto-generated RISC0 guest program for tile: {tile_id}
#![no_main]
#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use risc0_zkvm::guest::env;
use raster::core::registry::find_tile_by_str;

risc0_zkvm::guest::entry!(main);

fn main() {{
    // Read input bytes from the host
    let input: Vec<u8> = env::read();

    // Look up and execute the tile
    let tile = find_tile_by_str("{tile_id}")
        .expect("Tile not found in registry");

    let output = tile.execute(&input)
        .expect("Tile execution failed");

    // Commit output to the journal
    env::commit_slice(&output);
}}
"##,
            tile_id = tile_id
        )
    }

    /// Generate Cargo.toml for a guest crate.
    pub fn generate_guest_cargo_toml(&self, tile_id: &str) -> String {
        let user_crate_dep = if let Some(ref path) = self.user_crate_path {
            format!(
                r#"user-crate = {{ path = "{}" }}"#,
                path.display()
            )
        } else {
            String::new()
        };

        format!(
            r##"[package]
name = "raster-guest-{tile_id}"
version = "0.1.0"
edition = "2021"

[dependencies]
risc0-zkvm = {{ version = "1.2", default-features = false, features = ["guest"] }}
raster = {{ path = "../../../../crates/raster" }}
{user_crate_dep}

[profile.release]
opt-level = 3
lto = true

[workspace]
"##,
            tile_id = tile_id,
            user_crate_dep = user_crate_dep
        )
    }

    /// Build a guest crate and return the path to the ELF.
    pub fn build_guest(
        &self,
        tile_id: &str,
        guest_dir: &Path,
    ) -> Result<PathBuf, GuestBuildError> {
        // Create guest source directory
        let src_dir = guest_dir.join("src");
        fs::create_dir_all(&src_dir)?;

        // Write main.rs
        let main_rs = self.generate_guest_main(tile_id);
        fs::write(src_dir.join("main.rs"), main_rs)?;

        // Write Cargo.toml
        let cargo_toml = self.generate_guest_cargo_toml(tile_id);
        fs::write(guest_dir.join("Cargo.toml"), cargo_toml)?;

        // Build using cargo for risc0 target
        let output = Command::new("cargo")
            .current_dir(guest_dir)
            .args([
                "build",
                "--release",
                "--target",
                "riscv32im-risc0-zkvm-elf",
            ])
            .env("RUSTFLAGS", "-C passes=loweratomic")
            .output()?;

        if !output.status.success() {
            return Err(GuestBuildError::CompilationFailed(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        // Find the ELF file
        let elf_path = guest_dir
            .join("target")
            .join("riscv32im-risc0-zkvm-elf")
            .join("release")
            .join(format!("raster-guest-{}", tile_id));

        if !elf_path.exists() {
            return Err(GuestBuildError::ElfNotFound(elf_path));
        }

        Ok(elf_path)
    }

    /// Copy artifacts to the output directory.
    pub fn write_artifacts(
        &self,
        tile_id: &str,
        elf_bytes: &[u8],
        method_id: &[u8],
    ) -> Result<PathBuf, GuestBuildError> {
        let artifact_dir = self.artifact_dir(tile_id);
        fs::create_dir_all(&artifact_dir)?;

        // Write ELF
        fs::write(artifact_dir.join("guest.elf"), elf_bytes)?;

        // Write method ID as hex
        let method_id_hex = hex::encode(method_id);
        fs::write(artifact_dir.join("method_id"), &method_id_hex)?;

        // Write manifest
        let manifest = serde_json::json!({
            "tile_id": tile_id,
            "method_id": method_id_hex,
            "elf_size": elf_bytes.len(),
        });
        fs::write(
            artifact_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest)?,
        )?;

        Ok(artifact_dir)
    }
}

/// Errors that can occur during guest building.
#[derive(Debug, thiserror::Error)]
pub enum GuestBuildError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Compilation failed: {0}")]
    CompilationFailed(String),

    #[error("ELF not found at: {0}")]
    ElfNotFound(PathBuf),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
}
