use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::hash::Hash;
use core::ops::{Deref, DerefMut};
use serde::{Deserialize, Serialize};

use crate::cfs::{CfsCoordinates, SequenceId, TileId};
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
    pub storage: StorageInput,
}

pub type StorageBindingName = String;
pub type StorageInput = BTreeMap<StorageBindingName, StorageData>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct StorageData {
    pub coordinates: CfsCoordinates,
    pub commitment: Vec<u8>,
    pub selector: SelectorPath,
    pub selection: SelectionCommitment,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum FnInputValue {
    Inline(Vec<u8>),
    StorageBinding,
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

    pub fn storage(&self) -> &StorageInput {
        &self.storage
    }

    pub fn source_witness_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(&(self.values.clone(), self.storage.clone())).unwrap_or_default()
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

/// The storage roots either side of a step that may write to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct StorageRoots {
    pub root_before: Vec<u8>,
    pub root_after: Vec<u8>,
    pub index_root_before: Vec<u8>,
    pub index_root_after: Vec<u8>,
}

/// What an [`ExecStep`] ran. The distinction is not cosmetic: it decides how
/// the step's output is verified (only `Tile` carries a replay proof) and
/// which CFS item kind the step may occupy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ExecTarget {
    Tile(TileId),
    RecurTile(TileId),
    RecurSequence(SequenceId),
}

/// A step that ran something and committed to what it consumed and produced.
///
/// The three targets share one shape deliberately: they commit to exactly
/// the same things and differ only in what ran, so a field added here cannot
/// be added to two of them and forgotten on the third.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ExecStep {
    pub target: ExecTarget,
    pub intra_sequence_index: u32,

    pub input_commitment: Vec<u8>,
    pub input_source_commitment: Vec<u8>,
    pub output_commitment: Vec<u8>,

    pub storage: StorageRoots,
}

/// What kind of entry-time preparation an [`EntrypointStep`] performs.
/// Currently only one op exists; new entry-time-authorized preparations
/// (checked against the authorization journal rather than a replay proof)
/// can be added here without growing [`StepKind`] again.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum EntrypointOp {
    /// Binds `main`'s declared external arguments, in CFS declaration
    /// order, to a single storage object at this step's coordinates.
    BindEntryArguments { names: Vec<String> },
}

/// A step that authorizes something at sequence entry against the
/// authorization journal, rather than against a replay proof. Only ever
/// emitted for `main`; currently only `main`'s declared external-argument
/// binding uses this.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct EntrypointStep {
    pub op: EntrypointOp,

    pub input_source_commitment: Vec<u8>,
    pub output_commitment: Vec<u8>,

    pub storage: StorageRoots,
}

/// What a step did, and the commitments that go with it.
///
/// Each kind carries exactly the commitments it makes — no more, and none of
/// them optional. That is what lets the guest's checks be total: an absent
/// commitment is a kind that does not make one, never a kind that failed to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum StepKind {
    SequenceStart {
        input_commitment: Vec<u8>,
        input_source_commitment: Vec<u8>,
    },
    SequenceEnd {
        output_commitment: Vec<u8>,
    },
    Exec(ExecStep),
    Entrypoint(EntrypointStep),
}

/// One step of a trace: where it sits, and what it did.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct StepRecord {
    pub exec_index: u64,
    pub sequence_id: String,
    pub coordinates: CfsCoordinates,
    pub kind: StepKind,
}

impl StepRecord {
    pub fn coordinates(&self) -> &CfsCoordinates {
        &self.coordinates
    }

    /// The step's own serialized input bytes, for kinds that consume any.
    pub fn input_commitment(&self) -> Option<&Vec<u8>> {
        match &self.kind {
            StepKind::SequenceStart {
                input_commitment, ..
            } => Some(input_commitment),
            StepKind::Exec(exec) => Some(&exec.input_commitment),
            // An entrypoint declares bindings rather than consuming an
            // input, and a sequence end only reports what it produced.
            StepKind::SequenceEnd { .. } | StepKind::Entrypoint(_) => None,
        }
    }

    pub fn output_commitment(&self) -> Option<&Vec<u8>> {
        match &self.kind {
            StepKind::SequenceEnd { output_commitment } => Some(output_commitment),
            StepKind::Exec(exec) => Some(&exec.output_commitment),
            StepKind::Entrypoint(entrypoint) => Some(&entrypoint.output_commitment),
            StepKind::SequenceStart { .. } => None,
        }
    }

    pub fn input_source_commitment(&self) -> Option<&Vec<u8>> {
        match &self.kind {
            StepKind::SequenceStart {
                input_source_commitment,
                ..
            } => Some(input_source_commitment),
            StepKind::Exec(exec) => Some(&exec.input_source_commitment),
            StepKind::Entrypoint(entrypoint) => Some(&entrypoint.input_source_commitment),
            StepKind::SequenceEnd { .. } => None,
        }
    }

    /// The storage roots this step claims, for kinds that may write.
    /// Sequence boundaries never touch the store, so they have none.
    pub fn storage_roots(&self) -> Option<&StorageRoots> {
        match &self.kind {
            StepKind::Exec(exec) => Some(&exec.storage),
            StepKind::Entrypoint(entrypoint) => Some(&entrypoint.storage),
            StepKind::SequenceStart { .. } | StepKind::SequenceEnd { .. } => None,
        }
    }

    /// Whether this step's `output_commitment` is verified through a
    /// mechanism other than a direct byte-witness comparison: a replay
    /// proof for a tile, or the authorization journal for an entrypoint.
    pub fn is_execution_step(&self) -> bool {
        matches!(self.kind, StepKind::Exec(_) | StepKind::Entrypoint(_))
    }

    pub fn requires_replay_proof(&self) -> bool {
        matches!(
            &self.kind,
            StepKind::Exec(ExecStep {
                target: ExecTarget::Tile(_),
                ..
            })
        )
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
/// already happened in storage by the time this is published).
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
