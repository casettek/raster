//! Guest crate builder for RISC0 zkVM.
//!
//! This module generates temporary guest crates that wrap tile functions
//! for execution in the RISC0 zkVM.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Configuration for building guest crates.
pub struct GuestBuilder {
    /// Output directory for artifacts.
    output_dir: PathBuf,
    /// Path to the user's crate that contains the tiles.
    user_crate_path: Option<PathBuf>,
    /// Name of the user's crate package.
    user_crate_name: Option<String>,
    /// Path to the raster workspace root.
    raster_workspace: Option<PathBuf>,
}

impl GuestBuilder {
    /// Create a new guest builder with the given output directory.
    pub fn new(output_dir: PathBuf) -> Self {
        Self {
            output_dir,
            user_crate_path: None,
            user_crate_name: None,
            raster_workspace: None,
        }
    }

    /// Set the path to the user's crate containing tiles.
    pub fn with_user_crate(mut self, path: PathBuf) -> Self {
        self.user_crate_path = Some(path.clone());
        // Try to find the raster workspace by looking at the user crate's Cargo.toml
        if let Ok(cargo_toml) = fs::read_to_string(path.join("Cargo.toml")) {
            // Extract the package name
            self.user_crate_name = Self::extract_package_name(&cargo_toml);
            
            // Look for raster dependency path
            if let Some(raster_path) = Self::extract_raster_path(&cargo_toml) {
                let absolute_raster = path.join(&raster_path).canonicalize().ok();
                if let Some(raster_crate_path) = absolute_raster {
                    // The raster workspace is two levels up from crates/raster
                    self.raster_workspace = raster_crate_path.parent()
                        .and_then(|p| p.parent())
                        .map(|p| p.to_path_buf());
                }
            }
        }
        self
    }

    /// Extract the package name from Cargo.toml content.
    fn extract_package_name(cargo_toml: &str) -> Option<String> {
        // Simple parsing: look for name = "..."
        let mut in_package_section = false;
        for line in cargo_toml.lines() {
            let trimmed = line.trim();
            if trimmed == "[package]" {
                in_package_section = true;
                continue;
            }
            if trimmed.starts_with('[') {
                in_package_section = false;
                continue;
            }
            if in_package_section && trimmed.starts_with("name") {
                if let Some(start) = line.find("\"") {
                    let rest = &line[start + 1..];
                    if let Some(end) = rest.find('"') {
                        return Some(rest[..end].to_string());
                    }
                }
            }
        }
        None
    }

    /// Extract the raster dependency path from Cargo.toml content.
    fn extract_raster_path(cargo_toml: &str) -> Option<String> {
        // Simple parsing: look for raster = { path = "..." }
        for line in cargo_toml.lines() {
            if line.contains("raster") && line.contains("path") {
                // Extract the path value
                if let Some(start) = line.find("path = \"") {
                    let rest = &line[start + 8..];
                    if let Some(end) = rest.find('"') {
                        return Some(rest[..end].to_string());
                    }
                }
            }
        }
        None
    }

    /// Get the artifact directory for a tile.
    pub fn artifact_dir(&self, tile_id: &str) -> PathBuf {
        self.output_dir.join("tiles").join(tile_id).join("risc0")
    }

    /// Generate the guest crate source for a tile.
    ///
    /// The generated guest program:
    /// 1. Reads input bytes from the zkVM environment
    /// 2. Calls the tile's ABI wrapper function directly
    /// 3. Commits the output to the journal
    pub fn generate_guest_main(&self, tile_id: &str) -> String {
        // Convert tile_id to a valid Rust identifier (replace hyphens with underscores)
        let fn_name = tile_id.replace('-', "_");
        
        // The wrapper function name follows the pattern from the #[tile] macro
        let wrapper_name = format!("__raster_tile_entry_{}", fn_name);
        
        // Get the crate name for the import
        let crate_name = self.user_crate_name.as_ref()
            .map(|n| n.replace('-', "_"))
            .unwrap_or_else(|| "user_crate".to_string());

        format!(
            r##"//! Auto-generated RISC0 guest program for tile: {tile_id}
            #![no_main]
            #![no_std]

            extern crate alloc;

            use alloc::vec::Vec;

            // Import the tile's ABI wrapper from the user crate
            // The #[tile] macro generates a public wrapper function for each tile
            use {crate_name}::{wrapper_name};

            risc0_zkvm::guest::entry!(main);

            fn main() {{
                // Read input length first, then raw bytes
                let input_len: u32 = risc0_zkvm::guest::env::read();
                let mut input = alloc::vec![0u8; input_len as usize];
                risc0_zkvm::guest::env::read_slice(&mut input);

                // Call the tile's ABI wrapper (handles deserialization/serialization)
                let output = {wrapper_name}(&input).expect("Tile execution failed");

                // Commit output to the journal
                risc0_zkvm::guest::env::commit_slice(&output);
            }}
            "##,
            tile_id = tile_id,
            crate_name = crate_name,
            wrapper_name = wrapper_name
        )
    }

    /// Generate Cargo.toml for a guest crate.
    pub fn generate_guest_cargo_toml(&self, tile_id: &str) -> String {
        // User crate dependency with default-features = false for no_std
        let user_crate_dep = match (&self.user_crate_path, &self.user_crate_name) {
            (Some(path), Some(name)) => {
                format!(
                    r#"{} = {{ path = "{}", default-features = false }}"#,
                    name,
                    path.display()
                )
            }
            (Some(path), None) => {
                format!(
                    r#"user-crate = {{ path = "{}", default-features = false }}"#,
                    path.display()
                )
            }
            _ => String::new(),
        };

        // Use absolute path to the raster crate
        let raster_path = if let Some(ref workspace) = self.raster_workspace {
            workspace.join("crates").join("raster").display().to_string()
        } else {
            // Fallback: assume we're in the raster workspace
            "../../../../crates/raster".to_string()
        };

        format!(
            r##"[package]
            name = "raster-guest-{tile_id}"
            version = "0.1.0"
            edition = "2021"

            [dependencies]
            risc0-zkvm = {{ version = "1.2", default-features = false }}
            raster = {{ path = "{raster_path}", default-features = false }}
            {user_crate_dep}

            [profile.release]
            opt-level = 3
            lto = true

            [workspace]
            "##,
            tile_id = tile_id,
            user_crate_dep = user_crate_dep,
            raster_path = raster_path
        )
    }

    /// Find the RISC0 toolchain's cargo binary.
    fn find_risc0_cargo() -> Option<PathBuf> {
        // Check for RISC0_RUST_TOOLCHAIN_PATH env var first
        if let Ok(path) = env::var("RISC0_RUST_TOOLCHAIN_PATH") {
            let cargo = PathBuf::from(&path).join("bin").join("cargo");
            if cargo.exists() {
                return Some(cargo);
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

        // Find RISC0's cargo with the riscv32im-risc0-zkvm-elf target
        let cargo_path = Self::find_risc0_cargo()
            .ok_or_else(|| GuestBuildError::CompilationFailed(
                "RISC0 toolchain not found. Please install it with: rzup install".to_string()
            ))?;

        // Get the toolchain directory (parent of bin/)
        let toolchain_dir = cargo_path.parent()
            .and_then(|p| p.parent())
            .ok_or_else(|| GuestBuildError::CompilationFailed(
                "Invalid RISC0 toolchain path structure".to_string()
            ))?;

        // Build using RISC0's cargo for risc0 target
        // We need to ensure cargo uses the RISC0 toolchain's rustc by setting RUSTC
        let rustc_path = toolchain_dir.join("bin").join("rustc");
        let output = Command::new(&cargo_path)
            .current_dir(guest_dir)
            .args([
                "build",
                "--release",
                "--target",
                "riscv32im-risc0-zkvm-elf",
            ])
            .env("RUSTC", &rustc_path)
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
