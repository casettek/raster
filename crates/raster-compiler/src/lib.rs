//! Build orchestration for the Raster toolchain.
//!
//! This crate handles:
//! - Compiling tiles into standalone binaries
//! - Generating sequence schemas
//! - Managing build artifacts
//! - Source-based tile discovery
//! - Control flow schema (CFS) generation

pub mod builder;
pub mod cfs_builder;
pub mod discovery;
pub mod flow_resolver;
pub mod schema_gen;

pub use builder::{BuildOutput, Builder, TileArtifact, TileManifest};
pub use cfs_builder::{extract_project_name, CfsBuilder};
pub use discovery::{
    DiscoveredSequence, DiscoveredTile, SequenceCall, SequenceDiscovery, TileDiscovery,
};
pub use flow_resolver::FlowResolver;
pub use schema_gen::SchemaGenerator;
