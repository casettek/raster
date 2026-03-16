//! Trace types (requires std feature).

use serde::{Deserialize, Serialize};
use std::ops::{Deref, DerefMut};
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
pub struct TileExecRecord {
    pub exec_index: u64,
    pub sequence_id: String,
    pub intra_sequence_index: u32,
    pub coordinates: CfsCoordinates,
    pub fn_call_record: FnCallRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceStartRecord {
    pub exec_index: u64,

    pub sequence_id: String,
    pub sequence_coordinates: CfsCoordinates,

    pub inputs: Vec<FnInputParam>,
    pub input_data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceEndRecord {
    pub exec_index: u64,

    pub sequence_id: String,
    pub sequence_coordinates: CfsCoordinates,

    pub output_type: Option<String>,
    pub output_data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StepRecord {
    SequenceStart(SequenceStartRecord),
    SequenceEnd(SequenceEndRecord),
    TileExec(TileExecRecord),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Trace(pub Vec<StepRecord>);

impl Trace {
    pub fn new() -> Self {
        Trace(Vec::new())
    }
}

impl Deref for Trace {
    type Target = Vec<StepRecord>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Trace {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl IntoIterator for Trace {
    type Item = StepRecord;
    type IntoIter = std::vec::IntoIter<StepRecord>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TraceEvent {
    SequenceStart(FnCallRecord),
    SequenceEnd(FnCallRecord),

    TileExec(FnCallRecord),
}
