//! Trace types (requires std feature).

use serde::{Deserialize, Serialize};
use std::hash::Hash;
use std::ops::{Deref, DerefMut};
use std::string::String;
use std::vec::Vec;

use crate::cfs::CfsCoordinates;
use crate::fingerprint::Fingerprint;

/// Describes an input parameter for a tile function.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FnInputArgs {
    /// Parameter name from the function signature
    pub name: String,
    /// Type name as a string (e.g., "u64", "String")
    pub ty: String,
    #[serde(default)]
    pub external_name: Option<String>,
    #[serde(default)]
    pub external_data_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FnInput {
    pub data: Vec<u8>,
    pub args: Vec<FnInputArgs>,
}

impl FnInput {
    pub fn new(data: Vec<u8>, args: Vec<FnInputArgs>) -> Self {
        Self { data, args }
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn args(&self) -> &[FnInputArgs] {
        &self.args
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FnOutput {
    pub data: Vec<u8>,
    pub ty: String,
}

impl FnOutput {
    pub fn new(data: Vec<u8>, ty: impl Into<String>) -> Self {
        Self {
            data,
            ty: ty.into(),
        }
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn ty(&self) -> &str {
        &self.ty
    }
}

/// A structured trace item emitted during tile execution.
///
/// This captures the tile's function signature metadata along with
/// the serialized input/output data for complete traceability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FnCallRecord {
    pub fn_name: String,
    pub input: Option<FnInput>,
    pub output: Option<FnOutput>,
}

impl FnCallRecord {
    pub fn input_data(&self) -> Option<&[u8]> {
        self.input.as_ref().map(FnInput::data)
    }

    pub fn output_data(&self) -> Option<&[u8]> {
        self.output.as_ref().map(FnOutput::data)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TileExecRecord {
    pub exec_index: u64,

    pub tile_id: String,

    pub sequence_id: String,
    pub coordinates: CfsCoordinates,

    pub intra_sequence_index: u32,

    pub input_commitment: Vec<u8>,
    pub output_commitment: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SequenceStartRecord {
    pub exec_index: u64,

    pub sequence_id: String,
    pub coordinates: CfsCoordinates,

    pub input_commitment: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SequenceEndRecord {
    pub exec_index: u64,

    pub sequence_id: String,
    pub coordinates: CfsCoordinates,

    pub output_commitment: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum StepRecord {
    SequenceStart(SequenceStartRecord),
    SequenceEnd(SequenceEndRecord),
    TileExec(TileExecRecord),
}

impl StepRecord {
    pub fn coordinates(&self) -> &CfsCoordinates {
        match self {
            StepRecord::TileExec(tile_exec_record) => &tile_exec_record.coordinates,
            StepRecord::SequenceStart(sequence_start_record) => &sequence_start_record.coordinates,
            StepRecord::SequenceEnd(sequence_end_record) => &sequence_end_record.coordinates,
        }
    }

    pub fn input(&self) -> Option<&FnInput> {
        None
    }

    pub fn output(&self) -> Option<&FnOutput> {
        None
    }
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
