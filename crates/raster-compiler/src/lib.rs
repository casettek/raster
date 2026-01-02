//! Build orchestration for the Raster toolchain.
//!
//! This crate handles:
//! - Compiling tiles into standalone binaries
//! - Generating sequence schemas
//! - Managing build artifacts
//! - Source-based tile discovery

pub mod builder;
pub mod discovery;
pub mod schema_gen;

pub use builder::{Builder, BuildOutput, TileArtifact, TileManifest};
pub use discovery::{DiscoveredSequence, DiscoveredTile, SequenceDiscovery, TileDiscovery};
pub use schema_gen::SchemaGenerator;
