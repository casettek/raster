//! RISC0 zkVM backend for the Raster toolchain.
//!
//! This crate provides the `Risc0Backend` which compiles tiles into RISC0 guest
//! programs and executes them in the zkVM with optional proof generation.

mod guest_builder;
mod risc0;

pub use risc0::Risc0Backend;
