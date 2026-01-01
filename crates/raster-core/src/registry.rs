//! Tile registry for discovering and executing tiles at runtime.
//!
//! This module provides a global registry of tiles that are automatically
//! populated by the `#[tile]` macro using linkme's distributed slices.
//! The registry enables both host-side tile discovery and guest-side
//! tile execution in the RISC0 zkVM.

use crate::tile::{TileId, TileIdStatic, TileMetadata, TileMetadataStatic};
use crate::Result;
use linkme::distributed_slice;

/// The function signature for a tile's ABI entry point.
///
/// This wrapper function handles bincode deserialization of inputs,
/// calls the actual tile function, and serializes the output.
pub type TileEntryFn = fn(&[u8]) -> Result<Vec<u8>>;

/// A registration entry for a tile in the global registry.
///
/// Each `#[tile]`-annotated function generates a `TileRegistration`
/// that is automatically added to the `TILE_REGISTRY` distributed slice.
/// Uses static string references to enable const construction.
#[derive(Clone, Copy)]
pub struct TileRegistration {
    /// Static metadata describing the tile (id, name, description, resource hints).
    pub metadata: TileMetadataStatic,
    /// The ABI wrapper function that handles serialization/deserialization.
    pub entry: TileEntryFn,
}

impl TileRegistration {
    /// Create a new tile registration with const construction.
    pub const fn new(metadata: TileMetadataStatic, entry: TileEntryFn) -> Self {
        Self { metadata, entry }
    }

    /// Execute the tile with the given input bytes.
    pub fn execute(&self, input: &[u8]) -> Result<Vec<u8>> {
        (self.entry)(input)
    }

    /// Get the tile ID as a static reference.
    pub const fn id(&self) -> &TileIdStatic {
        &self.metadata.id
    }

    /// Get the tile ID as a string slice.
    pub const fn id_str(&self) -> &'static str {
        self.metadata.id.0
    }

    /// Get owned metadata (converts static strings to owned Strings).
    pub fn metadata_owned(&self) -> TileMetadata {
        self.metadata.to_owned()
    }
}

impl std::fmt::Debug for TileRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TileRegistration")
            .field("metadata", &self.metadata)
            .field("entry", &"<fn>")
            .finish()
    }
}

/// The global distributed slice containing all tile registrations.
///
/// This slice is populated at link time by the `#[tile]` macro.
/// It works in both host builds and RISC0 guest builds.
#[distributed_slice]
pub static TILE_REGISTRY: [TileRegistration];

/// Iterate over all registered tiles.
pub fn iter_tiles() -> impl Iterator<Item = &'static TileRegistration> {
    TILE_REGISTRY.iter()
}

/// Find a tile by its ID.
pub fn find_tile(id: &TileId) -> Option<&'static TileRegistration> {
    TILE_REGISTRY
        .iter()
        .find(|reg| reg.metadata.id.0 == id.0)
}

/// Find a tile by its static ID.
pub fn find_tile_static(id: &TileIdStatic) -> Option<&'static TileRegistration> {
    TILE_REGISTRY.iter().find(|reg| &reg.metadata.id == id)
}

/// Find a tile by its string ID.
pub fn find_tile_by_str(id: &str) -> Option<&'static TileRegistration> {
    TILE_REGISTRY.iter().find(|reg| reg.metadata.id.0 == id)
}

/// Get the count of registered tiles.
pub fn tile_count() -> usize {
    TILE_REGISTRY.len()
}

/// Get all tile IDs as a vector of owned TileId.
pub fn all_tile_ids() -> Vec<TileId> {
    TILE_REGISTRY
        .iter()
        .map(|reg| reg.metadata.id.to_owned())
        .collect()
}

/// Get all tile metadata as a vector of owned TileMetadata.
pub fn all_tile_metadata() -> Vec<TileMetadata> {
    TILE_REGISTRY
        .iter()
        .map(|reg| reg.metadata.to_owned())
        .collect()
}
