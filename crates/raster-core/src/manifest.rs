//! Manifest types (requires std feature).

use crate::schema::SequenceSchema;
use crate::tile::TileMetadata;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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

/// A file-backed external input declared inside the user input document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalInputEntry {
    pub path: String,
    #[serde(default)]
    pub data_hash: Option<String>,
}

/// A JSON input document used by the native whole-program runner.
///
/// Each top-level field may be either a plain JSON value or a file-backed external
/// reference described by `ExternalInputEntry`.
pub type InputDocument = BTreeMap<String, serde_json::Value>;
