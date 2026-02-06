//! Core types and schemas for the Raster toolchain.
//!
//! This crate defines the foundational data structures used across the entire
//! Raster system. It contains no logic—only type definitions, serialization
//! formats, and error types.
//!
//! This crate is `no_std` compatible when the `std` feature is disabled.

#![no_std]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

pub mod error;
pub mod tile;

// These modules are only available with std (they use serde_json for complex serialization)
#[cfg(feature = "std")]
pub mod cfs;
#[cfg(feature = "std")]
pub mod ipc;
#[cfg(feature = "std")]
pub mod manifest;
#[cfg(feature = "std")]
pub mod schema;
#[cfg(feature = "std")]
pub mod trace;

// The registry module uses linkme which doesn't support RISC-V targets
#[cfg(all(feature = "std", not(target_arch = "riscv32")))]
pub mod registry;

pub use error::{Error, Result};

// Re-export linkme for use by the macro-generated code (not available on RISC-V)
#[cfg(all(feature = "std", not(target_arch = "riscv32")))]
pub use linkme;

// Re-export postcard for tile ABI serialization (no_std compatible)
pub use postcard;

// Re-export bincode for std-only code that needs it
#[cfg(feature = "std")]
pub use bincode;
