//! Core types and schemas for the Raster toolchain.
//!
//! This crate defines the foundational data structures used across the entire
//! Raster system. It contains no logic—only type definitions, serialization
//! formats, and internal runtime/protocol error types.
//!
//! This crate is `no_std` compatible when the `std` feature is disabled.

#![no_std]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

pub mod cfs;
pub mod draft;
pub mod error;
pub mod fingerprint;
pub mod input;
pub mod tile;
pub mod trace;

// These modules are only available with std (they use serde_json for complex serialization)
#[cfg(feature = "std")]
pub mod authorization;
#[cfg(feature = "std")]
pub mod coordinate_index;
#[cfg(feature = "std")]
pub mod manifest;
#[cfg(feature = "std")]
pub mod schema;
#[cfg(feature = "std")]
pub mod transition;

pub use error::{Error, Result};

// Re-export postcard for tile ABI serialization (no_std compatible)
pub use postcard;

// Re-export bincode for std-only code that needs it
#[cfg(feature = "std")]
pub use bincode;
