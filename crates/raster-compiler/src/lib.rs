//! Build orchestration for the Raster toolchain.
//!
//! This crate handles:
//! - Compiling tiles into standalone binaries
//! - Generating sequence schemas
//! - Managing build artifacts
//! - Source-based tile discovery
//! - Control flow schema (CFS) generation

pub mod ast;
pub mod backend;
pub mod builder;
pub mod cfs_builder;
pub mod flow_resolver;
pub mod project;
pub mod sequence;
pub mod tile;

pub use ast::ProjectAst;
pub use builder::{BuildOutput, Builder, SequenceRunner, TileArtifact, TileManifest, TileRunner};
pub use cfs_builder::CfsBuilder;
pub use flow_resolver::FlowResolver;
pub use project::Project;
