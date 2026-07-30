use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::hash::Hash;
use core::ops::{Deref, DerefMut};
use serde::{Deserialize, Serialize};

use crate::cfs::{CfsCoordinates, SequenceId, TileId};
use crate::draft::DraftTransitionWitness;
use crate::fingerprint::Fingerprint;
use crate::input::{
    encode_index_leaf_payload, selection_payload_hash, Hash32, IndexWidth, SelectionCommitment,
    SelectorPath, SelectorSegment,
};

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

/// A step's storage map violates a [`SelectorSegment::BoundIndex`] obligation.
///
/// Every variant is a rejection, never a warning: a dynamic index whose
/// provenance cannot be checked is exactly the prover-chosen index the segment
/// exists to rule out. See `docs/proposals/dynamic-index-selection.md` §3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundIndexViolation {
    /// `source` names no binding on this step. Fail-closed: an index whose
    /// supplier is absent was never proved by the read loop.
    MissingSource {
        binding: StorageBindingName,
        source: StorageBindingName,
    },
    /// A binding's index cites itself, or a cycle of bindings cite each other.
    ///
    /// Rejected because a cyclic citation lets a prover pick any *fixed point*
    /// — an `i` with `list[i] == i` — and pass it off as a data-derived index.
    /// The index would then be prover-chosen, which is the substitution
    /// `BoundIndex` exists to prevent.
    CyclicSource { binding: StorageBindingName },
    /// The recorded `index` does not fit the width it declares. A truncating
    /// cast here would be a forgery (see [`IndexWidth::encode`]).
    IndexExceedsWidth {
        binding: StorageBindingName,
        index: u64,
        width: IndexWidth,
    },
    /// The source binding commits to a different value than the index claims.
    IndexMismatch {
        binding: StorageBindingName,
        source: StorageBindingName,
        index: u64,
    },
}

/// Collect the `(index, source, width)` of every bound index in a selector.
fn bound_indexes(path: &SelectorPath) -> impl Iterator<Item = (u64, &str, IndexWidth)> {
    path.segments.iter().filter_map(|segment| match segment {
        SelectorSegment::BoundIndex {
            index,
            source,
            width,
        } => Some((*index, source.as_str(), *width)),
        SelectorSegment::Field(_) | SelectorSegment::Index(_) | SelectorSegment::Range { .. } => {
            None
        }
    })
}

/// Discharge the [`SelectorSegment::BoundIndex`] obligations over one step's
/// storage map.
///
/// The read loop that calls this has already proved each entry *individually*:
/// its coordinates commit to its commitment in the store, and its selection
/// witness folds its payload up to that commitment. What it has **not** done is
/// check that a data-sourced index is the value it claims to come from — the
/// element proof only pins the index the segment *claims*
/// (`step_proves_segment`), not where that claim came from. This closes that
/// gap, and it is the whole of the new soundness surface:
///
/// * `source` resolves to a binding on this same step (absent ⇒ reject), so the
///   index can only come from a value the read loop already authorized;
/// * that binding's committed payload is byte-for-byte the canonical encoding of
///   the claimed `index` at the declared width;
/// * the citation graph is acyclic, so no binding bootstraps its own index.
///
/// Scans `selection.path` rather than `selector`: the selection commitment's
/// path is the one `verify_selection_witness` pins the proof to, so it is the
/// path whose segments actually govern which bytes were read.
pub fn verify_bound_index_bindings(storage: &StorageInput) -> Result<(), BoundIndexViolation> {
    for (binding_name, data) in storage {
        for (index, source, width) in bound_indexes(&data.selection.path) {
            if source == binding_name.as_str() {
                return Err(BoundIndexViolation::CyclicSource {
                    binding: binding_name.clone(),
                });
            }

            let Some(source_data) = storage.get(source) else {
                return Err(BoundIndexViolation::MissingSource {
                    binding: binding_name.clone(),
                    source: String::from(source),
                });
            };

            // Encode-and-compare: re-derive the leaf payload the source binding
            // must have committed if the claimed index is honest, and compare
            // one hash. No integer decoder in the verifier, and — because the
            // leaf encoding is fixed-width — no second spelling of a value.
            let Some(expected) = encode_index_leaf_payload(index, width) else {
                return Err(BoundIndexViolation::IndexExceedsWidth {
                    binding: binding_name.clone(),
                    index,
                    width,
                });
            };
            if source_data.selection.selected_len != expected.len() as u64
                || source_data.selection.selected_hash != selection_payload_hash(&expected)
            {
                return Err(BoundIndexViolation::IndexMismatch {
                    binding: binding_name.clone(),
                    source: String::from(source),
                    index,
                });
            }
        }
    }

    detect_bound_index_cycle(storage)
}

/// Reject a cycle in the "binding cites binding" graph.
///
/// Self-citation is caught above; this catches the length-≥2 case (A's index
/// from B, B's index from A), which buys a prover the same fixed-point freedom.
/// Iterative three-colour DFS — the guest has no stack to spare, and a step's
/// binding count is small.
fn detect_bound_index_cycle(storage: &StorageInput) -> Result<(), BoundIndexViolation> {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mark {
        InProgress,
        Done,
    }

    let mut marks: BTreeMap<&str, Mark> = BTreeMap::new();

    for root in storage.keys() {
        if marks.get(root.as_str()) == Some(&Mark::Done) {
            continue;
        }

        // Each frame is (binding, whether its children have been pushed).
        let mut stack: Vec<(&str, bool)> = Vec::from([(root.as_str(), false)]);
        while let Some((binding, expanded)) = stack.pop() {
            if expanded {
                marks.insert(binding, Mark::Done);
                continue;
            }
            match marks.get(binding) {
                Some(Mark::Done) => continue,
                Some(Mark::InProgress) => {
                    return Err(BoundIndexViolation::CyclicSource {
                        binding: String::from(binding),
                    })
                }
                None => {}
            }
            marks.insert(binding, Mark::InProgress);
            stack.push((binding, true));

            if let Some(data) = storage.get(binding) {
                for (_, source, _) in bound_indexes(&data.selection.path) {
                    if marks.get(source) != Some(&Mark::Done) {
                        stack.push((source, false));
                    }
                }
            }
        }
    }

    Ok(())
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
            StepKind::SequenceEnd { .. } | StepKind::ProgramStart(_) | StepKind::ProgramEnd(_) => {
                None
            }
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
            StepKind::ProgramStart(_) | StepKind::ProgramEnd(_) | StepKind::SequenceEnd { .. } => {
                None
            }
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

/// A commitment to one recorded trace: the packed fingerprint over every
/// step's cumulative trace-tree root, plus the first fraud-proof window of
/// steps revealed in the clear.
///
/// The struct lives in `raster-core` (not `raster-prover`) because the
/// transition guest must decode the exact `commit.bin` bytes it refutes: at
/// `Init` it hashes them into the journal's `refuted_trace_commitment` and
/// asserts the window fingerprint is a slice of `fingerprint` at the offset
/// fixed by the window's initial frontier (see
/// `docs/proposals/chain-fraud-proof.md`). Construction and verification
/// (Merkle-tree logic) stay host-side in `raster-prover::trace`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceCommitment {
    pub fingerprint: Fingerprint,
    pub revealed_items: Vec<StepRecord>,
}

impl TraceCommitment {
    /// Fraud-proof window size this commitment was built with.
    pub fn window_size(&self) -> usize {
        self.revealed_items.len()
    }

    /// Get the number of commitments.
    pub fn len(&self) -> usize {
        self.fingerprint.len()
    }

    /// Check if the commitment is empty.
    pub fn is_empty(&self) -> bool {
        self.fingerprint.is_empty()
    }
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

#[cfg(test)]
mod bound_index_tests {
    use super::*;
    use crate::input::encode_index_leaf_payload;
    use alloc::vec;

    /// A storage binding whose selection commits to `value` at `width` — i.e. a
    /// plausible index supplier.
    fn index_supplier(value: u64, width: IndexWidth) -> StorageData {
        let payload = encode_index_leaf_payload(value, width).expect("value fits width");
        StorageData {
            coordinates: CfsCoordinates::new(),
            commitment: vec![1, 2, 3],
            selector: SelectorPath::default(),
            selection: SelectionCommitment {
                path: SelectorPath::default(),
                source_root_hash: [0u8; 32],
                selected_hash: selection_payload_hash(&payload),
                selected_len: payload.len() as u64,
            },
        }
    }

    /// A binding that reaches an element through a bound index citing `source`.
    fn indexed_binding(index: u64, source: &str, width: IndexWidth) -> StorageData {
        let path = SelectorPath::new(vec![
            SelectorSegment::Field(String::from("rows")),
            SelectorSegment::BoundIndex {
                index,
                source: String::from(source),
                width,
            },
        ]);
        StorageData {
            coordinates: CfsCoordinates::new(),
            commitment: vec![4, 5, 6],
            selector: path.clone(),
            selection: SelectionCommitment {
                path,
                source_root_hash: [9u8; 32],
                selected_hash: [7u8; 32],
                selected_len: 32,
            },
        }
    }

    fn map(entries: &[(&str, StorageData)]) -> StorageInput {
        entries
            .iter()
            .map(|(name, data)| (String::from(*name), data.clone()))
            .collect()
    }

    #[test]
    fn accepts_an_honest_citation() {
        let storage = map(&[
            ("row", indexed_binding(7, "@idx/a", IndexWidth::U32)),
            ("@idx/a", index_supplier(7, IndexWidth::U32)),
        ]);
        assert_eq!(verify_bound_index_bindings(&storage), Ok(()));
    }

    #[test]
    fn accepts_two_bindings_sharing_one_index() {
        // The dedup property: "the same index" must mean the same index, so two
        // values citing one supplier is the expected shape, not a conflict.
        let storage = map(&[
            ("row", indexed_binding(3, "@idx/a", IndexWidth::U32)),
            ("ple", indexed_binding(3, "@idx/a", IndexWidth::U32)),
            ("@idx/a", index_supplier(3, IndexWidth::U32)),
        ]);
        assert_eq!(verify_bound_index_bindings(&storage), Ok(()));
    }

    #[test]
    fn rejects_a_substituted_index() {
        // The core forgery: claim element 9 while the authorized value says 7.
        let storage = map(&[
            ("row", indexed_binding(9, "@idx/a", IndexWidth::U32)),
            ("@idx/a", index_supplier(7, IndexWidth::U32)),
        ]);
        assert_eq!(
            verify_bound_index_bindings(&storage),
            Err(BoundIndexViolation::IndexMismatch {
                binding: String::from("row"),
                source: String::from("@idx/a"),
                index: 9,
            })
        );
    }

    #[test]
    fn rejects_an_absent_source() {
        let storage = map(&[("row", indexed_binding(7, "@idx/missing", IndexWidth::U32))]);
        assert_eq!(
            verify_bound_index_bindings(&storage),
            Err(BoundIndexViolation::MissingSource {
                binding: String::from("row"),
                source: String::from("@idx/missing"),
            })
        );
    }

    #[test]
    fn rejects_self_citation() {
        let storage = map(&[("row", indexed_binding(7, "row", IndexWidth::U32))]);
        assert_eq!(
            verify_bound_index_bindings(&storage),
            Err(BoundIndexViolation::CyclicSource {
                binding: String::from("row"),
            })
        );
    }

    #[test]
    fn rejects_a_two_binding_cycle() {
        // Neither binding cites itself, so only the graph walk catches this.
        // Both must still pass encode-and-compare, so make each commit to the
        // index the other claims.
        let mut a = indexed_binding(5, "b", IndexWidth::U32);
        let mut b = indexed_binding(5, "a", IndexWidth::U32);
        let payload = encode_index_leaf_payload(5, IndexWidth::U32).unwrap();
        for data in [&mut a, &mut b] {
            data.selection.selected_hash = selection_payload_hash(&payload);
            data.selection.selected_len = payload.len() as u64;
        }
        let storage = map(&[("a", a), ("b", b)]);
        assert!(matches!(
            verify_bound_index_bindings(&storage),
            Err(BoundIndexViolation::CyclicSource { .. })
        ));
    }

    #[test]
    fn rejects_an_index_that_overflows_its_declared_width() {
        // The truncation forgery: 300 does not fit a u8, and `300 as u8` is 44.
        // If the verifier cast instead of range-checking, this would match a
        // supplier that honestly committed 44 — proving element 300 on the
        // strength of a value that says 44.
        let storage = map(&[
            ("row", indexed_binding(300, "@idx/a", IndexWidth::U8)),
            ("@idx/a", index_supplier(44, IndexWidth::U8)),
        ]);
        assert_eq!(
            verify_bound_index_bindings(&storage),
            Err(BoundIndexViolation::IndexExceedsWidth {
                binding: String::from("row"),
                index: 300,
                width: IndexWidth::U8,
            })
        );
    }

    #[test]
    fn rejects_a_width_confused_citation() {
        // Same numeric value, different committed width: a u32 leaf is four
        // bytes where a u64 leaf is eight, so the encodings differ and the
        // citation must not verify.
        let storage = map(&[
            ("row", indexed_binding(7, "@idx/a", IndexWidth::U64)),
            ("@idx/a", index_supplier(7, IndexWidth::U32)),
        ]);
        assert!(matches!(
            verify_bound_index_bindings(&storage),
            Err(BoundIndexViolation::IndexMismatch { .. })
        ));
    }

    #[test]
    fn ignores_literal_index_paths() {
        // Every existing program: no bound index, nothing to discharge.
        let plain = StorageData {
            coordinates: CfsCoordinates::new(),
            commitment: vec![1],
            selector: SelectorPath::default(),
            selection: SelectionCommitment {
                path: SelectorPath::new(vec![
                    SelectorSegment::Field(String::from("rows")),
                    SelectorSegment::Index(7),
                    SelectorSegment::Range { start: 0, end: 2 },
                ]),
                source_root_hash: [0u8; 32],
                selected_hash: [0u8; 32],
                selected_len: 0,
            },
        };
        assert_eq!(verify_bound_index_bindings(&map(&[("row", plain)])), Ok(()));
    }

    #[test]
    fn accepts_a_chain_of_distinct_citations() {
        // Nesting is allowed as long as it is acyclic: `outer` cites `inner`,
        // which itself reaches its value through a bound index citing a plain
        // supplier.
        let payload = encode_index_leaf_payload(2, IndexWidth::U32).unwrap();
        let mut inner = indexed_binding(4, "@idx/base", IndexWidth::U32);
        inner.selection.selected_hash = selection_payload_hash(&payload);
        inner.selection.selected_len = payload.len() as u64;

        let storage = map(&[
            ("outer", indexed_binding(2, "inner", IndexWidth::U32)),
            ("inner", inner),
            ("@idx/base", index_supplier(4, IndexWidth::U32)),
        ]);
        assert_eq!(verify_bound_index_bindings(&storage), Ok(()));
    }
}
