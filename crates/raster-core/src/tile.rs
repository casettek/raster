use serde::{Deserialize, Serialize};

/// Unique identifier for a tile.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TileId(pub String);

impl TileId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

/// Metadata about a tile's resource requirements and characteristics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TileMetadata {
    pub id: TileId,
    pub name: String,
    pub description: Option<String>,

    /// Expected cycle count (for cost estimation)
    pub estimated_cycles: Option<u64>,

    /// Maximum memory usage in bytes
    pub max_memory: Option<u64>,
}
