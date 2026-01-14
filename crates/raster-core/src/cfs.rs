//! Control Flow Schema (CFS) types.
//!
//! This module defines the data structures for representing the control flow
//! and data flow of a Raster application. The CFS captures:
//! - All tiles and their input/output arities
//! - All sequences and their item composition
//! - Data flow bindings between tiles, sequences, and external inputs

use serde::{Deserialize, Serialize};
use std::string::{String, ToString};
use std::vec::Vec;

/// The root control flow schema structure for a Raster project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlFlowSchema {
    /// Schema version for forward compatibility.
    pub version: String,
    /// Project name (from Cargo.toml).
    pub project: String,
    /// Serialization encoding used (e.g., "postcard").
    pub encoding: String,
    /// All tiles defined in the project.
    pub tiles: Vec<TileDef>,
    /// All sequences defined in the project.
    pub sequences: Vec<SequenceDef>,
}

impl ControlFlowSchema {
    /// Create a new CFS with the given project name.
    pub fn new(project: impl Into<String>) -> Self {
        Self {
            version: "1.0".to_string(),
            project: project.into(),
            encoding: "postcard".to_string(),
            tiles: Vec::new(),
            sequences: Vec::new(),
        }
    }
}

/// Definition of a tile in the CFS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TileDef {
    /// Unique identifier for the tile (function name).
    pub id: String,
    /// Tile type (e.g., "iter" for iterator-style tiles).
    #[serde(rename = "type")]
    pub tile_type: String,
    /// Number of input arguments.
    pub inputs: usize,
    /// Number of output values.
    pub outputs: usize,
}

impl TileDef {
    /// Create a new tile definition with the specified type.
    pub fn new(id: impl Into<String>, tile_type: impl Into<String>, inputs: usize, outputs: usize) -> Self {
        Self {
            id: id.into(),
            tile_type: tile_type.into(),
            inputs,
            outputs,
        }
    }

    /// Create a new tile definition with the default "iter" type.
    pub fn iter(id: impl Into<String>, inputs: usize, outputs: usize) -> Self {
        Self::new(id, "iter", inputs, outputs)
    }
}

/// Definition of a sequence in the CFS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceDef {
    /// Unique identifier for the sequence (function name).
    pub id: String,
    /// Sources for the sequence's own inputs.
    pub input_sources: Vec<InputBinding>,
    /// Ordered list of items (tiles or nested sequences) in this sequence.
    pub items: Vec<SequenceItem>,
}

impl SequenceDef {
    /// Create a new sequence definition.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            input_sources: Vec::new(),
            items: Vec::new(),
        }
    }
}

/// An item within a sequence (either a tile or a nested sequence).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceItem {
    /// Type of item: "tile" or "sequence".
    pub item_type: String,
    /// ID of the tile or sequence being invoked.
    pub item_id: String,
    /// Sources for each input to this item.
    pub input_sources: Vec<InputBinding>,
}

impl SequenceItem {
    /// Create a new tile item.
    pub fn tile(id: impl Into<String>, input_sources: Vec<InputBinding>) -> Self {
        Self {
            item_type: "tile".to_string(),
            item_id: id.into(),
            input_sources,
        }
    }

    /// Create a new sequence item.
    pub fn sequence(id: impl Into<String>, input_sources: Vec<InputBinding>) -> Self {
        Self {
            item_type: "sequence".to_string(),
            item_id: id.into(),
            input_sources,
        }
    }
}

/// A binding that specifies where an input value comes from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputBinding {
    /// The source of this input.
    pub source: InputSource,
}

impl InputBinding {
    /// Create a binding from an input source.
    pub fn new(source: InputSource) -> Self {
        Self { source }
    }

    /// Create an external input binding.
    pub fn external() -> Self {
        Self::new(InputSource::External)
    }

    /// Create a sequence input binding.
    pub fn seq_input(input_index: usize) -> Self {
        Self::new(InputSource::SeqInput { input_index })
    }

    /// Create an item output binding.
    pub fn item_output(item_index: usize, output_index: usize) -> Self {
        Self::new(InputSource::ItemOutput {
            item_index,
            output_index,
        })
    }
}

/// Source of an input value in the data flow schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum InputSource {
    /// Input comes from outside the sequence (runtime-provided).
    #[serde(rename = "external")]
    External,

    /// Input comes from one of the sequence's declared inputs.
    #[serde(rename = "seq_input")]
    SeqInput {
        /// Index of the sequence input (0-based).
        input_index: usize,
    },

    /// Input comes from a previous item's output.
    #[serde(rename = "item_output")]
    ItemOutput {
        /// Index of the item in the sequence (0-based).
        item_index: usize,
        /// Index of the output from that item (0-based).
        output_index: usize,
    },
}
