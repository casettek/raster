//! Backend abstraction layer for the Raster toolchain.
//!
//! This crate defines the `Backend` trait that all compilation and execution
//! backends must implement. It also provides the native backend implementation.

pub mod backend;
pub mod native;

pub use backend::{
    calculate_proof_cycles, ArtifactStore, Backend, CompilationArtifact, ExecutionMode, ResourceEstimate,
    TileExecDescriptor, TileExecutionResult, MIN_PROOF_SEGMENT_CYCLES,
};
pub use native::{NativeArtifactStore, NativeBackend, NativeCompilationArtifact};
