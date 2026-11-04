//! RISC0 zkVM backend for the Raster toolchain.
//!
//! This crate provides the `Risc0Backend` which compiles tiles into RISC0 guest
//! programs and executes them in the zkVM with optional proof generation.
//!
//! # GPU Acceleration
//!
//! GPU acceleration can be enabled for proving via feature flags:
//! - `metal` - Apple Metal (macOS with Apple Silicon)
//! - `cuda` - NVIDIA CUDA (Linux/Windows with NVIDIA GPU)
//!
//! Use `is_gpu_available()` to check at runtime if GPU support is compiled in.

mod guest_builder;
mod risc0;

pub use risc0::{Risc0ArtifactStore, Risc0Backend, Risc0CompilationArtifact};

/// The toolchain-free digest of everything a tile guest of the project at
/// `user_crate` compiles besides each tile's own source: what `Raster.lock`
/// records as `guest_inputs`. Resolves the project with `cargo metadata` and
/// builds nothing.
pub fn guest_inputs(output_dir: &std::path::Path, user_crate: &std::path::Path) -> Result<String, String> {
    guest_builder::GuestBuilder::resolve(output_dir, user_crate).map(|b| b.inputs().to_string())
}
