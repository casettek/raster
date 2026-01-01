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

pub use risc0::{is_cuda_available, is_gpu_available, is_metal_available, Risc0Backend};
