//! Core types and schemas for the Raster toolchain.
//!
//! This crate defines the foundational data structures used across the entire
//! Raster system. It contains no logic—only type definitions, serialization
//! formats, and error types.

pub mod error;
pub mod manifest;
pub mod schema;
pub mod tile;
pub mod trace;

pub use error::{Error, Result};
