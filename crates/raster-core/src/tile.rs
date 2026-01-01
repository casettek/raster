use serde::{Deserialize, Serialize};

/// Unique identifier for a tile.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TileId(pub String);

impl TileId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl From<&str> for TileId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<&TileIdStatic> for TileId {
    fn from(s: &TileIdStatic) -> Self {
        Self(s.0.to_string())
    }
}

/// Static tile identifier for use in const contexts (e.g., distributed slices).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileIdStatic(pub &'static str);

impl TileIdStatic {
    pub const fn new(id: &'static str) -> Self {
        Self(id)
    }

    pub fn to_owned(&self) -> TileId {
        TileId(self.0.to_string())
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

impl TileMetadata {
    /// Convert from static metadata to owned metadata.
    pub fn from_static(s: &TileMetadataStatic) -> Self {
        Self {
            id: s.id.to_owned(),
            name: s.name.to_string(),
            description: s.description.map(|d| d.to_string()),
            estimated_cycles: s.estimated_cycles,
            max_memory: s.max_memory,
        }
    }
}

/// Static tile metadata for use in const contexts (e.g., distributed slices).
///
/// This uses `&'static str` instead of `String` to allow const construction.
#[derive(Debug, Clone, Copy)]
pub struct TileMetadataStatic {
    pub id: TileIdStatic,
    pub name: &'static str,
    pub description: Option<&'static str>,
    pub estimated_cycles: Option<u64>,
    pub max_memory: Option<u64>,
}

impl TileMetadataStatic {
    pub const fn new(
        id: &'static str,
        name: &'static str,
        description: Option<&'static str>,
        estimated_cycles: Option<u64>,
        max_memory: Option<u64>,
    ) -> Self {
        Self {
            id: TileIdStatic::new(id),
            name,
            description,
            estimated_cycles,
            max_memory,
        }
    }

    pub fn to_owned(&self) -> TileMetadata {
        TileMetadata::from_static(self)
    }
}
