//! RISC0 guest program for trace state transitions.
//!
//! This guest performs a single state transition of the bridge tree by:
//! 1. Taking a serialized frontier + trace item data as input
//! 2. Hashing the trace item and appending it to the frontier
//! 3. Returning the new frontier

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

use bridgetree::{Hashable, Level, NonEmptyFrontier, Position};
use risc0_zkvm::guest::env;
use risc0_zkvm::sha::{Impl as Risc0Sha256, Sha256 as _};

use raster_core::authorization::AuthorizationJournal;
use raster_core::cfs::{
    CfsCoordinates, CfsCursor, ControlFlowSchema, InputSource, SequenceChildItem,
};
use raster_core::fingerprint::{Fingerprint, FingerprintAccumulator};
use raster_core::input::verify_selection_proof;
use raster_core::trace::{ExternalInput, StepRecord};
use raster_core::transition::{
    SerializableFrontier, StepRecordWitness, Transition, TransitionInput, TransitionJournal,
    TransitionState,
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

fn decode_step_record_witness(bytes: &[u8]) -> StepRecordWitness {
    let witness: StepRecordWitness =
        postcard::from_bytes(bytes).expect("Failed to deserialize step record witness");

    witness
}

fn verify_record_witness(record: &StepRecord, witness_bytes: &[u8], current_root: &Bytes) {
    let witness = decode_step_record_witness(witness_bytes);
    let witnessed_root = witness.path_elems.into_iter().zip(0u8..).fold(
        Bytes(hash_trace_item(record)),
        |root, (path_elem, level)| {
            let sibling = Bytes(path_elem);
            if (witness.position >> level) & 0x1 == 0 {
                Bytes::combine(level.into(), &root, &sibling)
            } else {
                Bytes::combine(level.into(), &sibling, &root)
            }
        },
    );

    assert!(
        &witnessed_root == current_root,
        "Step record witness does not match current frontier root"
    );
}

fn verify_step_record_inputs(
    cfs_cursor: &CfsCursor,
    frontier: &NonEmptyFrontier<Bytes>,
    step_record: &StepRecord,
    input_sources_witnesses: &HashMap<StepRecord, Vec<u8>>,
) {
    // TODO: SequenceStart/SequenceEnd entrypoint case. In case of SequenceStart input is External Kind from
    // cli or from file. SequenceEnd just binding latest executed tile output.
    if step_record.coordinates().is_empty() {
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

    // TODO: External Kind of input source not_implemented
    if step_inputs
        .iter()
        .all(|input| matches!(input.source, InputSource::External))
    {
        return;
    }

    let trace_tree = TraceBridgeTree::from_frontier(1, frontier.clone());
    let current_root = trace_tree.root(0).expect("Can't get current frontier root");

    // TODO: SequenceEnd/SequenceStart case
    let Some((parent_sequence_coordinates, item_coordinate)) =
        step_record.coordinates().try_parent()
    else {
        return;
    };

    for step_input in step_inputs {
        match step_input.source {
            InputSource::External => {
                todo!("External input source")
            }
            InputSource::SeqInput { .. } => {
                let (source_record, witness_bytes) = input_sources_witnesses
                    .iter()
                    .find(|(record, _)| {
                        matches!(
                            record,
                            StepRecord::SequenceStart(sequence_start_record)
                                if sequence_start_record.coordinates == parent_sequence_coordinates
                        )
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "Missing sequence input witness for step record {:?}",
                            step_record
                        )
                    });

                verify_record_witness(source_record, witness_bytes, &current_root);
            }
            InputSource::ItemOutput {
                item_index,
                output_index: _,
            } => {
                assert!(
                    item_index < item_coordinate as usize,
                    "Step {:?} cannot depend on source item {} from the same or a future position {}",
                    step_record,
                    item_index,
                    item_coordinate
                );

                let mut source_coordinates = parent_sequence_coordinates.clone();
                source_coordinates.push(
                    item_index
                        .try_into()
                        .expect("Producer item index exceeds CFS coordinate bounds"),
                );

                let source_cfs_item =
                    cfs_cursor
                        .try_get_item(&source_coordinates)
                        .unwrap_or_else(|| {
                            panic!(
                                "Failed to resolve producer item {} for step {:?}",
                                item_index, step_record
                            )
                        });

                let (source_record, witness_bytes) = input_sources_witnesses
                    .iter()
                    .find(|(record, _)| match (record, source_cfs_item) {
                        (StepRecord::TileExec(tile_exec_record), SequenceChildItem::Tile(_)) => {
                            tile_exec_record.coordinates == source_coordinates
                        }
                        (
                            StepRecord::SequenceEnd(sequence_end_record),
                            SequenceChildItem::Sequence(_),
                        ) => sequence_end_record.coordinates == source_coordinates,
                        _ => false,
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "Missing item-output witness for step record {:?}",
                            step_record
                        )
                    });

                verify_record_witness(source_record, witness_bytes, &current_root);
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

    match step_record {
        StepRecord::TileExec(record) => {
            assert_eq!(
                record.input_commitment,
                commitment_for(input_witness),
                "Tile input commitment does not match recorded input bytes",
            );
            assert_eq!(
                record.output_commitment,
                commitment_for(output_witness),
                "Tile output commitment does not match recorded output bytes",
            );
        }
        StepRecord::SequenceStart(record) => {
            assert_eq!(
                record.input_commitment,
                commitment_for(input_witness),
                "Sequence start input commitment does not match recorded input bytes",
            );
        }
        StepRecord::SequenceEnd(record) => {
            assert_eq!(
                record.output_commitment,
                commitment_for(output_witness),
                "Sequence end output commitment does not match recorded output bytes",
            );
        }
    }
}

fn verify_external_inputs(
    step: &StepRecord,
    external_input: &ExternalInput,
    external_inputs_commitments: &BTreeMap<String, Vec<u8>>,
) {
    let computed_commitment = external_input_commitment(external_input);

    match step {
        StepRecord::TileExec(record) => {
            assert_eq!(
                record.external_input_commitment, computed_commitment,
                "TileExec external input not authorized",
            );
        }
        StepRecord::SequenceStart(record) => {
            assert_eq!(
                record.external_input_commitment, computed_commitment,
                "SequenceStart external input not authorized",
            );
        }
        StepRecord::SequenceEnd(_) => {
            assert!(
                external_input.is_empty(),
                "SequenceEnd must not carry external input metadata",
            );
        }
    }

    for meta in external_input.values() {
        let authorized_commitment = external_inputs_commitments
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
        if !meta.selector.is_empty() || !meta.selected.bytes.is_empty() {
            assert!(
                !meta.selected.bytes.is_empty(),
                "External input '{}' selector is missing selected payload bytes",
                meta.name,
            );
            assert!(
                verify_selection_proof(&meta.selected.bytes, &meta.selected.proof),
                "External input '{}' selection proof is invalid",
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

    input_witness_bytes: Option<&Vec<u8>>,
    output_witness_bytes: Option<&Vec<u8>>,

    external_inputs: &ExternalInput,
    external_inputs_commitments: &BTreeMap<String, Vec<u8>>,
) {
    verify_io_witness(step_record, input_witness_bytes, output_witness_bytes);
    verify_external_inputs(step_record, external_inputs, external_inputs_commitments);

    if let StepRecord::TileExec(_) = step_record {
        let replay_image_id =
            replay_image_id.expect("replay image id should be provided for tile execution should ");
        let replay_image_id_digest = risc0_zkvm::sha::Digest::try_from(replay_image_id.as_slice())
            .expect("image_id must be 32 bytes");
        let output_bytes = output_witness_bytes.map(Vec::as_slice).unwrap_or(&[]);

        env::verify(replay_image_id_digest, output_bytes)
            .expect("Failed to verify trace replay image id");
    }
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

            verify_step_record_inputs(
                &cfs_cursor,
                &init_frontier,
                &input.step_record,
                &input.input_sources_witnesses,
            );
            verify_step_record(
                &input.step_record,
                input.replay_image_id.as_ref(),
                input.input_witness.as_ref(),
                input.output_witness.as_ref(),
                &input.external_input,
                &input.authorization_journal.external_inputs_commitments,
            );
            let next_expected_coordinates =
                get_next_expected_coordinates(&cfs_cursor, &input.step_record, None);

            let mut actual_fingerprint_acc =
                FingerprintAccumulator::new(init_transition.fingerprint.bits_packer);
            let new_frontier = next_frontier(
                &mut init_frontier,
                &input.step_record,
                &mut actual_fingerprint_acc,
            );

            let current_state = TransitionState::Next(Transition {
                frontier: new_frontier,
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

            verify_step_record_inputs(
                &cfs_cursor,
                &current_frontier,
                &input.step_record,
                &input.input_sources_witnesses,
            );
            verify_step_record(
                &input.step_record,
                input.replay_image_id.as_ref(),
                input.input_witness.as_ref(),
                input.output_witness.as_ref(),
                &input.external_input,
                &input.authorization_journal.external_inputs_commitments,
            );
            let next_expected_coordinates = get_next_expected_coordinates(
                &cfs_cursor,
                &input.step_record,
                Some(&transition.next_expected_coordinates),
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
    use raster_core::cfs::CfsCoordinates;
    use raster_core::trace::{ExternalData, SequenceEndRecord, SequenceStartRecord, TileExecRecord};

    fn sha(bytes: &[u8]) -> Vec<u8> {
        sha256_bytes(bytes)
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
                selector: Default::default(),
                selected: raster_core::input::SelectedPayload {
                    bytes: selected_bytes.to_vec(),
                    ..Default::default()
                },
            },
        )]
        .into_iter()
        .collect()
    }

    fn authorization_journal(
        binding_name: &str,
        commitment: &[u8],
    ) -> AuthorizationJournal {
        AuthorizationJournal {
            external_inputs_commitments: [(binding_name.to_string(), commitment.to_vec())]
                .into_iter()
                .collect(),
            manifest_commitment: vec![7; 32],
        }
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
            external_input_commitment: external_input_commitment(&ext),
            output_commitment: sha(b"out"),
        });

        verify_io_witness(&step, Some(&b"in".to_vec()), Some(&b"out".to_vec()));
        verify_external_inputs(
            &step,
            &ext,
            &AuthorizationJournal {
                external_inputs_commitments: BTreeMap::new(),
                manifest_commitment: vec![0; 32],
            }
            .external_inputs_commitments,
        );
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
            external_input_commitment: external_input_commitment(&ext),
            output_commitment: sha(b"out"),
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
            &AuthorizationJournal {
                external_inputs_commitments: BTreeMap::new(),
                manifest_commitment: vec![0; 32],
            }
            .external_inputs_commitments,
        );
        verify_external_inputs(
            &end,
            &ExternalInput::new(),
            &AuthorizationJournal {
                external_inputs_commitments: BTreeMap::new(),
                manifest_commitment: vec![0; 32],
            }
            .external_inputs_commitments,
        );
    }

    #[test]
    fn verify_external_inputs_accept_matching_authorized_binding() {
        let ext = external_input(
            "personal_data",
            sha256_hex(b"payload").as_slice(),
            b"",
        );
        let step = StepRecord::TileExec(TileExecRecord {
            exec_index: 1,
            tile_id: "tile".to_string(),
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![0]),
            intra_sequence_index: 0,
            input_commitment: sha(b"in"),
            external_input_commitment: external_input_commitment(&ext),
            output_commitment: sha(b"out"),
        });

        let authorization = authorization_journal(
            "personal_data",
            sha256_hex(b"payload").as_slice(),
        );
        verify_external_inputs(&step, &ext, &authorization.external_inputs_commitments);
    }

    #[test]
    #[should_panic(expected = "Missing authorized commitment for external input 'personal_data'")]
    fn verify_external_inputs_reject_missing_authorized_binding() {
        let ext = external_input(
            "personal_data",
            sha256_hex(b"payload").as_slice(),
            b"",
        );
        let step = StepRecord::TileExec(TileExecRecord {
            exec_index: 1,
            tile_id: "tile".to_string(),
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![0]),
            intra_sequence_index: 0,
            input_commitment: sha(b"in"),
            external_input_commitment: external_input_commitment(&ext),
            output_commitment: sha(b"out"),
        });

        verify_external_inputs(
            &step,
            &ext,
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
        let ext = external_input(
            "personal_data",
            sha256_hex(b"payload").as_slice(),
            b"",
        );
        let step = StepRecord::TileExec(TileExecRecord {
            exec_index: 1,
            tile_id: "tile".to_string(),
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![0]),
            intra_sequence_index: 0,
            input_commitment: sha(b"in"),
            external_input_commitment: external_input_commitment(&ext),
            output_commitment: sha(b"out"),
        });

        let authorization = authorization_journal("personal_data", b"wrong");
        verify_external_inputs(&step, &ext, &authorization.external_inputs_commitments);
    }
}
