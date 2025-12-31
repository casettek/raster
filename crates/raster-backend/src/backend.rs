use raster_core::{Result, tile::TileMetadata};

/// Trait defining the interface for compilation and execution backends.
pub trait Backend {
    /// Compile a tile into a standalone binary.
    fn compile_tile(&self, metadata: &TileMetadata, source_path: &str) -> Result<Vec<u8>>;

    /// Execute a tile and return its result.
    fn execute_tile(&self, tile_binary: &[u8], input: &[u8]) -> Result<Vec<u8>>;

    /// Estimate resource usage for a tile.
    fn estimate_resources(&self, metadata: &TileMetadata) -> Result<ResourceEstimate>;
}

#[derive(Debug, Clone)]
pub struct ResourceEstimate {
    pub cycles: Option<u64>,
    pub memory_bytes: Option<u64>,
}
