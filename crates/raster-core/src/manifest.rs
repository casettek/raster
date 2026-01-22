//! Manifest types (requires std feature).

use crate::schema::SequenceSchema;
use crate::tile::TileMetadata;
use serde::{Deserialize, Serialize};
use std::string::String;
use std::vec::Vec;

/// Project-level manifest describing all tiles and sequences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub tiles: Vec<TileMetadata>,
    pub sequences: Vec<SequenceSchema>,
}

impl Manifest {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            tiles: Vec::new(),
            sequences: Vec::new(),
        }
    }
}
