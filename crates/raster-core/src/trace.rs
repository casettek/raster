use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::hash::Hash;
use core::ops::{Deref, DerefMut};
use serde::{Deserialize, Serialize};

use crate::cfs::CfsCoordinates;
use crate::draft::DraftTransitionWitness;
use crate::fingerprint::Fingerprint;
use crate::input::{Hash32, SelectionCommitment, SelectorPath};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FnInputArg {
    /// Parameter name from the function signature
    pub name: String,
    /// Type name as a string (e.g., "u64", "String")
    pub ty: String,
}

/// Describes the input parameters for a tile function.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FnInput {
    pub data: Vec<u8>,
    pub values: Vec<FnInputValue>,
    pub args: Vec<FnInputArg>,
    pub internal: InternalInput,
}

pub type InternalBindingName = String;
pub type InternalInput = BTreeMap<InternalBindingName, InternalData>;

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

    pub fn internal(&self) -> &InternalInput {
        &self.internal
    }

    pub fn source_witness_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(&(self.values.clone(), self.internal.clone())).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RasterPayload {
    pub bytes: Vec<u8>,
    pub index_bytes: Vec<u8>,
    pub root_hash: Hash32,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SequenceEndRecord {
    pub exec_index: u64,

    pub sequence_id: String,

    pub coordinates: CfsCoordinates,

    pub output_commitment: Vec<u8>,
}

/// What kind of entry-time preparation an `EntrypointRecord` performs.
/// Currently only one op exists; new entry-time-authorized preparations
/// (checked against the authorization journal rather than a replay proof)
/// can be added here without growing `StepRecord` again.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum EntrypointOp {
    /// Binds `main`'s declared external arguments, in CFS declaration
    /// order, to a single internal-store object at this step's coordinates.
    BindEntryArguments { names: Vec<String> },
}

/// A step that authorizes something at sequence entry against the
/// authorization journal, rather than against a replay proof. Only ever
/// emitted for `main`; currently only `main`'s declared external-argument
/// binding uses this.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct EntrypointRecord {
    pub exec_index: u64,

    pub sequence_id: String,

    pub coordinates: CfsCoordinates,

    pub op: EntrypointOp,

    pub input_source_commitment: Vec<u8>,
    pub output_commitment: Vec<u8>,

    pub internal_store_root_before: Vec<u8>,
    pub internal_store_root_after: Vec<u8>,
    pub internal_store_index_root_before: Vec<u8>,
    pub internal_store_index_root_after: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum StepRecord {
    SequenceStart(SequenceStartRecord),
    SequenceEnd(SequenceEndRecord),
    TileExec(TileExecRecord),
    RecurTileExec(RecurTileExecRecord),
    RecurSequenceExec(RecurSequenceExecRecord),
    Entrypoint(EntrypointRecord),
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
            StepRecord::Entrypoint(entrypoint_record) => &entrypoint_record.coordinates,
        }
    }

    pub fn input_commitment(&self) -> Option<&Vec<u8>> {
        match self {
            StepRecord::TileExec(record) => Some(&record.input_commitment),
            StepRecord::RecurTileExec(record) => Some(&record.input_commitment),
            StepRecord::RecurSequenceExec(record) => Some(&record.input_commitment),
            StepRecord::SequenceStart(record) => Some(&record.input_commitment),
            StepRecord::SequenceEnd(_) => None,
            // No serialized "input" analogous to a tile's ABI bytes — this
            // step declares bindings, it does not consume any.
            StepRecord::Entrypoint(_) => None,
        }
    }

    pub fn output_commitment(&self) -> Option<&Vec<u8>> {
        match self {
            StepRecord::TileExec(record) => Some(&record.output_commitment),
            StepRecord::RecurTileExec(record) => Some(&record.output_commitment),
            StepRecord::RecurSequenceExec(record) => Some(&record.output_commitment),
            StepRecord::SequenceStart(_) => None,
            StepRecord::SequenceEnd(record) => Some(&record.output_commitment),
            StepRecord::Entrypoint(record) => Some(&record.output_commitment),
        }
    }

    pub fn input_source_commitment(&self) -> Option<&Vec<u8>> {
        match self {
            StepRecord::TileExec(record) => Some(&record.input_source_commitment),
            StepRecord::RecurTileExec(record) => Some(&record.input_source_commitment),
            StepRecord::RecurSequenceExec(record) => Some(&record.input_source_commitment),
            StepRecord::SequenceStart(record) => Some(&record.input_source_commitment),
            StepRecord::SequenceEnd(_) => None,
            StepRecord::Entrypoint(record) => Some(&record.input_source_commitment),
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
            StepRecord::Entrypoint(record) => Some((
                &record.internal_store_root_before,
                &record.internal_store_root_after,
                &record.internal_store_index_root_before,
                &record.internal_store_index_root_after,
            )),
            StepRecord::SequenceStart(_) | StepRecord::SequenceEnd(_) => None,
        }
    }

    /// Whether this step's `output_commitment` is verified through a
    /// mechanism other than a direct byte-witness comparison: a replay
    /// proof for `TileExec`/`RecurTileExec`/`RecurSequenceExec`, or the
    /// authorization journal for `Entrypoint`.
    pub fn is_execution_step(&self) -> bool {
        matches!(
            self,
            StepRecord::TileExec(_)
                | StepRecord::RecurTileExec(_)
                | StepRecord::RecurSequenceExec(_)
                | StepRecord::Entrypoint(_)
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

/// One declared `main` entry argument, as bound at runtime — enough for
/// the recorder to independently reconstruct the `Referenced` object's
/// commitment without touching any file bytes. `encoding` says which
/// selection mechanism applies; per-source deserialization capability for
/// `Postcard` sources (which aren't self-describing) is looked up by name
/// from the entry-argument kit registry populated by `bind_entry_arguments`
/// — it can't travel through this event, since it isn't serializable data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntrypointArgumentBinding {
    pub name: String,
    pub encoding: crate::input::ExternalEncoding,
    pub commitment: Vec<u8>,
}

/// Recorded once, at the top of `main`, when it declares external entry
/// arguments — carries just enough for the recorder to rebuild the
/// matching `Referenced` object and `EntrypointRecord` (the live write
/// already happened in the internal store by the time this is published).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntrypointBindEvent {
    pub arguments: Vec<EntrypointArgumentBinding>,
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

    EntrypointBind(EntrypointBindEvent),
}
