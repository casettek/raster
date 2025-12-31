//! Build orchestration for the Raster toolchain.
//!
//! This crate handles:
//! - Compiling tiles into standalone binaries
//! - Generating sequence schemas
//! - Managing build artifacts

pub mod builder;
pub mod schema_gen;

pub use builder::Builder;
pub use schema_gen::SchemaGenerator;
