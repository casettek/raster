//! Trace types (requires std feature).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::hash::Hash;
use core::ops::{Deref, DerefMut};
use serde::{Deserialize, Serialize};

use crate::cfs::CfsCoordinates;
use crate::draft::DraftTransitionWitness;
use crate::fingerprint::Fingerprint;
use crate::input::{SelectionCommitment, SelectorPath};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FnInputArg {
    /// Parameter name from the function signature
    pub name: String,
    /// Type name as a string (e.g., "u64", "String")
    pub ty: String,
}

/// Describes an external input parameter for a tile function.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FnInput {
    pub data: Vec<u8>,
    pub values: Vec<FnInputValue>,
    pub args: Vec<FnInputArg>,
    pub external: ExternalInput,
    pub internal: InternalInput,
}

pub type InternalBindingName = String;
pub type ExternalInput = BTreeMap<InternalBindingName, ExternalData>;
pub type InternalInput = BTreeMap<InternalBindingName, InternalData>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ExternalData {
    pub name: String,
    pub commitment: Vec<u8>,
    pub tree_root: Vec<u8>,
    pub selector: SelectorPath,
    pub selection: SelectionCommitment,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct InternalData {
    pub coordinates: CfsCoordinates,
    pub commitment: Vec<u8>,
    pub selector: SelectorPath,
    pub selection: SelectionCommitment,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum FnInputValue {
    Inline(Vec<u8>),
    ExternalBinding,
    InternalBinding,
}

impl FnInput {
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn args(&self) -> &[FnInputArg] {
        &self.args
    }

    pub fn values(&self) -> &[FnInputValue] {
        &self.values
    }

    pub fn external(&self) -> &ExternalInput {
        &self.external
    }

    pub fn internal(&self) -> &InternalInput {
        &self.internal
    }

    pub fn source_witness_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(&(
            self.values.clone(),
            self.external.clone(),
            self.internal.clone(),
        ))
        .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RasterPayload {
    pub bytes: Vec<u8>,
    pub index_bytes: Vec<u8>,
    pub root_hash: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FnOutput {
    pub data: Vec<u8>,
    pub ty: String,
    pub raster: Option<RasterPayload>,
}

impl FnOutput {
    pub fn new(data: Vec<u8>, ty: impl Into<String>) -> Self {
        Self {
            data,
            ty: ty.into(),
            raster: None,
        }
    }

    pub fn with_raster(mut self, raster: RasterPayload) -> Self {
        self.raster = Some(raster);
        self
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
    pub draft_transition_witness: Option<DraftTransitionWitness>,
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
    pub intra_sequence_index: u32,

    pub coordinates: CfsCoordinates,

    pub input_commitment: Vec<u8>,
    pub input_source_commitment: Vec<u8>,
    pub output_commitment: Vec<u8>,

    pub external_input_commitment: Vec<u8>,
    pub internal_store_root_before: Vec<u8>,
    pub internal_store_root_after: Vec<u8>,
    pub internal_store_index_root_before: Vec<u8>,
    pub internal_store_index_root_after: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RecurTileExecRecord {
    pub exec_index: u64,

    pub recur_tile_id: String,

    pub sequence_id: String,
    pub intra_sequence_index: u32,

    pub coordinates: CfsCoordinates,

    pub input_commitment: Vec<u8>,
    pub input_source_commitment: Vec<u8>,
    pub output_commitment: Vec<u8>,

    pub external_input_commitment: Vec<u8>,
    pub internal_store_root_before: Vec<u8>,
    pub internal_store_root_after: Vec<u8>,
    pub internal_store_index_root_before: Vec<u8>,
    pub internal_store_index_root_after: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RecurSequenceExecRecord {
    pub exec_index: u64,

    pub recur_sequence_id: String,

    pub sequence_id: String,
    pub intra_sequence_index: u32,

    pub coordinates: CfsCoordinates,

    pub input_commitment: Vec<u8>,
    pub input_source_commitment: Vec<u8>,
    pub output_commitment: Vec<u8>,

    pub external_input_commitment: Vec<u8>,
    pub internal_store_root_before: Vec<u8>,
    pub internal_store_root_after: Vec<u8>,
    pub internal_store_index_root_before: Vec<u8>,
    pub internal_store_index_root_after: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SequenceStartRecord {
    pub exec_index: u64,

    pub sequence_id: String,

    pub coordinates: CfsCoordinates,

    pub input_commitment: Vec<u8>,
    pub input_source_commitment: Vec<u8>,
    pub external_input_commitment: Vec<u8>,
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
    RecurTileExec(RecurTileExecRecord),
    RecurSequenceExec(RecurSequenceExecRecord),
}

impl StepRecord {
    pub fn coordinates(&self) -> &CfsCoordinates {
        match self {
            StepRecord::TileExec(tile_exec_record) => &tile_exec_record.coordinates,
            StepRecord::RecurTileExec(recur_exec_record) => &recur_exec_record.coordinates,
            StepRecord::RecurSequenceExec(recur_sequence_exec_record) => {
                &recur_sequence_exec_record.coordinates
            }
            StepRecord::SequenceStart(sequence_start_record) => &sequence_start_record.coordinates,
            StepRecord::SequenceEnd(sequence_end_record) => &sequence_end_record.coordinates,
        }
    }

    pub fn input_commitment(&self) -> Option<&Vec<u8>> {
        match self {
            StepRecord::TileExec(record) => Some(&record.input_commitment),
            StepRecord::RecurTileExec(record) => Some(&record.input_commitment),
            StepRecord::RecurSequenceExec(record) => Some(&record.input_commitment),
            StepRecord::SequenceStart(record) => Some(&record.input_commitment),
            StepRecord::SequenceEnd(_) => None,
        }
    }

    pub fn output_commitment(&self) -> Option<&Vec<u8>> {
        match self {
            StepRecord::TileExec(record) => Some(&record.output_commitment),
            StepRecord::RecurTileExec(record) => Some(&record.output_commitment),
            StepRecord::RecurSequenceExec(record) => Some(&record.output_commitment),
            StepRecord::SequenceStart(_) => None,
            StepRecord::SequenceEnd(record) => Some(&record.output_commitment),
        }
    }

    pub fn input_source_commitment(&self) -> Option<&Vec<u8>> {
        match self {
            StepRecord::TileExec(record) => Some(&record.input_source_commitment),
            StepRecord::RecurTileExec(record) => Some(&record.input_source_commitment),
            StepRecord::RecurSequenceExec(record) => Some(&record.input_source_commitment),
            StepRecord::SequenceStart(record) => Some(&record.input_source_commitment),
            StepRecord::SequenceEnd(_) => None,
        }
    }

    pub fn external_input_commitment(&self) -> Option<&Vec<u8>> {
        match self {
            StepRecord::TileExec(record) => Some(&record.external_input_commitment),
            StepRecord::RecurTileExec(record) => Some(&record.external_input_commitment),
            StepRecord::RecurSequenceExec(record) => Some(&record.external_input_commitment),
            StepRecord::SequenceStart(record) => Some(&record.external_input_commitment),
            StepRecord::SequenceEnd(_) => None,
        }
    }

    pub fn internal_store_roots(&self) -> Option<(&Vec<u8>, &Vec<u8>, &Vec<u8>, &Vec<u8>)> {
        match self {
            StepRecord::TileExec(record) => Some((
                &record.internal_store_root_before,
                &record.internal_store_root_after,
                &record.internal_store_index_root_before,
                &record.internal_store_index_root_after,
            )),
            StepRecord::RecurTileExec(record) => Some((
                &record.internal_store_root_before,
                &record.internal_store_root_after,
                &record.internal_store_index_root_before,
                &record.internal_store_index_root_after,
            )),
            StepRecord::RecurSequenceExec(record) => Some((
                &record.internal_store_root_before,
                &record.internal_store_root_after,
                &record.internal_store_index_root_before,
                &record.internal_store_index_root_after,
            )),
            StepRecord::SequenceStart(_) | StepRecord::SequenceEnd(_) => None,
        }
    }

    pub fn is_execution_step(&self) -> bool {
        matches!(
            self,
            StepRecord::TileExec(_)
                | StepRecord::RecurTileExec(_)
                | StepRecord::RecurSequenceExec(_)
        )
    }

    pub fn requires_replay_proof(&self) -> bool {
        matches!(self, StepRecord::TileExec(_))
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
    type IntoIter = alloc::vec::IntoIter<StepRecord>;

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
    RecurSequenceStart(FnCallRecord),
    RecurSequenceEnd(FnCallRecord),

    TileExec(FnCallRecord),
    RecurTileIterationExec(FnCallRecord),
    RecurTileExec(FnCallRecord),
    RecurSequenceExec(FnCallRecord),
}
