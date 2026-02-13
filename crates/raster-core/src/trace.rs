//! Trace types (requires std feature).

use crate::tile::TileId;
use serde::{Deserialize, Serialize};
use std::string::String;
use std::vec::Vec;

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
    /// Postcard-serialized input data
    pub input_data: Vec<u8>,
    /// The return type as a string (e.g., "String", "Result<u64, Error>")
    pub output_type: Option<String>,
    /// Postcard-serialized output data
    pub output_data: Vec<u8>,
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

/// Information about where execution diverged during audit verification.
///
/// Contains both the index where divergence was detected and the merkle tree
/// frontier state that can be used to replay execution from the window start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditDiff {
    /// The trace index where divergence was detected
    pub index: usize,
    /// Serialized merkle tree frontier before the first trace window item.
    /// This can be used to replay execution from the window start.
    pub frontier: Vec<u8>,
    /// The expected fingerprint (packed u64s as bytes, little-endian).
    /// Used to verify the computed tree root after each transition.
    pub fingerprint: Vec<u8>,
    /// Number of bits per fingerprint item.
    pub bits_per_item: usize,
    /// Starting position in the fingerprint for the trace window.
    /// This is the position corresponding to the first item in trace_window.
    pub window_start_position: usize,
}

/// Result of an audit verification run.
///
/// This is emitted via `RASTER_AUDIT:` prefix when audit mode completes,
/// providing structured information about the verification outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditResult {
    /// Whether the audit verification passed (no mismatches detected)
    pub success: bool,
    /// On mismatch: diff information including index and frontier
    pub diff: Option<AuditDiff>,
    /// Window of trace items leading up to and including the diff point.
    /// Contains the last N items (configurable) for debugging context.
    /// Empty if verification succeeded.
    pub trace_window: Vec<TraceItem>,
}
