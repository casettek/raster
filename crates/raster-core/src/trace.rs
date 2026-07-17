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

/// The trace's first step: the program starts, and `main`'s declared
/// external arguments (if any) are loaded into a single authorized storage
/// object at the sequence root (coordinates `[]`).
///
/// This is the one step whose output is tied to the public manifest through
/// the authorization journal rather than a replay proof (see the transition
/// guest's `checks::entrypoint`). It is always emitted, even when `main`
/// declares no entry arguments — in that case it binds nothing and touches
/// no storage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ProgramStartStep {
    /// `main`'s declared entry-argument names, in CFS declaration order.
    /// Empty when `main` declares none.
    pub entry_arguments: Vec<String>,

    /// The struct-of-commitments root over the authorized per-argument
    /// commitments — the commitment of the combined entry object written at
    /// coordinates `[]`. Empty when there are no arguments (no write).
    pub output_commitment: Vec<u8>,

    /// Genesis roots -> roots containing the entry object, or unchanged when
    /// there are no arguments.
    pub storage: StorageRoots,
}

/// The trace's last step: `main` returned, and the program's output — a value
/// that provably lives in committed storage — is committed as the authorized
/// program output (see the transition guest's `checks::program`).
///
/// `main` must return either unit or a storage-backed value (a tile or
/// `select!` result); an inline literal is rejected before this step is
/// reached. A `main` that returned `Err` or panicked never produces a
/// `ProgramEnd` at all — an incomplete trace is simply unattestable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ProgramEndStep {
    /// Where the program output lives: its storage coordinates, the source
    /// object's commitment, and the selection that narrows to the returned
    /// value. `None` when `main` returns unit.
    pub output: Option<StorageData>,

    /// The committed program output: the `selected_hash` of `output`'s
    /// selection. Empty when `main` returns unit.
    pub output_commitment: Vec<u8>,

    /// Storage roots — unchanged; a program end reads its output but writes
    /// nothing.
    pub storage: StorageRoots,
}

/// What a step did, and the commitments that go with it.
///
/// Each kind carries exactly the commitments it makes — no more, and none of
/// them optional. That is what lets the guest's checks be total: an absent
/// commitment is a kind that does not make one, never a kind that failed to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum StepKind {
    /// The trace's first step: starts the program and binds `main`'s entry
    /// arguments (see [`ProgramStartStep`]). Always at coordinates `[]`.
    ProgramStart(ProgramStartStep),
    /// The trace's last step: commits `main`'s authorized output (see
    /// [`ProgramEndStep`]). Always at coordinates `[]`.
    ProgramEnd(ProgramEndStep),
    SequenceStart {
        input_commitment: Vec<u8>,
        input_source_commitment: Vec<u8>,
    },
    SequenceEnd {
        output_commitment: Vec<u8>,
    },
    Exec(ExecStep),
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
            // A program start binds authorized external data and a program end
            // commits an authorized output rather than consuming a step input;
            // a sequence end only reports what it produced.
            StepKind::SequenceEnd { .. }
            | StepKind::ProgramStart(_)
            | StepKind::ProgramEnd(_) => None,
        }
    }

    pub fn output_commitment(&self) -> Option<&Vec<u8>> {
        match &self.kind {
            StepKind::SequenceEnd { output_commitment } => Some(output_commitment),
            StepKind::Exec(exec) => Some(&exec.output_commitment),
            StepKind::ProgramStart(program_start) => Some(&program_start.output_commitment),
            StepKind::ProgramEnd(program_end) => Some(&program_end.output_commitment),
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
            // A program start makes no input commitment at all (its "input" is
            // the outside world, authorized against the manifest journal), and
            // a program end carries its output binding in the record itself
            // rather than as a bound source witness.
            StepKind::ProgramStart(_)
            | StepKind::ProgramEnd(_)
            | StepKind::SequenceEnd { .. } => None,
        }
    }

    /// The storage roots this step claims, for kinds that touch the store.
    /// Sequence boundaries never touch it, so they have none. A program end
    /// reads its output (roots unchanged) so it claims them too.
    pub fn storage_roots(&self) -> Option<&StorageRoots> {
        match &self.kind {
            StepKind::Exec(exec) => Some(&exec.storage),
            StepKind::ProgramStart(program_start) => Some(&program_start.storage),
            StepKind::ProgramEnd(program_end) => Some(&program_end.storage),
            StepKind::SequenceStart { .. } | StepKind::SequenceEnd { .. } => None,
        }
    }

    /// Whether this step's `output_commitment` is verified through a
    /// mechanism other than a direct byte-witness comparison: a replay proof
    /// for a tile, the authorization journal for a program start, or a
    /// storage selection proof for a program end.
    pub fn is_execution_step(&self) -> bool {
        matches!(
            self.kind,
            StepKind::Exec(_) | StepKind::ProgramStart(_) | StepKind::ProgramEnd(_)
        )
    }

    /// Whether this step appends an object to storage, as opposed to only
    /// reading it (`ProgramEnd`) or not touching it (sequence boundaries).
    /// This decides which step owns the write recorded at a coordinate —
    /// necessary because `ProgramStart` (append) and `ProgramEnd` (read-only)
    /// share coordinates `[]` and thus a witness-store entry.
    pub fn appends_to_storage(&self) -> bool {
        matches!(self.kind, StepKind::Exec(_) | StepKind::ProgramStart(_))
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
/// from the entry-argument kit registry populated by `start_program`
/// — it can't travel through this event, since it isn't serializable data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntrypointArgumentBinding {
    pub name: String,
    pub encoding: crate::input::ExternalEncoding,
    pub commitment: Vec<u8>,
}

/// Recorded once, as the program's first event: `main` starts, and its
/// declared external entry arguments (if any) are bound. Carries just enough
/// for the recorder to rebuild the matching `Referenced` object (the live
/// write already happened in storage by the time this is published). The
/// `arguments` list is empty when `main` declares no entry arguments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramStartEvent {
    pub arguments: Vec<EntrypointArgumentBinding>,
}

/// Recorded once, as the program's last event: `main` returned its authorized
/// output. `output` is the storage binding of the returned value (already
/// committed in storage by a verified tile), or `None` when `main` returns
/// unit. Only emitted on success — a failed `main` publishes nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramEndEvent {
    pub output: Option<StorageData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TraceEvent {
    ProgramStart(ProgramStartEvent),
    ProgramEnd(ProgramEndEvent),

    SequenceStart(FnCallRecord),
    SequenceEnd(FnCallRecord),
    RecurSequenceStart(FnCallRecord),
    RecurSequenceEnd(FnCallRecord),

    TileExec(FnCallRecord),
    RecurTileIterationExec(FnCallRecord),
    RecurTileExec(FnCallRecord),
    RecurSequenceExec(FnCallRecord),
}
