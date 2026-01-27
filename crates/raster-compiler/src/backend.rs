use raster_backend_native::NativeBackend;
use raster_backend_risc0::Risc0Backend;

use raster_backend::{Backend, CompilationArtifact, ExecutionMode, TileExecutionResult, ArtifactStore,  ResourceEstimate};

use raster_core::{Result, tile::TileMetadata};

/// Enum of all available backends.
pub enum BackendImpl {
    Native(NativeBackend),
    Risc0(Risc0Backend),
}

impl Backend for BackendImpl {
    fn name(&self) -> &'static str {
        match self {
            BackendImpl::Native(b) => b.name(),
            BackendImpl::Risc0(b) => b.name(),
        }
    }

    fn compile_tile(&self, tile: &TileMetadata, content_hash: Option<&str>) -> Result<Box<dyn CompilationArtifact>> {
        match self {
            BackendImpl::Native(b) => b.compile_tile(tile, content_hash),
            BackendImpl::Risc0(b) => b.compile_tile(tile, content_hash),
        }
    }

    fn execute_tile(
        &self,
        artifact: &dyn CompilationArtifact,
        input: &[u8],
        mode: ExecutionMode,
    ) -> Result<TileExecutionResult> {
        match self {
            BackendImpl::Native(b) => b.execute_tile(artifact, input, mode),
            BackendImpl::Risc0(b) => b.execute_tile(artifact, input, mode),
        }
    }

    fn artifact_store(&self) -> &dyn ArtifactStore {
        match self {
            BackendImpl::Native(b) => b.artifact_store(),
            BackendImpl::Risc0(b) => b.artifact_store(),
        }
    }

    fn estimate_resources(&self, metadata: &TileMetadata) -> Result<ResourceEstimate> {
        match self {
            BackendImpl::Native(b) => b.estimate_resources(metadata),
            BackendImpl::Risc0(b) => b.estimate_resources(metadata),
        }
    }

    fn verify_receipt(&self, artifact: &dyn CompilationArtifact, receipt: &[u8]) -> Result<bool> {
        match self {
            BackendImpl::Native(b) => b.verify_receipt(artifact, receipt),
            BackendImpl::Risc0(b) => b.verify_receipt(artifact, receipt),
        }
    }
}