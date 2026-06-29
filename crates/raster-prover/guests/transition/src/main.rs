//! RISC0 guest program for trace state transitions.
//!
//! This guest performs a single state transition of the bridge tree by:
//! 1. Taking a serialized frontier + trace item data as input
//! 2. Hashing the trace item and appending it to the frontier
//! 3. Returning the new frontier

use std::cmp::Ordering;
use std::collections::BTreeMap;

use bridgetree::{Hashable, Level, NonEmptyFrontier, Position};
use risc0_zkvm::guest::env;
use risc0_zkvm::sha::{Impl as Risc0Sha256, Sha256 as _};

use raster_core::authorization::AuthorizationJournal;
use raster_core::cfs::{
    CfsCoordinates, CfsCursor, ControlFlowSchema, InputBinding, InputSource,
};
use raster_core::coordinate_index::{
    verify_coordinate_index_membership, verify_coordinate_index_non_membership,
};
use raster_core::draft::{
    apply_draft_ops, schema_hash as compute_schema_hash, verify_witness_root, DraftReplayTransition,
    DraftTransitionWitness, TileReplayJournal, TrackedDraftState,
};
use raster_core::fingerprint::{Fingerprint, FingerprintAccumulator};
use raster_core::input::verify_selection_witness;
use raster_core::trace::{ExternalData, ExternalInput, FnInput, FnInputValue, InternalData, StepRecord};
use raster_core::transition::{
    InternalStoreEntry, InternalStoreLogWitness, InternalStoreReadWitness, InternalStoreWitness,
    InternalStoreWriteWitness, SerializableFrontier, Transition, TransitionInput,
    TransitionJournal, TransitionState,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bytes(pub Vec<u8>);

pub type TraceBridgeTree = bridgetree::BridgeTree<Bytes, u64, 32>;

// ============================================================================
// Bytes + Hashable for bridgetree (matches prover's empty leaf and combine)
// ============================================================================

const HASH_SIZE: usize = 32;

/// Empty leaf hash (precomputed SHA256 of "empty"); matches prover EMPTY_TRIE_NODES[0].
const EMPTY_LEAF: [u8; 32] =
    hex_literal::hex!("6d97a6c02676a41a9636c6cd4e5d2d47d14d27a35d18e608115fd93cd42e6b3a");

impl PartialEq for Bytes {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Bytes {}

impl PartialOrd for Bytes {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Bytes {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl Hashable for Bytes {
    fn empty_leaf() -> Self {
        Bytes(EMPTY_LEAF.to_vec())
    }

    fn combine(level: Level, a: &Self, b: &Self) -> Self {
        let mut data = Vec::with_capacity(1 + HASH_SIZE + HASH_SIZE);
        data.push(u8::from(level));
        data.extend_from_slice(&a.0);
        data.extend_from_slice(&b.0);
        Bytes(sha256_bytes(&data))
    }
}

// ============================================================================
// SerializableFrontier <-> NonEmptyFrontier<Bytes> (guest-local conversion)
// ============================================================================

fn deserialize_frontier(ser: &SerializableFrontier) -> Option<NonEmptyFrontier<Bytes>> {
    NonEmptyFrontier::from_parts(
        Position::from(ser.position),
        Bytes(ser.leaf.clone()),
        ser.ommers.iter().map(|o| Bytes(o.clone())).collect(),
    )
    .ok()
}

fn serialize_frontier(frontier: &NonEmptyFrontier<Bytes>) -> SerializableFrontier {
    SerializableFrontier {
        position: frontier.position().into(),
        leaf: frontier.leaf().0.clone(),
        ommers: frontier.ommers().iter().map(|o| o.0.clone()).collect(),
    }
}

// ============================================================================
// Hashing
// ============================================================================

/// Hash a TileExecRecord using SHA256 of its postcard-serialized form.
fn hash_trace_item(item: &StepRecord) -> Vec<u8> {
    let data = postcard::to_allocvec(item).expect("Failed to serialize TileExecRecord");
    sha256_bytes(&data)
}

fn sha256_bytes(bytes: &[u8]) -> Vec<u8> {
    Risc0Sha256::hash_bytes(bytes).as_bytes().to_vec()
}

fn frontier_root(frontier: &NonEmptyFrontier<Bytes>) -> Vec<u8> {
    TraceBridgeTree::from_frontier(1, frontier.clone())
        .root(0)
        .expect("Can't get current frontier root")
        .0
}

fn sha256_hex(bytes: &[u8]) -> Vec<u8> {
    let digest = sha256_bytes(bytes);
    let mut out = Vec::with_capacity(digest.len() * 2);
    for byte in digest {
        let hi = (byte >> 4) & 0x0f;
        let lo = byte & 0x0f;
        out.push(if hi < 10 { b'0' + hi } else { b'a' + (hi - 10) });
        out.push(if lo < 10 { b'0' + lo } else { b'a' + (lo - 10) });
    }
    out
}

fn external_input_commitment(external_input: &ExternalInput) -> Vec<u8> {
    let bytes = postcard::to_allocvec(external_input).unwrap_or_default();
    sha256_bytes(&bytes)
}

fn input_source_commitment(input: &FnInput) -> Vec<u8> {
    sha256_bytes(&input.source_witness_bytes())
}

fn internal_store_leaf_hash(entry: &InternalStoreEntry) -> Vec<u8> {
    sha256_bytes(&entry.to_bytes())
}

fn combine_merkle_level(level: usize, left: &[u8], right: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + left.len() + right.len());
    data.push(level as u8);
    data.extend_from_slice(left);
    data.extend_from_slice(right);
    sha256_bytes(&data)
}

fn append_log_root_from_witness(entry: &InternalStoreEntry, witness: &InternalStoreLogWitness) -> Vec<u8> {
    let mut current = internal_store_leaf_hash(entry);
    for (level, sibling) in witness.path_elems.iter().enumerate() {
        current = if ((witness.position >> level) & 1) == 0 {
            combine_merkle_level(level, &current, sibling)
        } else {
            combine_merkle_level(level, sibling, &current)
        };
    }
    current
}

fn verify_internal_store_read_witness(
    read_witness: &InternalStoreReadWitness,
    current_log_root: &[u8],
    current_index_root: &[u8],
    coordinates: &CfsCoordinates,
    commitment: &[u8],
) {
    assert_eq!(
        read_witness.entry.coordinates, *coordinates,
        "Internal store read witness coordinates do not match requested coordinates",
    );
    assert_eq!(
        read_witness.entry.object_commitment, commitment,
        "Internal store read witness commitment does not match requested commitment",
    );
    assert!(
        verify_coordinate_index_membership(current_index_root, &read_witness.index_witness),
        "Internal store coordinate-index membership proof is invalid",
    );
    assert_eq!(
        read_witness.index_witness.coordinates, *coordinates,
        "Coordinate-index witness coordinates do not match internal input",
    );
    assert_eq!(
        read_witness.index_witness.value.object_commitment, commitment,
        "Coordinate-index witness commitment does not match internal input commitment",
    );
    assert_eq!(
        read_witness.index_witness.value.log_position, read_witness.log_witness.position,
        "Coordinate-index witness log position does not match append-log witness position",
    );
    assert_eq!(
        append_log_root_from_witness(&read_witness.entry, &read_witness.log_witness),
        current_log_root,
        "Append-log witness does not match current internal store root",
    );
}

fn verify_internal_store_write_witness(
    write_witness: &InternalStoreWriteWitness,
    current_index_root: &[u8],
    next_index_root: &[u8],
    expected_entry: &InternalStoreEntry,
    expected_log_position: u64,
) {
    assert_eq!(
        write_witness.entry, *expected_entry,
        "Internal store write witness entry does not match expected append entry",
    );
    assert_eq!(
        write_witness.index_non_membership_witness.coordinates,
        expected_entry.coordinates,
        "Coordinate-index non-membership proof coordinates do not match write entry",
    );
    assert!(
        verify_coordinate_index_non_membership(
            current_index_root,
            &write_witness.index_non_membership_witness,
        ),
        "Coordinate-index non-membership proof is invalid before write",
    );
    assert_eq!(
        write_witness.index_membership_witness.coordinates,
        expected_entry.coordinates,
        "Coordinate-index membership proof coordinates do not match write entry",
    );
    assert_eq!(
        write_witness.index_membership_witness.value.object_commitment,
        expected_entry.object_commitment,
        "Coordinate-index membership proof commitment does not match write entry",
    );
    assert_eq!(
        write_witness.index_membership_witness.value.log_position,
        expected_log_position,
        "Coordinate-index membership proof log position does not match append-log position",
    );
    assert_eq!(
        write_witness.index_non_membership_witness.siblings,
        write_witness.index_membership_witness.siblings,
        "Coordinate-index update proof siblings changed across insertion",
    );
    assert!(
        verify_coordinate_index_membership(next_index_root, &write_witness.index_membership_witness),
        "Coordinate-index membership proof is invalid after write",
    );
}

enum ResolvedSource<'a> {
    Inline(&'a Vec<u8>),
    External(&'a ExternalData),
    Internal(&'a InternalData),
}

fn resolved_source_at<'a>(input: &'a FnInput, index: usize) -> ResolvedSource<'a> {
    let arg = input
        .args()
        .get(index)
        .unwrap_or_else(|| panic!("Missing input arg metadata at index {}", index));
    let value = input
        .values()
        .get(index)
        .unwrap_or_else(|| panic!("Missing input source value at index {}", index));

    match value {
        FnInputValue::Inline(bytes) => ResolvedSource::Inline(bytes),
        FnInputValue::ExternalBinding => ResolvedSource::External(
            input
                .external()
                .get(&arg.name)
                .unwrap_or_else(|| panic!("Missing external input metadata for arg '{}'", arg.name)),
        ),
        FnInputValue::InternalBinding => ResolvedSource::Internal(
            input
                .internal()
                .get(&arg.name)
                .unwrap_or_else(|| panic!("Missing internal input metadata for arg '{}'", arg.name)),
        ),
    }
}

fn assert_same_source(left: ResolvedSource<'_>, right: ResolvedSource<'_>) {
    match (left, right) {
        (ResolvedSource::Inline(left_bytes), ResolvedSource::Inline(right_bytes)) => {
            assert_eq!(
                left_bytes, right_bytes,
                "Inline sequence scope input does not match consumer binding",
            );
        }
        (ResolvedSource::External(left_meta), ResolvedSource::External(right_meta)) => {
            assert_eq!(
                left_meta, right_meta,
                "External sequence scope input does not match consumer binding",
            );
        }
        (ResolvedSource::Internal(left_meta), ResolvedSource::Internal(right_meta)) => {
            assert_eq!(
                left_meta, right_meta,
                "Internal sequence scope input does not match consumer binding",
            );
        }
        _ => {
            panic!("Sequence scope source kind does not match consumer binding");
        }
    }
}

fn has_coordinate_prefix(coordinates: &CfsCoordinates, prefix: &CfsCoordinates) -> bool {
    coordinates.len() >= prefix.len()
        && coordinates
            .iter()
            .zip(prefix.iter())
            .all(|(coordinate, expected)| coordinate == expected)
}

fn verify_step_record_inputs(
    cfs_cursor: &CfsCursor,
    step_record: &StepRecord,
    input_source_witness: Option<&FnInput>,
    sequence_scope_witness: Option<&FnInput>,
) {
    // TODO: SequenceStart/SequenceEnd entrypoint case. In case of SequenceStart input is External Kind from
    // cli or from file. SequenceEnd just binding latest executed tile output.
    if step_record.coordinates().is_empty() {
        return;
    }

    if cfs_cursor
        .try_get_recur_iteration_coordinates(step_record.coordinates())
        .is_some()
    {
        return;
    }

    let cfs_item = cfs_cursor
        .try_get_item(step_record.coordinates())
        .unwrap_or_else(|| {
            panic!(
                "Failed to resolve cfs item for step record {:?}",
                step_record
            )
        });
    let step_inputs = cfs_item.inputs();

    let input_source_witness = input_source_witness.unwrap_or_else(|| {
        panic!(
            "Missing input source witness for step record {:?}",
            step_record
        )
    });
    assert_eq!(
        step_inputs.len(),
        input_source_witness.values().len(),
        "CFS input count does not match input source witness arity",
    );

    let Some((parent_sequence_coordinates, item_coordinate)) =
        step_record.coordinates().try_parent()
    else {
        return;
    };

    for (input_index, step_input) in step_inputs.iter().enumerate() {
        let resolved_source = resolved_source_at(input_source_witness, input_index);
        match step_input {
            InputBinding::Direct(InputSource::External) => {
                assert!(
                    matches!(resolved_source, ResolvedSource::External(_)),
                    "Expected external input source for step {:?} arg {}",
                    step_record,
                    input_index,
                );
            }
            InputBinding::Direct(InputSource::Inline) => {
                assert!(
                    matches!(resolved_source, ResolvedSource::Inline(_)),
                    "Expected inline input source for step {:?} arg {}",
                    step_record,
                    input_index,
                );
            }
            InputBinding::Direct(InputSource::Internal) => {
                assert!(
                    matches!(resolved_source, ResolvedSource::Internal(_)),
                    "Expected internal input source for step {:?} arg {}",
                    step_record,
                    input_index,
                );
            }
            InputBinding::SequenceScope { input_index } => {
                let sequence_scope_witness = sequence_scope_witness.unwrap_or_else(|| {
                    panic!(
                        "Missing sequence scope witness for step record {:?}",
                        step_record
                    )
                });
                let scope_source = resolved_source_at(sequence_scope_witness, *input_index);
                assert_same_source(resolved_source, scope_source);
            }
            InputBinding::ProducerOutput {
                item_index,
                output_index: _,
            } => {
                assert!(
                    *item_index < item_coordinate as usize,
                    "Step {:?} cannot depend on source item {} from the same or a future position {}",
                    step_record,
                    item_index,
                    item_coordinate
                );

                let mut source_coordinates = parent_sequence_coordinates.clone();
                source_coordinates.push(
                    (*item_index)
                        .try_into()
                        .expect("Producer item index exceeds CFS coordinate bounds"),
                );
                let internal_meta = match resolved_source {
                    ResolvedSource::Internal(meta) => meta,
                    _ => {
                        panic!(
                            "Expected internal input source for step {:?} arg {}",
                            step_record, input_index
                        )
                    }
                };
                match cfs_cursor
                    .try_get_item(&source_coordinates)
                    .expect("Expected producer item coordinates to resolve in CFS")
                {
                    raster_core::cfs::SequenceChildItem::Sequence(_)
                    | raster_core::cfs::SequenceChildItem::RecurSequence(_) => {
                        assert!(
                            has_coordinate_prefix(&internal_meta.coordinates, &source_coordinates),
                            "Internal input producer coordinates do not descend from expected sequence source",
                        );
                    }
                    raster_core::cfs::SequenceChildItem::Tile(_)
                    | raster_core::cfs::SequenceChildItem::RecurTile(_) => {
                        assert_eq!(
                            internal_meta.coordinates, source_coordinates,
                            "Internal input producer coordinates do not match expected CFS source",
                        );
                    }
                }
            }
        }
    }
}

fn verify_io_witness(
    step_record: &StepRecord,
    input_witness: Option<&Vec<u8>>,
    output_witness: Option<&Vec<u8>>,
) {
    let commitment_for = |bytes: Option<&Vec<u8>>| -> Vec<u8> {
        bytes.map(|bytes| sha256_bytes(bytes)).unwrap_or_default()
    };

    if let Some(input_commitment) = step_record.input_commitment() {
        assert_eq!(
            input_commitment,
            &commitment_for(input_witness),
            "Step input commitment does not match recorded input bytes",
        );
    }
    if let Some(output_commitment) = step_record.output_commitment() {
        if step_record.is_execution_step() {
            return;
        }
        assert_eq!(
            output_commitment,
            &commitment_for(output_witness),
            "Step output commitment does not match recorded output bytes",
        );
    }
}

fn verify_external_inputs(
    step: &StepRecord,
    external_input: &ExternalInput,
    external_selection_witnesses: &BTreeMap<String, raster_core::input::SelectionWitness>,
    external_inputs_commitments: &BTreeMap<String, Vec<u8>>,
) {
    let computed_commitment = external_input_commitment(external_input);

    if let Some(external_commitment) = step.external_input_commitment() {
        assert_eq!(
            external_commitment, &computed_commitment,
            "Step external input commitment does not match authorized inputs",
        );
    } else {
        assert!(
            external_input.is_empty(),
            "SequenceEnd must not carry external input metadata",
        );
    }

    for (binding_name, meta) in external_input {
        let authorized_commitment =
            external_inputs_commitments
                .get(&meta.name)
                .unwrap_or_else(|| {
                    panic!(
                        "Missing authorized commitment for external input '{}'",
                        meta.name
                    )
                });
        assert_eq!(
            authorized_commitment, &meta.commitment,
            "External input '{}' commitment does not match authorized source",
            meta.name,
        );
        assert_eq!(
            meta.tree_root, meta.selection.source_root_hash,
            "External input '{}' tree root does not match selection commitment root",
            meta.name,
        );
        if !meta.selector.is_empty() || meta.selection.selected_len > 0 {
            let witness = external_selection_witnesses
                .get(binding_name.as_str())
                .unwrap_or_else(|| {
                    panic!(
                        "Missing external selection witness for binding '{}'",
                        binding_name
                    )
                });
            assert!(
                verify_selection_witness(&meta.selection, witness),
                "External input '{}' selection witness is invalid",
                meta.name,
            );
        }
    }
}

fn verify_authorization_journal<'a>(
    authorization_journal: &'a AuthorizationJournal,
    authorization_image_id: &[u8],
) -> bool {
    let image_id_digest = risc0_zkvm::sha::Digest::try_from(authorization_image_id)
        .expect("authorization image id must be 32 bytes");

    let journal_bytes = risc0_zkvm::serde::to_vec(authorization_journal)
        .expect("Failed to serialize authorization journal");

    env::verify(image_id_digest, &journal_bytes).is_ok()
}

fn verify_step_record(
    step_record: &StepRecord,
    replay_image_id: Option<&Vec<u8>>,
    replay_journal: Option<&raster_core::draft::TileReplayJournal>,

    input_witness_bytes: Option<&Vec<u8>>,
    output_witness_bytes: Option<&Vec<u8>>,
    input_source_witness: Option<&FnInput>,

    external_inputs: &ExternalInput,
    external_selection_witnesses: &BTreeMap<String, raster_core::input::SelectionWitness>,
    external_inputs_commitments: &BTreeMap<String, Vec<u8>>,
) {
    verify_io_witness(step_record, input_witness_bytes, output_witness_bytes);
    if let Some(expected_input_source_commitment) = step_record.input_source_commitment() {
        let input_source_witness =
            input_source_witness.expect("Step input source witness is missing");
        assert_eq!(
            expected_input_source_commitment,
            &input_source_commitment(input_source_witness),
            "Step input source witness does not match recorded source commitment",
        );
    } else {
        assert!(
            input_source_witness.is_none(),
            "SequenceEnd must not carry input source witness",
        );
    }
    verify_external_inputs(
        step_record,
        external_inputs,
        external_selection_witnesses,
        external_inputs_commitments,
    );

    if step_record.requires_replay_proof() {
        let replay_image_id =
            replay_image_id.expect("replay image id should be provided for tile execution should ");
        let replay_journal =
            replay_journal.expect("tile execution should provide a replay journal witness");
        let replay_image_id_digest = risc0_zkvm::sha::Digest::try_from(replay_image_id.as_slice())
            .expect("image_id must be 32 bytes");
        let replay_journal_bytes = postcard::to_allocvec(replay_journal)
            .expect("Failed to encode replay journal for receipt verification");
        env::verify(replay_image_id_digest, &replay_journal_bytes)
            .expect("Failed to verify trace replay image id");
        let output_bytes = output_witness_bytes.map(Vec::as_slice).unwrap_or(&[]);
        assert_eq!(
            replay_journal.output_bytes.as_slice(),
            output_bytes,
            "Replay journal output bytes do not match recorded tile output witness",
        );
    }
}

fn verify_internal_store_transition(
    step_record: &StepRecord,
    input_source_witness: Option<&FnInput>,
    internal_selection_witnesses: &BTreeMap<String, raster_core::input::SelectionWitness>,
    _output_witness_bytes: Option<&Vec<u8>>,
    internal_store_witness: Option<&InternalStoreWitness>,
    current_frontier: &mut NonEmptyFrontier<Bytes>,
    current_index_root: &[u8],
) -> (SerializableFrontier, Vec<u8>, Vec<u8>) {
    if let Some((
        internal_store_root_before,
        internal_store_root_after,
        internal_store_index_root_before,
        internal_store_index_root_after,
    )) = step_record.internal_store_roots()
    {
        let current_root = frontier_root(current_frontier);
        assert_eq!(
            internal_store_root_before, &current_root,
            "Execution-step internal store root before does not match current internal store root",
        );
        assert_eq!(
            internal_store_index_root_before, &current_index_root,
            "Execution-step internal store index root before does not match current index root",
        );

        if let Some(input_source_witness) = input_source_witness {
            for (binding_name, internal_meta) in input_source_witness.internal() {
                let read_witness = internal_store_witness
                    .and_then(|witness| {
                        witness.reads.iter().find(|read| {
                            read.entry.coordinates == internal_meta.coordinates
                                && read.entry.object_commitment == internal_meta.commitment
                        })
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "Missing internal store read witness for coordinates {:?}",
                            internal_meta.coordinates
                        )
                    });
                verify_internal_store_read_witness(
                    read_witness,
                    &current_root,
                    current_index_root,
                    &internal_meta.coordinates,
                    &internal_meta.commitment,
                );
                assert_eq!(
                    internal_meta.commitment, internal_meta.selection.source_root_hash,
                    "Internal input '{}' commitment must match raster selection root",
                    binding_name,
                );
                if internal_meta.selection.selected_len > 0 {
                    let witness = internal_selection_witnesses
                        .get(binding_name.as_str())
                        .unwrap_or_else(|| {
                            panic!(
                                "Missing internal selection witness for binding '{}'",
                                binding_name
                            )
                        });
                    assert!(
                        verify_selection_witness(&internal_meta.selection, witness),
                        "Internal input '{}' selection witness is invalid",
                        binding_name,
                    );
                }
            }
        }

        let write_witness = internal_store_witness.and_then(|witness| witness.write.as_ref());

        match write_witness {
            Some(write_witness) => {
                let object_commitment = step_record
                    .output_commitment()
                    .expect("Execution step must expose output commitment")
                    .clone();

                let expected_entry = InternalStoreEntry {
                    coordinates: step_record.coordinates().clone(),
                    object_commitment,
                };
                current_frontier.append(Bytes(internal_store_leaf_hash(&expected_entry)));
                let next_root = frontier_root(current_frontier);
                let next_position: u64 = current_frontier.position().into();
                verify_internal_store_write_witness(
                    write_witness,
                    current_index_root,
                    internal_store_index_root_after,
                    &expected_entry,
                    next_position,
                );
                assert_eq!(
                    internal_store_root_after, &next_root,
                    "Execution-step internal store root after does not match appended internal store root",
                );

                (
                    serialize_frontier(current_frontier),
                    next_root,
                    internal_store_index_root_after.clone(),
                )
            }
            None => {
                assert_eq!(
                    internal_store_root_before, internal_store_root_after,
                    "Execution-step without internal store write must leave append-log root unchanged",
                );
                assert_eq!(
                    internal_store_index_root_before, internal_store_index_root_after,
                    "Execution-step without internal store write must leave index root unchanged",
                );
                (
                    serialize_frontier(current_frontier),
                    current_root,
                    current_index_root.to_vec(),
                )
            }
        }
    } else {
        assert!(
            internal_store_witness.is_none(),
            "Only execution steps may carry internal store witnesses",
        );
        (
            serialize_frontier(current_frontier),
            frontier_root(current_frontier),
            current_index_root.to_vec(),
        )
    }
}

fn verify_draft_transition(
    step_record: &StepRecord,
    replay_journal: Option<&TileReplayJournal>,
    draft_transition_witness: Option<&DraftTransitionWitness>,
    active_drafts: &mut BTreeMap<[u8; 32], TrackedDraftState>,
) {
    if !step_record.requires_replay_proof() {
        assert!(
            replay_journal.is_none(),
            "Only tile steps may carry replay journals",
        );
        assert!(
            draft_transition_witness.is_none(),
            "Only tile steps may carry draft transition witnesses",
        );
        return;
    }

    let Some(replay_journal) = replay_journal else {
        panic!("TileExec replay journal is missing");
    };
    let Some(DraftReplayTransition {
        draft_id,
        schema_hash,
        root_before,
        ops,
    }) = replay_journal.draft_transition.as_ref()
    else {
        assert!(
            draft_transition_witness.is_none(),
            "TileExec without replay-emitted draft transition must not carry draft witnesses",
        );
        return;
    };

    let witness = draft_transition_witness
        .expect("TileExec with replay-emitted draft transition must carry draft witness");
    let witness_schema_hash = compute_schema_hash(&witness.pre_state.schema);
    assert_eq!(
        &witness_schema_hash, schema_hash,
        "Draft witness schema hash does not match replay journal schema hash",
    );
    verify_witness_root(&witness.pre_state, root_before)
        .expect("Draft witness root must match replay journal root_before");

    if let Some(native_transition) = witness.native_transition.as_ref() {
        assert_eq!(
            native_transition.schema_hash, *schema_hash,
            "Native draft capture schema hash does not match replay journal",
        );
        assert_eq!(
            native_transition.root_before, *root_before,
            "Native draft capture root_before does not match replay journal",
        );
    }

    if let Some(tracked_state) = active_drafts.get(draft_id) {
        assert_eq!(
            tracked_state.schema_hash, *schema_hash,
            "Tracked draft schema hash changed within a draft chain",
        );
        assert_eq!(
            tracked_state.root, *root_before,
            "Replay journal root_before does not match tracked draft root",
        );
    }

    let (_, root_after) = apply_draft_ops(&witness.pre_state, ops)
        .expect("Draft replay ops must apply to authenticated pre-state witness");

    active_drafts.insert(
        *draft_id,
        TrackedDraftState {
            schema_hash: *schema_hash,
            root: root_after,
        },
    );
}

// Verify that current step record corrdinates are in preveious expected next coordinates and with
// CfsCursor iterate to next expected coordiantes
fn get_next_expected_coordinates(
    cfs_cursor: &CfsCursor,
    step: &StepRecord,
    current_expected_coordinates: Option<&Vec<CfsCoordinates>>,
) -> Vec<CfsCoordinates> {
    let coordinates = step.coordinates();
    if let Some(current_expected_coordinates) = current_expected_coordinates {
        assert!(
            current_expected_coordinates.contains(coordinates),
            "Step coordinates are not in expected next coordinates"
        );
    }

    cfs_cursor
        .try_get_next_coordinates(coordinates)
        .expect("Wrong tile coordinates")
}

fn next_frontier(
    frontier: &mut NonEmptyFrontier<Bytes>,
    step_record: &StepRecord,
    fingerprint_acc: &mut FingerprintAccumulator,
) -> SerializableFrontier {
    let item_hash = hash_trace_item(step_record);
    frontier.append(Bytes(item_hash));

    let tree = TraceBridgeTree::from_frontier(1, frontier.clone());
    let Some(tree_root) = tree.root(0) else {
        panic!("Can't get tree root");
    };
    fingerprint_acc.append(&tree_root.0);

    serialize_frontier(frontier)
}

// ============================================================================
// Main
// ============================================================================
fn main() {
    let cfs: ControlFlowSchema = env::read();
    let transition_image_id: Vec<u8> = env::read();

    let input: TransitionInput = env::read();
    let state: TransitionState = env::read();

    let cfs_cursor = CfsCursor::new(cfs);

    assert!(verify_authorization_journal(
        &input.authorization_journal,
        &input.authorization_image_id
    ));

    match state {
        TransitionState::Init(init_transition) => {
            let mut init_frontier = deserialize_frontier(&init_transition.init_frontier)
                .expect("Invalid frontier in input");
            let mut init_internal_store_frontier =
                deserialize_frontier(&init_transition.init_internal_store_frontier)
                    .expect("Invalid internal store frontier in input");
            assert_eq!(
                frontier_root(&init_internal_store_frontier),
                init_transition.init_internal_store_root,
                "Initial internal store root does not match initial internal store frontier",
            );

            verify_step_record_inputs(
                &cfs_cursor,
                &input.step_record,
                input.input_source_witness.as_ref(),
                input.sequence_scope_witness.as_ref(),
            );
            verify_step_record(
                &input.step_record,
                input.replay_image_id.as_ref(),
                input.replay_journal.as_ref(),
                input.input_witness.as_ref(),
                input.output_witness.as_ref(),
                input.input_source_witness.as_ref(),
                &input.external_input,
                &input.external_selection_witnesses,
                &input.authorization_journal.external_inputs_commitments,
            );
            let (
                new_internal_store_frontier,
                new_internal_store_root,
                new_internal_store_index_root,
            ) = verify_internal_store_transition(
                &input.step_record,
                input.input_source_witness.as_ref(),
                &input.internal_selection_witnesses,
                input.output_witness.as_ref(),
                input.internal_store_witness.as_ref(),
                &mut init_internal_store_frontier,
                &init_transition.init_internal_store_index_root,
            );
            let next_expected_coordinates =
                get_next_expected_coordinates(&cfs_cursor, &input.step_record, None);
            let mut active_drafts = init_transition.active_drafts.clone();
            verify_draft_transition(
                &input.step_record,
                input.replay_journal.as_ref(),
                input.draft_transition_witness.as_ref(),
                &mut active_drafts,
            );

            let mut actual_fingerprint_acc =
                FingerprintAccumulator::new(init_transition.fingerprint.bits_packer);
            let new_frontier = next_frontier(
                &mut init_frontier,
                &input.step_record,
                &mut actual_fingerprint_acc,
            );

            let current_state = TransitionState::Next(Transition {
                frontier: new_frontier,
                internal_store_frontier: new_internal_store_frontier,
                internal_store_root: new_internal_store_root,
                internal_store_index_root: new_internal_store_index_root,
                active_drafts,
                actual_fingerprint_acc,
                next_expected_coordinates,
            });

            let journal = TransitionJournal {
                init_state: init_transition,
                current_state,
                transition_image_id,
                authorization_image_id: input.authorization_image_id.clone(),
                manifest_commitment: input.authorization_journal.manifest_commitment.clone(),
            };

            env::commit(&journal);
        }
        TransitionState::Next(transition) => {
            let prev_journal: TransitionJournal = env::read();

            let transition_image_id_digest =
                risc0_zkvm::sha::Digest::try_from(transition_image_id.as_slice())
                    .expect("image_id must be 32 bytes");
            env::verify(
                transition_image_id_digest,
                &risc0_zkvm::serde::to_vec(&prev_journal).unwrap(),
            )
            .expect("Failed to verify previous transation journal");
            assert!(
                transition_image_id == prev_journal.transition_image_id,
                "The transition image ID is not the same within the fraud proof"
            );

            let TransitionState::Next(prev_transition) = prev_journal.current_state else {
                panic!("Provided Transition state does not allign to fraud proof state");
            };
            assert!(
                prev_transition == transition.clone(),
                "Transition mismatch: the provided transition does not align with the fraud proof"
            );

            assert!(
                input.authorization_journal.manifest_commitment == prev_journal.manifest_commitment,
                "Manifest commitment does not match"
            );

            let mut current_frontier =
                deserialize_frontier(&transition.frontier).expect("Invalid frontier in input");
            let mut current_internal_store_frontier =
                deserialize_frontier(&transition.internal_store_frontier)
                    .expect("Invalid internal store frontier in input");
            assert_eq!(
                frontier_root(&current_internal_store_frontier),
                transition.internal_store_root,
                "Transition internal store root does not match transition internal store frontier",
            );

            verify_step_record_inputs(
                &cfs_cursor,
                &input.step_record,
                input.input_source_witness.as_ref(),
                input.sequence_scope_witness.as_ref(),
            );
            verify_step_record(
                &input.step_record,
                input.replay_image_id.as_ref(),
                input.replay_journal.as_ref(),
                input.input_witness.as_ref(),
                input.output_witness.as_ref(),
                input.input_source_witness.as_ref(),
                &input.external_input,
                &input.external_selection_witnesses,
                &input.authorization_journal.external_inputs_commitments,
            );
            let (
                new_internal_store_frontier,
                new_internal_store_root,
                new_internal_store_index_root,
            ) = verify_internal_store_transition(
                &input.step_record,
                input.input_source_witness.as_ref(),
                &input.internal_selection_witnesses,
                input.output_witness.as_ref(),
                input.internal_store_witness.as_ref(),
                &mut current_internal_store_frontier,
                &transition.internal_store_index_root,
            );
            let next_expected_coordinates = get_next_expected_coordinates(
                &cfs_cursor,
                &input.step_record,
                Some(&transition.next_expected_coordinates),
            );
            let mut active_drafts = transition.active_drafts.clone();
            verify_draft_transition(
                &input.step_record,
                input.replay_journal.as_ref(),
                input.draft_transition_witness.as_ref(),
                &mut active_drafts,
            );

            let mut actual_fingerprint_acc = transition.actual_fingerprint_acc.clone();
            let new_frontier = next_frontier(
                &mut current_frontier,
                &input.step_record,
                &mut actual_fingerprint_acc,
            );

            let actual_fingerprint: Fingerprint = actual_fingerprint_acc.into_fingerprint();

            let current_state =
                if actual_fingerprint.len() == prev_journal.init_state.fingerprint.len() {
                    assert!(actual_fingerprint.diff_at_index(
                        actual_fingerprint.len() - 1,
                        &prev_journal.init_state.fingerprint
                    ));

                    TransitionState::Finished
                } else {
                    assert!(!actual_fingerprint.diff_at_index(
                        actual_fingerprint.len() - 1,
                        &prev_journal.init_state.fingerprint
                    ));

                    TransitionState::Next(Transition {
                        frontier: new_frontier,
                        internal_store_frontier: new_internal_store_frontier,
                        internal_store_root: new_internal_store_root,
                        internal_store_index_root: new_internal_store_index_root,
                        active_drafts,
                        actual_fingerprint_acc: FingerprintAccumulator::from(actual_fingerprint),
                        next_expected_coordinates,
                    })
                };
            let journal = TransitionJournal {
                init_state: prev_journal.init_state,
                current_state,
                transition_image_id,
                authorization_image_id: input.authorization_image_id.clone(),
                manifest_commitment: input.authorization_journal.manifest_commitment.clone(),
            };

            env::commit(&journal);
        }

        TransitionState::Finished => {
            panic!("Finished Transition");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::cfs::{
        CfsCoordinates, ControlFlowSchema, InputBinding, InputSource, SequenceChildItem, SequenceDef,
        SequenceItem, TileDef, TileItem,
    };
    use raster_core::coordinate_index::{
        coordinate_index_membership_proof, coordinate_index_non_membership_proof,
        coordinate_index_root,
    };
    use raster_core::draft::{
        draft_root_from_witness, DraftFieldValue, DraftOp, DraftReplayTransition, DraftStateWitness,
        DraftTransitionWitness, TileReplayJournal,
    };
    use raster_core::input::{SchemaField, SchemaFieldMode, SchemaNode, Selectable};
    use raster_core::trace::{
        ExternalData, FnInputArg, FnInputValue, InternalData, SequenceEndRecord,
        SequenceStartRecord, TileExecRecord,
    };

    fn sha(bytes: &[u8]) -> Vec<u8> {
        sha256_bytes(bytes)
    }

    struct DemoDraft;

    impl Selectable for DemoDraft {
        fn schema() -> SchemaNode {
            SchemaNode::Struct {
                type_name: "DemoDraft".into(),
                fields: vec![
                    SchemaField {
                        name: "title".into(),
                        label: "Title".into(),
                        mode: SchemaFieldMode::SetOnce,
                        schema: Box::new(SchemaNode::Leaf {
                            type_name: "String".into(),
                        }),
                    },
                    SchemaField {
                        name: "items".into(),
                        label: "Items".into(),
                        mode: SchemaFieldMode::AppendOnlyVec,
                        schema: Box::new(SchemaNode::List {
                            type_name: "Vec<String>".into(),
                            element: Box::new(SchemaNode::Leaf {
                                type_name: "String".into(),
                            }),
                        }),
                    },
                ],
            }
        }
    }

    fn draft_tile_step(exec_index: u64) -> StepRecord {
        StepRecord::TileExec(TileExecRecord {
            exec_index,
            tile_id: "collect_lines".into(),
            sequence_id: "main".into(),
            intra_sequence_index: exec_index as u32,
            coordinates: CfsCoordinates(vec![exec_index as u32]),
            input_commitment: vec![exec_index as u8; 32],
            input_source_commitment: vec![0; 32],
            output_commitment: vec![1; 32],
            external_input_commitment: Vec::new(),
            internal_store_root_before: EMPTY_LEAF.to_vec(),
            internal_store_root_after: EMPTY_LEAF.to_vec(),
            internal_store_index_root_before: Vec::new(),
            internal_store_index_root_after: Vec::new(),
        })
    }

    fn external_input(
        binding_name: &str,
        commitment: &[u8],
        selected_bytes: &[u8],
    ) -> ExternalInput {
        [(
            "arg".to_string(),
            ExternalData {
                name: binding_name.to_string(),
                commitment: commitment.to_vec(),
                tree_root: Vec::new(),
                selector: Default::default(),
                selection: raster_core::input::SelectionCommitment {
                    selected_hash: sha(selected_bytes),
                    selected_len: selected_bytes.len() as u64,
                    ..Default::default()
                },
            },
        )]
        .into_iter()
        .collect()
    }

    fn authorization_journal(binding_name: &str, commitment: &[u8]) -> AuthorizationJournal {
        AuthorizationJournal {
            external_inputs_commitments: [(binding_name.to_string(), commitment.to_vec())]
                .into_iter()
                .collect(),
            manifest_commitment: vec![7; 32],
        }
    }

    fn internal_input_witness(coordinates: CfsCoordinates, commitment: Vec<u8>) -> FnInput {
        FnInput {
            data: Vec::new(),
            values: vec![FnInputValue::InternalBinding],
            args: vec![FnInputArg {
                name: "arg".to_string(),
                ty: "Vec<u8>".to_string(),
            }],
            external: ExternalInput::new(),
            internal: [(
                "arg".to_string(),
                InternalData {
                    coordinates,
                    commitment,
                    selector: Default::default(),
                    selection: Default::default(),
                },
            )]
            .into_iter()
            .collect(),
        }
    }

    fn producer_sequence_cfs() -> CfsCursor {
        CfsCursor::new(ControlFlowSchema {
            version: "1.0".into(),
            project: "test".into(),
            encoding: "postcard".into(),
            tiles: vec![TileDef::iter("producer", 0, 1), TileDef::iter("consumer", 1, 1)],
            sequences: vec![
                SequenceDef {
                    id: "main".into(),
                    input_sources: vec![],
                    items: vec![
                        SequenceChildItem::Sequence(SequenceItem {
                            id: "sub".into(),
                            sources: vec![],
                        }),
                        SequenceChildItem::Tile(TileItem {
                            id: "consumer".into(),
                            sources: vec![InputBinding::ProducerOutput {
                                item_index: 0,
                                output_index: 0,
                            }],
                        }),
                    ],
                },
                SequenceDef {
                    id: "sub".into(),
                    input_sources: vec![],
                    items: vec![SequenceChildItem::Tile(TileItem {
                        id: "producer".into(),
                        sources: vec![InputBinding::Direct(InputSource::Inline)],
                    })],
                },
            ],
        })
    }

    #[test]
    fn verify_tile_commitments_accept_matching_recorded_io() {
        let ext = ExternalInput::new();
        let step = StepRecord::TileExec(TileExecRecord {
            exec_index: 1,
            tile_id: "tile".to_string(),
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![0]),
            intra_sequence_index: 0,
            input_commitment: sha(b"in"),
            input_source_commitment: Vec::new(),
            external_input_commitment: external_input_commitment(&ext),
            output_commitment: sha(b"out"),
            internal_store_root_before: vec![0; 32],
            internal_store_root_after: vec![0; 32],
            internal_store_index_root_before: Vec::new(),
            internal_store_index_root_after: Vec::new(),
        });

        verify_io_witness(&step, Some(&b"in".to_vec()), Some(&b"out".to_vec()));
        verify_external_inputs(
            &step,
            &ext,
            &BTreeMap::new(),
            &AuthorizationJournal {
                external_inputs_commitments: BTreeMap::new(),
                manifest_commitment: vec![0; 32],
            }
            .external_inputs_commitments,
        );
    }

    #[test]
    fn verify_step_record_inputs_accepts_sequence_descendant_producer_coordinates() {
        let cfs_cursor = producer_sequence_cfs();
        let step_record = StepRecord::TileExec(TileExecRecord {
            exec_index: 1,
            tile_id: "consumer".into(),
            sequence_id: "main".into(),
            coordinates: CfsCoordinates(vec![1]),
            intra_sequence_index: 1,
            input_commitment: Vec::new(),
            input_source_commitment: Vec::new(),
            output_commitment: Vec::new(),
            external_input_commitment: Vec::new(),
            internal_store_root_before: Vec::new(),
            internal_store_root_after: Vec::new(),
            internal_store_index_root_before: Vec::new(),
            internal_store_index_root_after: Vec::new(),
        });
        let input_source_witness =
            internal_input_witness(CfsCoordinates(vec![0, 0]), sha(b"producer-output"));

        verify_step_record_inputs(&cfs_cursor, &step_record, Some(&input_source_witness), None);
    }

    #[test]
    #[should_panic(expected = "Tile input commitment does not match recorded input bytes")]
    fn verify_tile_commitments_reject_mismatched_input() {
        let ext = ExternalInput::new();
        let step = StepRecord::TileExec(TileExecRecord {
            exec_index: 1,
            tile_id: "tile".to_string(),
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![0]),
            intra_sequence_index: 0,
            input_commitment: sha(b"expected"),
            input_source_commitment: Vec::new(),
            external_input_commitment: external_input_commitment(&ext),
            output_commitment: sha(b"out"),
            internal_store_root_before: vec![0; 32],
            internal_store_root_after: vec![0; 32],
            internal_store_index_root_before: Vec::new(),
            internal_store_index_root_after: Vec::new(),
        });

        verify_io_witness(&step, Some(&b"actual".to_vec()), Some(&b"out".to_vec()));
    }

    #[test]
    fn verify_sequence_boundary_commitments_accept_matching_recorded_io() {
        let ext = ExternalInput::new();
        let start = StepRecord::SequenceStart(SequenceStartRecord {
            exec_index: 1,
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![]),
            input_commitment: sha(b"sequence-in"),
            input_source_commitment: Vec::new(),
            external_input_commitment: external_input_commitment(&ext),
        });
        let end = StepRecord::SequenceEnd(SequenceEndRecord {
            exec_index: 2,
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![]),
            output_commitment: sha(b"sequence-out"),
        });

        verify_io_witness(&start, Some(&b"sequence-in".to_vec()), None);
        verify_io_witness(&end, None, Some(&b"sequence-out".to_vec()));
        verify_external_inputs(
            &start,
            &ext,
            &BTreeMap::new(),
            &AuthorizationJournal {
                external_inputs_commitments: BTreeMap::new(),
                manifest_commitment: vec![0; 32],
            }
            .external_inputs_commitments,
        );
        verify_external_inputs(
            &end,
            &ExternalInput::new(),
            &BTreeMap::new(),
            &AuthorizationJournal {
                external_inputs_commitments: BTreeMap::new(),
                manifest_commitment: vec![0; 32],
            }
            .external_inputs_commitments,
        );
    }

    #[test]
    fn verify_external_inputs_accept_matching_authorized_binding() {
        let ext = external_input("personal_data", sha256_hex(b"payload").as_slice(), b"");
        let step = StepRecord::TileExec(TileExecRecord {
            exec_index: 1,
            tile_id: "tile".to_string(),
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![0]),
            intra_sequence_index: 0,
            input_commitment: sha(b"in"),
            input_source_commitment: Vec::new(),
            external_input_commitment: external_input_commitment(&ext),
            output_commitment: sha(b"out"),
            internal_store_root_before: vec![0; 32],
            internal_store_root_after: vec![0; 32],
            internal_store_index_root_before: Vec::new(),
            internal_store_index_root_after: Vec::new(),
        });

        let authorization =
            authorization_journal("personal_data", sha256_hex(b"payload").as_slice());
        verify_external_inputs(
            &step,
            &ext,
            &BTreeMap::new(),
            &authorization.external_inputs_commitments,
        );
    }

    #[test]
    #[should_panic(expected = "Missing authorized commitment for external input 'personal_data'")]
    fn verify_external_inputs_reject_missing_authorized_binding() {
        let ext = external_input("personal_data", sha256_hex(b"payload").as_slice(), b"");
        let step = StepRecord::TileExec(TileExecRecord {
            exec_index: 1,
            tile_id: "tile".to_string(),
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![0]),
            intra_sequence_index: 0,
            input_commitment: sha(b"in"),
            input_source_commitment: Vec::new(),
            external_input_commitment: external_input_commitment(&ext),
            output_commitment: sha(b"out"),
            internal_store_root_before: vec![0; 32],
            internal_store_root_after: vec![0; 32],
            internal_store_index_root_before: Vec::new(),
            internal_store_index_root_after: Vec::new(),
        });

        verify_external_inputs(
            &step,
            &ext,
            &BTreeMap::new(),
            &AuthorizationJournal {
                external_inputs_commitments: BTreeMap::new(),
                manifest_commitment: vec![0; 32],
            }
            .external_inputs_commitments,
        );
    }

    #[test]
    #[should_panic(
        expected = "External input 'personal_data' commitment does not match authorized source"
    )]
    fn verify_external_inputs_reject_mismatched_authorized_binding() {
        let ext = external_input("personal_data", sha256_hex(b"payload").as_slice(), b"");
        let step = StepRecord::TileExec(TileExecRecord {
            exec_index: 1,
            tile_id: "tile".to_string(),
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![0]),
            intra_sequence_index: 0,
            input_commitment: sha(b"in"),
            input_source_commitment: Vec::new(),
            external_input_commitment: external_input_commitment(&ext),
            output_commitment: sha(b"out"),
            internal_store_root_before: vec![0; 32],
            internal_store_root_after: vec![0; 32],
            internal_store_index_root_before: Vec::new(),
            internal_store_index_root_after: Vec::new(),
        });

        let authorization = authorization_journal("personal_data", b"wrong");
        verify_external_inputs(
            &step,
            &ext,
            &BTreeMap::new(),
            &authorization.external_inputs_commitments,
        );
    }

    fn empty_internal_store_frontier_for_test() -> NonEmptyFrontier<Bytes> {
        deserialize_frontier(&SerializableFrontier {
            position: 0,
            leaf: EMPTY_LEAF.to_vec(),
            ommers: Vec::new(),
        })
        .expect("empty internal store frontier should deserialize")
    }

    fn build_internal_store_context(
        entries: &[InternalStoreEntry],
    ) -> (
        NonEmptyFrontier<Bytes>,
        Vec<u8>,
        BTreeMap<CfsCoordinates, raster_core::transition::InternalStoreIndexValue>,
        Vec<u8>,
    ) {
        let mut frontier = empty_internal_store_frontier_for_test();
        let mut index = BTreeMap::new();
        for entry in entries {
            frontier.append(Bytes(internal_store_leaf_hash(entry)));
            let log_position: u64 = frontier.position().into();
            index.insert(
                entry.coordinates.clone(),
                raster_core::transition::InternalStoreIndexValue {
                    log_position,
                    object_commitment: entry.object_commitment.clone(),
                },
            );
        }
        let root = frontier_root(&frontier);
        let index_root = coordinate_index_root(&index);
        (frontier, root, index, index_root)
    }

    fn build_internal_store_log_witness_for_entries(
        entries: &[InternalStoreEntry],
        log_position: u64,
    ) -> InternalStoreLogWitness {
        let mut tree = TraceBridgeTree::new(1);
        tree.append(Bytes(EMPTY_LEAF.to_vec()));
        let mut marked_position = None;
        for (index, entry) in entries.iter().enumerate() {
            tree.append(Bytes(internal_store_leaf_hash(entry)));
            if u64::try_from(index).expect("index overflow") + 1 == log_position {
                marked_position = tree.mark();
            }
        }
        let marked_position = marked_position.expect("log position should exist in append tree");
        let auth_path = tree
            .witness(marked_position, 0)
            .expect("append-log witness should exist");
        InternalStoreLogWitness {
            position: u64::from(marked_position),
            path_elems: auth_path.iter().map(|elem| elem.0.clone()).collect(),
        }
    }

    fn build_read_witness(
        entries: &[InternalStoreEntry],
        entry: &InternalStoreEntry,
    ) -> InternalStoreReadWitness {
        let (_frontier, _root, index, _index_root) = build_internal_store_context(entries);
        let index_witness = coordinate_index_membership_proof(&index, &entry.coordinates)
            .expect("membership proof should exist");
        let log_witness = build_internal_store_log_witness_for_entries(
            entries,
            index_witness.value.log_position,
        );
        InternalStoreReadWitness {
            entry: entry.clone(),
            log_witness,
            index_witness,
        }
    }

    fn build_write_witness(
        before_entries: &[InternalStoreEntry],
        new_entry: &InternalStoreEntry,
    ) -> InternalStoreWriteWitness {
        let (_frontier, _root, before_index, _before_index_root) =
            build_internal_store_context(before_entries);
        let mut after_entries = before_entries.to_vec();
        after_entries.push(new_entry.clone());
        let (_frontier, _root, after_index, _after_index_root) =
            build_internal_store_context(&after_entries);
        InternalStoreWriteWitness {
            entry: new_entry.clone(),
            index_non_membership_witness: coordinate_index_non_membership_proof(
                &before_index,
                &new_entry.coordinates,
            ),
            index_membership_witness: coordinate_index_membership_proof(
                &after_index,
                &new_entry.coordinates,
            )
            .expect("membership proof should exist after write"),
        }
    }

    fn tile_step_with_store_roots(
        exec_index: u64,
        coordinates: CfsCoordinates,
        input_source_commitment: Vec<u8>,
        output_commitment: Vec<u8>,
        root_before: Vec<u8>,
        root_after: Vec<u8>,
        index_root_before: Vec<u8>,
        index_root_after: Vec<u8>,
    ) -> StepRecord {
        StepRecord::TileExec(TileExecRecord {
            exec_index,
            tile_id: "tile".to_string(),
            sequence_id: "main".to_string(),
            coordinates,
            intra_sequence_index: 0,
            input_commitment: Vec::new(),
            input_source_commitment,
            external_input_commitment: Vec::new(),
            output_commitment,
            internal_store_root_before: root_before,
            internal_store_root_after: root_after,
            internal_store_index_root_before: index_root_before,
            internal_store_index_root_after: index_root_after,
        })
    }

    #[test]
    fn verify_internal_store_transition_uses_output_commitment_as_keyed_entry() {
        let output_commitment = sha(b"out");
        let new_entry = InternalStoreEntry {
            coordinates: CfsCoordinates(vec![0]),
            object_commitment: output_commitment.clone(),
        };
        let (mut before_frontier, root_before, _before_index, index_root_before) =
            build_internal_store_context(&[]);
        let (_after_frontier, root_after, _after_index, index_root_after) =
            build_internal_store_context(&[new_entry.clone()]);
        let step = tile_step_with_store_roots(
            1,
            new_entry.coordinates.clone(),
            Vec::new(),
            output_commitment,
            root_before.clone(),
            root_after.clone(),
            index_root_before.clone(),
            index_root_after.clone(),
        );
        let witness = InternalStoreWitness {
            reads: Vec::new(),
            write: Some(build_write_witness(&[], &new_entry)),
        };

        let (_next_frontier, next_root, next_index_root) = verify_internal_store_transition(
            &step,
            None,
            &BTreeMap::new(),
            Some(&b"out".to_vec()),
            Some(&witness),
            &mut before_frontier,
            &index_root_before,
        );

        assert_eq!(next_root, root_after);
        assert_eq!(next_index_root, index_root_after);
    }

    #[test]
    #[should_panic(expected = "Coordinate-index non-membership proof is invalid before write")]
    fn verify_internal_store_transition_rejects_duplicate_coordinates() {
        let existing_entry = InternalStoreEntry {
            coordinates: CfsCoordinates(vec![0]),
            object_commitment: sha(b"existing"),
        };
        let (mut before_frontier, root_before, before_index, index_root_before) =
            build_internal_store_context(&[existing_entry.clone()]);
        let step = tile_step_with_store_roots(
            1,
            existing_entry.coordinates.clone(),
            Vec::new(),
            sha(b"out"),
            root_before.clone(),
            root_before,
            index_root_before.clone(),
            index_root_before.clone(),
        );
        let witness = InternalStoreWitness {
            reads: Vec::new(),
            write: Some(InternalStoreWriteWitness {
                entry: InternalStoreEntry {
                    coordinates: existing_entry.coordinates.clone(),
                    object_commitment: sha(b"out"),
                },
                index_non_membership_witness: raster_core::transition::CoordinateIndexNonMembershipProof {
                    coordinates: existing_entry.coordinates.clone(),
                    siblings: vec![vec![0; 32]; 256],
                },
                index_membership_witness: coordinate_index_membership_proof(
                    &before_index,
                    &existing_entry.coordinates,
                )
                .expect("existing coordinate should have membership proof"),
            }),
        };

        let _ = verify_internal_store_transition(
            &step,
            None,
            &BTreeMap::new(),
            Some(&b"out".to_vec()),
            Some(&witness),
            &mut before_frontier,
            &index_root_before,
        );
    }

    #[test]
    #[should_panic(expected = "Missing internal store read witness for coordinates CfsCoordinates([0])")]
    fn verify_internal_store_transition_rejects_wrong_coordinates_with_correct_bytes() {
        let prior_entry = InternalStoreEntry {
            coordinates: CfsCoordinates(vec![9]),
            object_commitment: sha(b"shared"),
        };
        let new_entry = InternalStoreEntry {
            coordinates: CfsCoordinates(vec![1]),
            object_commitment: sha(b"out"),
        };
        let (mut before_frontier, root_before, _before_index, index_root_before) =
            build_internal_store_context(&[prior_entry.clone()]);
        let (_after_frontier, root_after, _after_index, index_root_after) =
            build_internal_store_context(&[prior_entry.clone(), new_entry.clone()]);
        let input_source_witness =
            internal_input_witness(CfsCoordinates(vec![0]), prior_entry.object_commitment.clone());
        let step = tile_step_with_store_roots(
            2,
            new_entry.coordinates.clone(),
            input_source_commitment(&input_source_witness),
            new_entry.object_commitment.clone(),
            root_before.clone(),
            root_after,
            index_root_before.clone(),
            index_root_after,
        );
        let witness = InternalStoreWitness {
            reads: vec![build_read_witness(&[prior_entry.clone()], &prior_entry)],
            write: Some(build_write_witness(&[prior_entry], &new_entry)),
        };

        let _ = verify_internal_store_transition(
            &step,
            Some(&input_source_witness),
            &BTreeMap::new(),
            Some(&b"out".to_vec()),
            Some(&witness),
            &mut before_frontier,
            &index_root_before,
        );
    }

    #[test]
    #[should_panic(expected = "TileExec internal store root before does not match current internal store root")]
    fn verify_internal_store_transition_rejects_stale_root() {
        let new_entry = InternalStoreEntry {
            coordinates: CfsCoordinates(vec![0]),
            object_commitment: sha(b"out"),
        };
        let (_before_frontier, root_before, _before_index, index_root_before) =
            build_internal_store_context(&[]);
        let (_after_frontier, root_after, _after_index, index_root_after) =
            build_internal_store_context(&[new_entry.clone()]);
        let step = tile_step_with_store_roots(
            3,
            new_entry.coordinates.clone(),
            Vec::new(),
            new_entry.object_commitment.clone(),
            root_before,
            root_after,
            index_root_before.clone(),
            index_root_after,
        );
        let stale_entry = InternalStoreEntry {
            coordinates: CfsCoordinates(vec![99]),
            object_commitment: sha(b"stale"),
        };
        let (mut stale_frontier, _stale_root, _stale_index, _stale_index_root) =
            build_internal_store_context(&[stale_entry]);
        let witness = InternalStoreWitness {
            reads: Vec::new(),
            write: Some(build_write_witness(&[], &new_entry)),
        };

        let _ = verify_internal_store_transition(
            &step,
            None,
            &BTreeMap::new(),
            Some(&b"out".to_vec()),
            Some(&witness),
            &mut stale_frontier,
            &index_root_before,
        );
    }

    #[test]
    #[should_panic(expected = "TileExec internal store index root before does not match current index root")]
    fn verify_internal_store_transition_rejects_stale_index_root() {
        let new_entry = InternalStoreEntry {
            coordinates: CfsCoordinates(vec![0]),
            object_commitment: sha(b"out"),
        };
        let (mut before_frontier, root_before, _before_index, index_root_before) =
            build_internal_store_context(&[]);
        let (_after_frontier, root_after, _after_index, index_root_after) =
            build_internal_store_context(&[new_entry.clone()]);
        let step = tile_step_with_store_roots(
            33,
            new_entry.coordinates.clone(),
            Vec::new(),
            new_entry.object_commitment.clone(),
            root_before,
            root_after,
            index_root_before.clone(),
            index_root_after,
        );
        let witness = InternalStoreWitness {
            reads: Vec::new(),
            write: Some(build_write_witness(&[], &new_entry)),
        };

        let _ = verify_internal_store_transition(
            &step,
            None,
            &BTreeMap::new(),
            Some(&b"out".to_vec()),
            Some(&witness),
            &mut before_frontier,
            &[9; 32],
        );
    }

    #[test]
    fn verify_internal_store_transition_accepts_non_empty_initial_state() {
        let prior_entry = InternalStoreEntry {
            coordinates: CfsCoordinates(vec![0]),
            object_commitment: sha(b"prior"),
        };
        let new_entry = InternalStoreEntry {
            coordinates: CfsCoordinates(vec![1]),
            object_commitment: sha(b"next"),
        };
        let (mut before_frontier, root_before, _before_index, index_root_before) =
            build_internal_store_context(&[prior_entry.clone()]);
        let (_after_frontier, root_after, _after_index, index_root_after) =
            build_internal_store_context(&[prior_entry.clone(), new_entry.clone()]);
        let input_source_witness = internal_input_witness(
            prior_entry.coordinates.clone(),
            prior_entry.object_commitment.clone(),
        );
        let step = tile_step_with_store_roots(
            4,
            new_entry.coordinates.clone(),
            input_source_commitment(&input_source_witness),
            new_entry.object_commitment.clone(),
            root_before.clone(),
            root_after.clone(),
            index_root_before.clone(),
            index_root_after.clone(),
        );
        let witness = InternalStoreWitness {
            reads: vec![build_read_witness(&[prior_entry.clone()], &prior_entry)],
            write: Some(build_write_witness(&[prior_entry], &new_entry)),
        };

        let (_next_frontier, next_root, next_index_root) = verify_internal_store_transition(
            &step,
            Some(&input_source_witness),
            &BTreeMap::new(),
            Some(&b"next".to_vec()),
            Some(&witness),
            &mut before_frontier,
            &index_root_before,
        );

        assert_eq!(next_root, root_after);
        assert_eq!(next_index_root, index_root_after);
    }

    #[test]
    fn verify_draft_transition_tracks_multi_step_chain() {
        let empty_witness = DraftStateWitness {
            schema: DemoDraft::schema(),
            fields: Vec::new(),
        };
        let empty_root = draft_root_from_witness(&empty_witness.schema, &BTreeMap::new(), false).unwrap();
        let schema_hash = compute_schema_hash(&empty_witness.schema);
        let draft_id = [7; 32];
        let mut active_drafts = BTreeMap::new();

        let step_one = TileReplayJournal {
            output_bytes: Vec::new(),
            draft_transition: Some(DraftReplayTransition {
                draft_id,
                schema_hash,
                root_before: empty_root,
                ops: vec![
                    DraftOp::Set {
                        field: "title".into(),
                        value: raster_core::draft::DraftValue::String("collected".into()),
                    },
                    DraftOp::Push {
                        field: "items".into(),
                        value: raster_core::draft::DraftValue::String("first".into()),
                    },
                ],
            }),
        };
        verify_draft_transition(
            &draft_tile_step(1),
            Some(&step_one),
            Some(&DraftTransitionWitness {
                pre_state: empty_witness.clone(),
                native_transition: step_one.draft_transition.clone(),
            }),
            &mut active_drafts,
        );
        let step_one_root = active_drafts.get(&draft_id).unwrap().root;

        let step_two_witness = DraftStateWitness {
            schema: empty_witness.schema.clone(),
            fields: vec![
                (
                    "title".into(),
                    DraftFieldValue::Set(raster_core::draft::DraftValue::String("collected".into())),
                ),
                (
                    "items".into(),
                    DraftFieldValue::Append(vec![raster_core::draft::DraftValue::String("first".into())]),
                ),
            ],
        };
        let step_two = TileReplayJournal {
            output_bytes: Vec::new(),
            draft_transition: Some(DraftReplayTransition {
                draft_id,
                schema_hash,
                root_before: step_one_root,
                ops: vec![DraftOp::Push {
                    field: "items".into(),
                    value: raster_core::draft::DraftValue::String("second".into()),
                }],
            }),
        };
        verify_draft_transition(
            &draft_tile_step(2),
            Some(&step_two),
            Some(&DraftTransitionWitness {
                pre_state: step_two_witness,
                native_transition: step_two.draft_transition.clone(),
            }),
            &mut active_drafts,
        );

        assert_ne!(active_drafts.get(&draft_id).unwrap().root, step_one_root);
    }

    #[test]
    #[should_panic(expected = "root_before does not match tracked draft root")]
    fn verify_draft_transition_rejects_wrong_root_before() {
        let witness = DraftStateWitness {
            schema: DemoDraft::schema(),
            fields: Vec::new(),
        };
        let empty_root = draft_root_from_witness(&witness.schema, &BTreeMap::new(), false).unwrap();
        let schema_hash = compute_schema_hash(&witness.schema);
        let draft_id = [9; 32];
        let mut active_drafts = BTreeMap::from([(
            draft_id,
            TrackedDraftState {
                schema_hash,
                root: [1; 32],
            },
        )]);

        verify_draft_transition(
            &draft_tile_step(1),
            Some(&TileReplayJournal {
                output_bytes: Vec::new(),
                draft_transition: Some(DraftReplayTransition {
                    draft_id,
                    schema_hash,
                    root_before: empty_root,
                    ops: Vec::new(),
                }),
            }),
            Some(&DraftTransitionWitness {
                pre_state: witness,
                native_transition: None,
            }),
            &mut active_drafts,
        );
    }

    #[test]
    #[should_panic(expected = "schema hash")]
    fn verify_draft_transition_rejects_wrong_schema_hash() {
        let witness = DraftStateWitness {
            schema: DemoDraft::schema(),
            fields: Vec::new(),
        };
        let empty_root = draft_root_from_witness(&witness.schema, &BTreeMap::new(), false).unwrap();
        let mut active_drafts = BTreeMap::new();

        verify_draft_transition(
            &draft_tile_step(1),
            Some(&TileReplayJournal {
                output_bytes: Vec::new(),
                draft_transition: Some(DraftReplayTransition {
                    draft_id: [3; 32],
                    schema_hash: [4; 32],
                    root_before: empty_root,
                    ops: Vec::new(),
                }),
            }),
            Some(&DraftTransitionWitness {
                pre_state: witness,
                native_transition: None,
            }),
            &mut active_drafts,
        );
    }

    #[test]
    #[should_panic(expected = "witness root")]
    fn verify_draft_transition_rejects_tampered_pre_state_witness() {
        let empty_root =
            draft_root_from_witness(&DemoDraft::schema(), &BTreeMap::new(), false).unwrap();
        let mut active_drafts = BTreeMap::new();

        verify_draft_transition(
            &draft_tile_step(1),
            Some(&TileReplayJournal {
                output_bytes: Vec::new(),
                draft_transition: Some(DraftReplayTransition {
                    draft_id: [6; 32],
                    schema_hash: compute_schema_hash(&DemoDraft::schema()),
                    root_before: empty_root,
                    ops: Vec::new(),
                }),
            }),
            Some(&DraftTransitionWitness {
                pre_state: DraftStateWitness {
                    schema: DemoDraft::schema(),
                    fields: vec![(
                        "title".into(),
                        DraftFieldValue::Set(raster_core::draft::DraftValue::String("tampered".into())),
                    )],
                },
                native_transition: None,
            }),
            &mut active_drafts,
        );
    }
}
