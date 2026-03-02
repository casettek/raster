//! Trace types (requires std feature).

use serde::{Deserialize, Serialize};
use std::string::String;
use std::vec::Vec;

use crate::cfs::CfsCoordinates;
use crate::fingerprint::Fingerprint;

/// Describes an input parameter for a tile function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FnInputParam {
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
pub struct FnCallRecord {
    /// The tile function name/identifier
    pub fn_name: String,
    /// Optional human-readable description
    pub desc: Option<String>,
    /// Input parameter metadata (name and type for each parameter)
    pub inputs: Vec<FnInputParam>,
    /// Postcard-serialized input data
    pub input_data: Vec<u8>,
    /// The return type as a string (e.g., "String", "Result<u64, Error>")
    pub output_type: Option<String>,
    /// Postcard-serialized output data
    pub output_data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub exec_index: u64,
    pub sequence_id: String,
    pub intra_sequence_index: u64,
    pub sequence_callstack_depth: u64,
    pub sequence_coordinates: CfsCoordinates,
    pub fn_call_record: FnCallRecord,
}

// TODO: after extracting logic from user process, this should be moved out of core
//
/// Information about where execution diverged during audit verification.
///
/// Contains both the index where divergence was detected and the merkle tree
/// frontier state that can be used to replay execution from the window start.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceWindow {
    pub frontier: Vec<u8>,
    pub fingerprint: Fingerprint,
    pub items: Vec<StepRecord>,
}

pub enum TraceEvent {
    SequenceStart(FnCallRecord),
    SequenceEnd(FnCallRecord),

    Tile(FnCallRecord),
}
