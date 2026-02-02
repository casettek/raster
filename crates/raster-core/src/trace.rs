//! Trace types (requires std feature).

use std::string::String;
use std::vec::Vec;
use serde::{Deserialize, Serialize};
use crate::tile::TileId;

/// Describes an input parameter for a tile function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceInputParam {
    /// Parameter name from the function signature
    pub name: String,
    /// Type name as a string (e.g., "u64", "String")
    pub ty: String,
}

/// A structured trace item emitted during tile execution.
///
/// This captures the tile's function signature metadata along with
/// the serialized input/output data for complete traceability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceItem {
    /// The tile function name/identifier
    pub fn_name: String,
    /// Optional human-readable description
    pub desc: Option<String>,
    /// Input parameter metadata (name and type for each parameter)
    pub inputs: Vec<TraceInputParam>,
    /// Base64-encoded postcard-serialized input data
    pub input_data: String,
    /// The return type as a string (e.g., "String", "Result<u64, Error>")
    pub output_type: Option<String>,
    /// Base64-encoded postcard-serialized output data
    pub output_data: String,
}

/// A complete execution trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub run_id: String,
    pub timestamp: u64,
    pub events: Vec<TraceEvent>,
}

/// A single event in an execution trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TraceEvent {
    TileStart {
        tile_id: TileId,
        timestamp: u64,
        depth: u32,
    },
    TileEnd {
        tile_id: TileId,
        timestamp: u64,
        duration_ns: u64,
        cycles: Option<u64>,
    },
    SequenceStart {
        name: String,
        timestamp: u64,
    },
    SequenceEnd {
        name: String,
        timestamp: u64,
        duration_ns: u64,
    },
}
