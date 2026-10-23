//! The TRANSITION part's segment (`transition-split`): the chains it commits,
//! how its segments stitch, and the inputs only the window's opening segment
//! may carry. Its per-step checks are tested in the rest of this suite.

use super::*;
use crate::merkle_tree::hash_trace_item;
use crate::part::{verify_segment, PartProgram};
use raster_core::fingerprint::{BitPacker, Fingerprint};
use raster_core::fraud_window::{
    authorization_digest, window_id, Chain, Column, PartKind, PartSegmentJournal,
    TransitionPartState, WindowChains,
};
use raster_core::program::{ProgramDefinition, ProgramManifest};
use raster_core::storage_check::{selection_witnesses_commitment, StorageState};
use raster_core::transition::{
    EntrypointAuthorization, InitTransition, OutputAuthorization, SerializableFrontier,
    TransitionPartHeader, TransitionStep,
};

const WINDOW_START: u64 = 20;

fn program() -> PartProgram {
    let schema = ControlFlowSchema {
        version: "1.0".into(),
        project: "test".into(),
        encoding: "postcard".into(),
        tiles: vec![],
        sequences: vec![SequenceDef::new("main")],
    };
    PartProgram {
        program: ProgramDefinition {
            manifest: ProgramManifest {
                name: "test".into(),
                version: "0".into(),
                inputs: BTreeMap::new(),
                output: None,
            },
            cfs: schema.clone(),
            tiles: BTreeMap::new(),
        },
        program_commitment: vec![0xc0; 32],
        cfs_cursor: CfsCursor::new(schema),
    }
}

fn init() -> InitTransition {
    InitTransition {
        init_frontier: SerializableFrontier {
            position: WINDOW_START,
            leaf: vec![1; 32],
            ommers: Vec::new(),
        },
        init_storage_frontier: SerializableFrontier {
            position: 0,
            leaf: vec![2; 32],
            ommers: Vec::new(),
        },
        init_storage_root: vec![3; 32],
        init_storage_index_root: vec![4; 32],
        fingerprint: Fingerprint {
            bits_packer: BitPacker::new(8),
            bits: vec![0],
            len: 3,
        },
    }
}

/// Three sequence starts at successive coordinates, each over its own store.
fn steps() -> Vec<TransitionStep> {
    (0..3u8)
        .map(|k| {
            let witness = storage_inputs_witness(&[]);
            TransitionStep {
                step_record: sequence_start_step(
                    CfsCoordinates(vec![i32::from(k) + 1]),
                    input_source_commitment(&witness),
                ),
                replay_journal: None,
                input_witness: None,
                output_witness: None,
                input_source_witness: Some(witness),
                storage_selection_witnesses: BTreeMap::new(),
                draft_transition_witness: None,
                store_before: StorageState {
                    position: u64::from(k),
                    root: vec![k; 32],
                    index_root: vec![k ^ 0xff; 32],
                },
                program_output_read_witness: None,
                program_output_selection_witness: None,
                returned_state_read_witness: None,
            }
        })
        .collect()
}

fn header(first_position: u64, step_count: u64) -> TransitionPartHeader {
    let chains = WindowChains::new(&window_id(&init()));
    TransitionPartHeader {
        init: init(),
        first_position,
        step_count,
        start: None,
        records_before: chains.records.value(),
        columns_before: [chains.store_before.value(), chains.selections.value()],
        replay_claims_before: Chain::replay_claims(&window_id(&init())).value(),
        authorization_journal: authorization_journal("arg", &sha(b"arg")),
        window_start_recur_progress: None,
        entrypoint_membership_witness: None,
    }
}

fn run(header: TransitionPartHeader, steps: Vec<TransitionStep>) -> PartSegmentJournal {
    let mut steps = steps.into_iter();
    let (journal, state) = verify_segment(&program(), header, || {
        steps.next().expect("the header counts the steps")
    });
    assert_eq!(
        journal.state.after,
        state.digest(),
        "the state handed on is the one committed"
    );
    journal
}

fn genesis_state() -> TransitionPartState {
    TransitionPartState {
        recur_progress: RecurProgressStack::new(),
        entrypoint_authorization: EntrypointAuthorization::NotRequired,
        output_authorization: OutputAuthorization::NotRequired,
    }
}

/// The record chain and both consumed columns fold exactly what the storage
/// part folds for the same steps, so FRAUD WINDOW can compare the two.
#[test]
fn a_segment_folds_records_and_both_consumed_columns() {
    let steps = steps();
    let journal = run(header(WINDOW_START, 3), steps.clone());

    let hashes: Vec<Vec<u8>> = steps
        .iter()
        .map(|s| hash_trace_item(&s.step_record))
        .collect();
    let selections = selection_witnesses_commitment(&BTreeMap::new());
    let expected = WindowChains::fold_all(
        &window_id(&init()),
        hashes
            .iter()
            .zip(&steps)
            .map(|(hash, step)| (hash.as_slice(), &step.store_before, &selections)),
    );
    assert_eq!(journal.part, PartKind::Transition);
    assert_eq!(journal.records.after, expected.records.value());
    assert_eq!(
        journal.column(Column::StoreBefore).unwrap().after,
        expected.store_before.value()
    );
    assert_eq!(
        journal.column(Column::Selections).unwrap().after,
        expected.selections.value()
    );
    assert_eq!(journal.state.before, genesis_state().digest());
    assert_eq!(journal.program_commitment, Some(vec![0xc0; 32]));
    let out = journal.transition_output().unwrap();
    assert_eq!(
        out.replay_claims.before, out.replay_claims.after,
        "no tile step, no claim"
    );
    assert_eq!(
        out.authorization,
        authorization_digest(&authorization_journal("arg", &sha(b"arg")))
    );
}

/// A segment inside the window starts from the state and chains the one
/// before it ended on, and the two end where the whole window does.
#[test]
fn segments_stitch_to_the_whole_window() {
    let whole = run(header(WINDOW_START, 3), steps());

    let mut steps = steps();
    let tail_steps = steps.split_off(1);
    let head = run(header(WINDOW_START, 1), steps);
    let mut tail_header = header(WINDOW_START + 1, 2);
    tail_header.start = Some(genesis_state());
    tail_header.records_before = head.records.after;
    tail_header.columns_before = [head.columns[0].1.after, head.columns[1].1.after];
    tail_header.replay_claims_before = head.transition_output().unwrap().replay_claims.after;
    let tail = run(tail_header, tail_steps);

    assert_eq!(head.state.after, tail.state.before);
    assert_eq!(tail.records.after, whole.records.after);
    assert_eq!(tail.state.after, whole.state.after);
    assert_eq!(
        tail.columns,
        vec![
            (
                Column::StoreBefore,
                raster_core::fraud_window::Span {
                    before: head.columns[0].1.after,
                    after: whole.columns[0].1.after,
                }
            ),
            (
                Column::Selections,
                raster_core::fraud_window::Span {
                    before: head.columns[1].1.after,
                    after: whole.columns[1].1.after,
                }
            ),
        ]
    );
}

#[test]
#[should_panic(expected = "derives its state; the host may not supply one")]
fn the_opening_segment_may_not_be_handed_a_state() {
    let mut opening = header(WINDOW_START, 3);
    opening.start = Some(genesis_state());
    run(opening, steps());
}

#[test]
#[should_panic(expected = "starts from the state the one before it ended on")]
fn a_later_segment_needs_its_start_state() {
    run(header(WINDOW_START + 1, 2), steps().split_off(1));
}

#[test]
#[should_panic(expected = "Only a window's opening step may carry a recur-progress seed")]
fn a_later_segment_may_not_carry_a_recur_seed() {
    let mut later = header(WINDOW_START + 1, 2);
    later.start = Some(genesis_state());
    later.window_start_recur_progress = Some(RecurProgressStack::new());
    run(later, steps().split_off(1));
}

#[test]
#[should_panic(
    expected = "Only a window's opening step may carry an entrypoint membership witness"
)]
fn a_later_segment_may_not_carry_an_entrypoint_witness() {
    use raster_core::transition::{StorageLogWitness, StorageReadWitness};
    let mut later = header(WINDOW_START + 1, 2);
    later.start = Some(genesis_state());
    later.entrypoint_membership_witness = Some(StorageReadWitness {
        entry: raster_core::transition::StorageEntry {
            coordinates: CfsCoordinates(vec![]),
            object_commitment: vec![0; 32],
        },
        log_witness: StorageLogWitness {
            position: 0,
            path_elems: Vec::new(),
        },
        index_witness: raster_core::transition::CoordinateIndexMembershipProof {
            coordinates: CfsCoordinates(vec![]),
            value: raster_core::transition::StorageIndexValue {
                log_position: 0,
                object_commitment: vec![0; 32],
            },
            siblings: Vec::new(),
        },
    });
    run(later, steps().split_off(1));
}

#[test]
#[should_panic(expected = "a transition segment must cover at least one step")]
fn an_empty_segment_is_refused() {
    run(header(WINDOW_START, 0), Vec::new());
}

/// The part reads the store before each step as a value: a different value
/// still passes its checks here, but leaves a different column chain, which
/// FRAUD WINDOW holds against the storage part's.
#[test]
fn a_different_store_before_changes_the_column_chain() {
    let honest = run(header(WINDOW_START, 3), steps());
    let mut steps = steps();
    steps[1].store_before.root[0] ^= 1;
    let tampered = run(header(WINDOW_START, 3), steps);
    assert_ne!(
        honest.column(Column::StoreBefore).unwrap().after,
        tampered.column(Column::StoreBefore).unwrap().after,
    );
    assert_eq!(honest.records, tampered.records);
}
