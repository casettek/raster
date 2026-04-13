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

/// A private file-backed external input declared inside `input.json`.
pub type ExternalInputPathEntry = String;

/// A public external input commitment declared inside `input_manifest.json`.
pub type ExternalInputManifestEntry = String;

/// A private JSON input document used by the native whole-program runner.
///
/// Each top-level field may be either a plain JSON value or a string path
/// described by `ExternalInputPathEntry`.
pub type InputDocument = BTreeMap<String, serde_json::Value>;

/// A public JSON manifest document that describes the commitments for externals.
pub type InputManifestDocument = BTreeMap<String, serde_json::Value>;
