use raster_core::{Result, tile::TileMetadata};
use crate::backend::{Backend, ResourceEstimate};

/// Native backend that compiles and executes tiles as native code.
pub struct NativeBackend {
    // Configuration options will go here
}

impl NativeBackend {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for NativeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for NativeBackend {
    fn compile_tile(&self, _metadata: &TileMetadata, _source_path: &str) -> Result<Vec<u8>> {
        // TODO: Implement native compilation
        // - Invoke rustc
        // - Generate standalone binary
        Ok(Vec::new())
    }

    fn execute_tile(&self, _tile_binary: &[u8], _input: &[u8]) -> Result<Vec<u8>> {
        // TODO: Implement native execution
        // - Load binary
        // - Execute in-process
        // - Return result
        Ok(Vec::new())
    }

    fn estimate_resources(&self, metadata: &TileMetadata) -> Result<ResourceEstimate> {
        Ok(ResourceEstimate {
            cycles: metadata.estimated_cycles,
            memory_bytes: metadata.max_memory,
        })
    }
}
