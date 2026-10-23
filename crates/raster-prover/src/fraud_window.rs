//! Host driver for the FRAUD WINDOW pipeline: how a fraud window is proven
//! (`docs/proposals/transition-split.md`).
//!
//! Every part's segment is independent of every other: the trace, control-flow
//! and storage segments (`window`), and the transition part's, which verify no
//! receipt. A transition segment needs the tile replay journals as values,
//! which the host gets by *executing* each tile (`Replayer::execute`), the
//! authorization journal, likewise executed
//! (`authorization::execute_authorization`), and the state the segment before
//! it ended on, which the host gets by executing that segment
//! ([`plan_transition_part`]). Then every segment, every tile replay and the
//! authorization are proven in one batch (`schedule::run_jobs`), and FRAUD
//! WINDOW last, with all of their receipts as assumptions
//! ([`prove_fraud_window`]). Its receipt is the fraud proof, pinned by one
//! image id ([`verify_fraud_window_receipt`]).

use std::collections::HashMap;

use raster_core::authorization::AuthorizationJournal;
use raster_core::draft::TileReplayJournal;
use raster_core::fraud_window::{
    storage_state_digest, window_id, Chain, FraudWindowJournal, FraudWindowParams,
    PartSegmentJournal, ReplayClaim, TransitionPartState, WindowChains,
};
use raster_core::recur_progress::RecurProgressStack;
use raster_core::storage_check::selection_witnesses_commitment;
use raster_core::trace::StepRecord;
use raster_core::transition::{StorageReadWitness, TransitionPartHeader, TransitionStep};
use sha2::{Digest, Sha256};

use crate::schedule::Job;
use crate::segments::{SegmentLength, ZkvmSegmentPo2};
use crate::storage::stores_before;
use crate::transition::{write_frame, write_postcard, StepIo};
use crate::window::{prove_part_segment, ProvenPartSegment, WindowSetup};
use crate::{
    FRAUD_WINDOW_GUEST_ELF, FRAUD_WINDOW_GUEST_ID, TRANSITION_GUEST_ELF, TRANSITION_GUEST_ID,
};

/// The FRAUD WINDOW guest's image id, as the byte encoding journals carry: the
/// one id a relying party pins, with every part's compiled into it.
pub fn fraud_window_guest_image_id() -> Vec<u8> {
    FRAUD_WINDOW_GUEST_ID
        .into_iter()
        .flat_map(|val| val.to_le_bytes())
        .collect()
}

/// The transition guest's image id, as journals carry it.
pub fn transition_guest_image_id() -> Vec<u8> {
    TRANSITION_GUEST_ID
        .into_iter()
        .flat_map(|val| val.to_le_bytes())
        .collect()
}

/// Each tile step's replay, executed but not proven: the registry image id the
/// tile compiled to, and its replay journal.
pub type ReplayJournals = HashMap<StepRecord, ([u8; 32], TileReplayJournal)>;

/// The claim FRAUD WINDOW verifies for one executed tile step: the receipt of
/// its image id whose journal hashes to the executed journal's encoding.
pub fn replay_claim(image_id: &[u8; 32], journal: &TileReplayJournal) -> ReplayClaim {
    let bytes = postcard::to_allocvec(journal).expect("a replay journal always encodes");
    ReplayClaim {
        image_id: *image_id,
        journal_digest: Sha256::digest(&bytes).into(),
    }
}

/// Every tile step's replay claim, in window order: the order the transition
/// part folds them in.
pub fn replay_claims(window: &[StepRecord], replays: &ReplayJournals) -> Vec<ReplayClaim> {
    window
        .iter()
        .filter(|step| step.requires_replay_proof())
        .map(|step| {
            let (image_id, journal) = replay_for(replays, step);
            replay_claim(image_id, journal)
        })
        .collect()
}

fn replay_for<'a>(
    replays: &'a ReplayJournals,
    step: &StepRecord,
) -> &'a ([u8; 32], TileReplayJournal) {
    replays
        .get(step)
        .unwrap_or_else(|| panic!("no executed replay for tile step {:?}", step.coordinates))
}

/// One planned transition part segment: its header and steps.
#[derive(Clone)]
pub struct TransitionPartSegment {
    pub header: TransitionPartHeader,
    pub steps: Vec<TransitionStep>,
}

/// What the window's transition part reads besides the records and their I/O.
pub struct TransitionInputs<'a> {
    pub replays: &'a ReplayJournals,
    pub authorization_journal: &'a AuthorizationJournal,
    pub entrypoint_membership_witness: Option<&'a StorageReadWitness>,
    pub window_start_recur_progress: Option<RecurProgressStack>,
}

/// One step as the transition part reads it: the record, the witnesses its
/// checks read (not the scope witness or the storage witness, which only the
/// control-flow and storage guests read), its executed replay journal if it
/// runs a tile, and the store before it.
pub(crate) fn transition_step(
    step_record: &StepRecord,
    step_io: &StepIo,
    store_before: raster_core::storage_check::StorageState,
    replays: &ReplayJournals,
) -> TransitionStep {
    let StepIo {
        input_witness,
        output_witness,
        input_source_witness,
        sequence_scope_witness: _,
        storage_selection_witnesses,
        storage_witness: _,
        draft_transition_witness,
        program_output_read_witness,
        program_output_selection_witness,
        returned_state_read_witness,
    } = step_io.clone();
    TransitionStep {
        step_record: step_record.clone(),
        replay_journal: step_record
            .requires_replay_proof()
            .then(|| replay_for(replays, step_record).1.clone()),
        input_witness,
        output_witness,
        input_source_witness,
        storage_selection_witnesses,
        draft_transition_witness,
        store_before,
        program_output_read_witness,
        program_output_selection_witness,
        returned_state_read_witness,
    }
}

/// Split the window into transition part segments.
///
/// Each segment starts from the chains the steps before it fold to, which the
/// host computes, and — inside the window — from the part state the segment
/// before it ended on, which only the guest's checks compute: so each segment
/// but the last is executed, in order, to learn the next one's start. Executing
/// costs no proof; the segments are proven afterwards, independently.
pub fn plan_transition_part(
    setup: &WindowSetup<'_>,
    inputs: &TransitionInputs<'_>,
    segments: SegmentLength,
    zkvm_segment_po2: ZkvmSegmentPo2,
) -> Vec<TransitionPartSegment> {
    let init = setup.init_transition();
    let window = window_id(&init);
    let records = setup.trace_window;
    let befores = stores_before(&init, records, setup.recorded_step_io);
    let mut chains = WindowChains::new(&window);
    let mut replay_chain = Chain::replay_claims(&window);
    let segment_len = segments.len_for(records.len());

    let mut planned: Vec<TransitionPartSegment> = Vec::new();
    let mut start: Option<TransitionPartState> = None;
    for (chunk_index, chunk) in records.chunks(segment_len).enumerate() {
        let first_index = chunk_index * segment_len;
        let opens_window = first_index == 0;
        if let Some(previous) = planned.last() {
            start = Some(execute_transition_part_segment(
                setup.program_frame,
                previous,
                zkvm_segment_po2,
            ));
        }
        let header = TransitionPartHeader {
            init: init.clone(),
            first_position: init.init_frontier.position + first_index as u64,
            step_count: chunk.len() as u64,
            start: start.clone(),
            records_before: chains.records.value(),
            columns_before: [chains.store_before.value(), chains.selections.value()],
            replay_claims_before: replay_chain.value(),
            authorization_journal: inputs.authorization_journal.clone(),
            window_start_recur_progress: opens_window
                .then(|| inputs.window_start_recur_progress.clone())
                .flatten(),
            entrypoint_membership_witness: opens_window
                .then(|| inputs.entrypoint_membership_witness.cloned())
                .flatten(),
        };
        let steps = chunk
            .iter()
            .enumerate()
            .map(|(offset, step_record)| {
                let step_io = setup.recorded_step_io.get(step_record).unwrap_or_else(|| {
                    panic!("Missing recorded I/O for transition step {:?}", step_record)
                });
                let before = befores[first_index + offset].clone();
                chains
                    .records
                    .fold(&crate::trace::BytesHashable::hash(step_record));
                chains.store_before.fold(&storage_state_digest(&before));
                chains.selections.fold(&selection_witnesses_commitment(
                    &step_io.storage_selection_witnesses,
                ));
                if step_record.requires_replay_proof() {
                    let (image_id, journal) = replay_for(inputs.replays, step_record);
                    replay_claim(image_id, journal).fold_into(&mut replay_chain);
                }
                transition_step(step_record, step_io, before, inputs.replays)
            })
            .collect();
        planned.push(TransitionPartSegment { header, steps });
    }
    planned
}

fn transition_part_env<'a>(
    program_frame: &[u8],
    segment: &TransitionPartSegment,
    zkvm_segment_po2: ZkvmSegmentPo2,
    stdout: &'a mut Vec<u8>,
) -> risc0_zkvm::ExecutorEnv<'a> {
    let mut builder = zkvm_segment_po2.env_builder();
    write_frame(&mut builder, program_frame);
    write_postcard(&mut builder, &segment.header);
    for step in &segment.steps {
        write_postcard(&mut builder, step);
    }
    builder.stdout(stdout);
    builder.build().unwrap()
}

/// Execute a transition part segment, without proving it, for the state it
/// ends on.
fn execute_transition_part_segment(
    program_frame: &[u8],
    segment: &TransitionPartSegment,
    zkvm_segment_po2: ZkvmSegmentPo2,
) -> TransitionPartState {
    let mut stdout = Vec::new();
    {
        let env = transition_part_env(program_frame, segment, zkvm_segment_po2, &mut stdout);
        let __probe_t = std::time::Instant::now(); // TEMP(cycle-probe)
        let session = risc0_zkvm::default_executor()
            .execute(env, TRANSITION_GUEST_ELF)
            .unwrap_or_else(|e| {
                panic!(
                    "the transition part refused the segment at trace position {}: {e}",
                    segment.header.first_position
                )
            });
        eprintln!("CYCLEPROBE label={} user={} total={} segments={} ms={}", "transition-exec", session.cycles(), session.segments.iter().map(|s| 1u64 << s.po2).sum::<u64>(), session.segments.len(), __probe_t.elapsed().as_millis()); // TEMP(cycle-probe)
    }
    postcard::from_bytes(&stdout).expect("the transition part writes its state after to stdout")
}

pub fn prove_transition_part_segment(
    program_frame: &[u8],
    segment: &TransitionPartSegment,
    zkvm_segment_po2: ZkvmSegmentPo2,
) -> ProvenPartSegment {
    let mut builder = zkvm_segment_po2.env_builder();
    write_frame(&mut builder, program_frame);
    write_postcard(&mut builder, &segment.header);
    for step in &segment.steps {
        write_postcard(&mut builder, step);
    }
    // The state after, written for the next segment's planning; not needed
    // once the segment is planned.
    builder.stdout(std::io::sink());
    prove_part_segment(
        TRANSITION_GUEST_ELF,
        builder.build().unwrap(),
        format_args!(
            "the transition part refused the segment at trace position {}",
            segment.header.first_position
        ),
    )
}

/// One job per transition part segment, for `schedule::run_jobs`.
pub fn transition_part_jobs<'a>(
    program_frame: &'a [u8],
    planned: &'a [TransitionPartSegment],
    zkvm_segment_po2: ZkvmSegmentPo2,
) -> Vec<Job<'a, ProvenPartSegment>> {
    planned
        .iter()
        .map(|segment| {
            Box::new(move || {
                prove_transition_part_segment(program_frame, segment, zkvm_segment_po2)
            }) as Job<'a, ProvenPartSegment>
        })
        .collect()
}

/// Every receipt FRAUD WINDOW assumes, and what it reads.
pub struct FraudWindowInputs<'a> {
    /// Every segment of all four parts, any order.
    pub segments: &'a [ProvenPartSegment],
    /// Every tile step's replay receipt, any order.
    pub replay_receipts: Vec<risc0_zkvm::Receipt>,
    /// Every tile step's claim, in window order.
    pub replay_claims: Vec<ReplayClaim>,
    pub authorization_receipt: &'a risc0_zkvm::Receipt,
    pub authorization_journal: &'a AuthorizationJournal,
}

/// What FRAUD WINDOW is proven from: the receipts it assumes, the segment
/// journals it verifies them by, and its params.
pub struct FraudWindowProof {
    pub assumptions: Vec<risc0_zkvm::Receipt>,
    pub segment_journals: Vec<Vec<u8>>,
    pub params: FraudWindowParams,
}

/// Assemble FRAUD WINDOW's input from a window's proven segments, and check it
/// as the guest will, so a refusal names its cause instead of surfacing as a
/// guest panic.
pub fn fraud_window_proof(
    setup: &WindowSetup<'_>,
    inputs: FraudWindowInputs<'_>,
) -> FraudWindowProof {
    let params = FraudWindowParams {
        init: setup.init_transition(),
        program_commitment: raster_core::program::commitment_of_bytes(setup.program_frame).to_vec(),
        replay_claims: inputs.replay_claims,
        authorization_journal: inputs.authorization_journal.clone(),
    };
    let journals: Vec<PartSegmentJournal> =
        inputs.segments.iter().map(|s| s.journal.clone()).collect();
    if let Err(refusal) = raster_core::fraud_window::check_window(&params, &journals) {
        panic!("FRAUD WINDOW would refuse the window: {refusal}");
    }
    let mut assumptions: Vec<risc0_zkvm::Receipt> =
        inputs.segments.iter().map(|s| s.receipt.clone()).collect();
    assumptions.extend(inputs.replay_receipts);
    assumptions.push(inputs.authorization_receipt.clone());
    FraudWindowProof {
        assumptions,
        segment_journals: inputs
            .segments
            .iter()
            .map(|s| s.receipt.journal.bytes.clone())
            .collect(),
        params,
    }
}

/// Prove FRAUD WINDOW. Its receipt is the fraud proof: composite, carrying
/// every part, replay and authorization receipt as an assumption.
pub fn prove_fraud_window_from(
    proof: FraudWindowProof,
    zkvm_segment_po2: ZkvmSegmentPo2,
) -> Result<risc0_zkvm::Receipt, String> {
    let mut builder = zkvm_segment_po2.env_builder();
    for receipt in proof.assumptions {
        builder.add_assumption(receipt);
    }
    write_postcard(&mut builder, &(proof.segment_journals.len() as u32));
    for journal in &proof.segment_journals {
        write_frame(&mut builder, journal);
    }
    write_postcard(&mut builder, &proof.params);
    let env = builder.build().map_err(|e| e.to_string())?;
    let __probe_t = std::time::Instant::now(); // TEMP(cycle-probe)
    risc0_zkvm::default_prover()
        .prove(env, FRAUD_WINDOW_GUEST_ELF)
        .map(|info| {
            eprintln!("CYCLEPROBE label={} user={} total={} segments={} ms={}", "fraud-window", info.stats.user_cycles, info.stats.total_cycles, info.stats.segments, __probe_t.elapsed().as_millis()); // TEMP(cycle-probe)
            info.receipt
        })
        .map_err(|e| e.to_string())
}

/// Prove FRAUD WINDOW over a window's proven segments.
pub fn prove_fraud_window(
    setup: &WindowSetup<'_>,
    inputs: FraudWindowInputs<'_>,
    zkvm_segment_po2: ZkvmSegmentPo2,
) -> risc0_zkvm::Receipt {
    prove_fraud_window_from(fraud_window_proof(setup, inputs), zkvm_segment_po2)
        .unwrap_or_else(|e| panic!("FRAUD WINDOW refused the window: {e}"))
}

/// Verify a FRAUD WINDOW fraud proof against the one pinned id and return its
/// journal.
pub fn verify_fraud_window_receipt(
    receipt: &risc0_zkvm::Receipt,
) -> crate::error::Result<FraudWindowJournal> {
    use crate::error::BitPackerError;
    receipt
        .verify(FRAUD_WINDOW_GUEST_ID)
        .map_err(|e| BitPackerError::InvalidCommitment(format!("fraud-proof receipt: {e}")))?;
    FraudWindowJournal::decode(&receipt.journal.bytes)
        .map_err(|e| BitPackerError::SerializationError(format!("fraud-proof journal: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorization::{authorization_guest_image_id, authorize_external_inputs};
    use crate::schedule::run_jobs;
    use crate::segments::ProveOptions;
    use crate::trace::{SerializableFrontier, TraceCommitment, TraceCommitmentExt};
    use crate::window::{part_jobs, plan_parts};
    use raster_core::authorization::ManifestedInputs;
    use raster_core::cfs::{
        CfsCoordinate, CfsCoordinates, ControlFlowSchema, SequenceDef, FIRST_COORDINATE,
    };
    use raster_core::coordinate_index::coordinate_index_root;
    use raster_core::fingerprint::{BitPacker, Fingerprint};
    use raster_core::merkle::EMPTY_LEAF;
    use raster_core::storage_check::StorageState;
    use raster_core::trace::{ExecStep, ExecTarget, FnInput, StepKind, StorageRoots};
    use std::collections::BTreeMap;

    fn image_id_bytes(id: [u32; 8]) -> Vec<u8> {
        id.into_iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn io_witnesses(input_witness: Option<Vec<u8>>, output_witness: Option<Vec<u8>>) -> StepIo {
        StepIo {
            input_witness,
            output_witness,
            ..StepIo::default()
        }
    }

    fn make_tile_step(exec_index: u64, coordinates: Vec<CfsCoordinate>) -> StepRecord {
        StepRecord {
            exec_index,
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(coordinates),
            kind: StepKind::Exec(ExecStep {
                target: ExecTarget::Tile("shared_tile".to_string()),
                intra_sequence_index: FIRST_COORDINATE,
                input_commitment: vec![exec_index as u8],
                input_source_commitment: Vec::new(),
                output_commitment: vec![exec_index as u8 + 1],
                storage: StorageRoots {
                    root_before: EMPTY_LEAF.to_vec(),
                    root_after: EMPTY_LEAF.to_vec(),
                    index_root_before: Vec::new(),
                    index_root_after: Vec::new(),
                },
            }),
            recur_progress_commitment: RecurProgressStack::new().commitment(),
            recur_state: None,
        }
    }

    fn replay_journal(output_bytes: Vec<u8>) -> TileReplayJournal {
        TileReplayJournal {
            input_commitment: [0u8; 32],
            output_bytes,
            output_root: None,
            input_roots: Vec::new(),
            draft_transition: None,
            recur: None,
        }
    }

    fn store(position: u64) -> StorageState {
        StorageState {
            position,
            root: vec![1; 32],
            index_root: vec![2; 32],
        }
    }

    #[test]
    fn a_transition_step_takes_its_own_replay_and_witnesses_for_repeated_tiles() {
        let first_step = make_tile_step(1, vec![0]);
        let second_step = make_tile_step(2, vec![1]);
        let recorded = HashMap::from([
            (
                first_step.clone(),
                io_witnesses(Some(vec![1]), Some(vec![11])),
            ),
            (
                second_step.clone(),
                io_witnesses(Some(vec![2]), Some(vec![22])),
            ),
        ]);
        let replays = ReplayJournals::from([
            (first_step.clone(), ([9; 32], replay_journal(vec![11]))),
            (second_step.clone(), ([7; 32], replay_journal(vec![22]))),
        ]);

        let step = transition_step(&second_step, &recorded[&second_step], store(3), &replays);
        assert_eq!(step.replay_journal.as_ref().unwrap().output_bytes, vec![22]);
        assert_eq!(step.input_witness, Some(vec![2]));
        assert_eq!(step.output_witness, Some(vec![22]));
        assert_eq!(step.store_before, store(3));
        assert_eq!(
            replay_claims(&[first_step, second_step], &replays),
            vec![
                replay_claim(&[9; 32], &replay_journal(vec![11])),
                replay_claim(&[7; 32], &replay_journal(vec![22])),
            ],
            "claims in window order, each with its own image id",
        );
    }

    #[test]
    fn a_transition_step_keeps_a_tile_s_error_output_bytes() {
        let tile_step = make_tile_step(1, vec![0]);
        let error_output =
            raster_core::postcard::to_allocvec(&Err::<u64, String>("denied".to_string())).unwrap();
        let recorded = HashMap::from([(
            tile_step.clone(),
            io_witnesses(Some(vec![9]), Some(error_output.clone())),
        )]);
        let replays = ReplayJournals::from([(
            tile_step.clone(),
            ([5; 32], replay_journal(error_output.clone())),
        )]);

        let step = transition_step(&tile_step, &recorded[&tile_step], store(0), &replays);
        assert_eq!(step.replay_journal.unwrap().output_bytes, error_output);
        assert_eq!(step.output_witness, Some(error_output));
    }

    #[test]
    fn a_sequence_step_carries_its_recorded_io_and_no_replay() {
        let sequence_start = StepRecord {
            exec_index: 1,
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![]),
            kind: StepKind::SequenceStart {
                input_commitment: vec![1; 32],
                input_source_commitment: Vec::new(),
            },
            recur_progress_commitment: RecurProgressStack::new().commitment(),
            recur_state: None,
        };
        let step = transition_step(
            &sequence_start,
            &io_witnesses(Some(vec![3, 4]), None),
            store(0),
            &ReplayJournals::new(),
        );
        assert!(step.replay_journal.is_none());
        assert_eq!(step.input_witness, Some(vec![3, 4]));
        assert_eq!(step.output_witness, None);
        assert!(replay_claims(&[sequence_start], &ReplayJournals::new()).is_empty());
    }

    #[test]
    fn authorize_external_inputs_returns_expected_journal() {
        let manifest = ManifestedInputs {
            manifest_bytes: br#"{"personal_data":{"type":"sha256","commitment":"239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5"}}"#
                .to_vec(),
        };
        let (_receipt, authorization) = authorize_external_inputs(&manifest, Default::default());
        assert_eq!(
            authorization
                .external_inputs_commitments
                .get("personal_data"),
            Some(&b"239f59ed55e737c77147cf55ad0c1b030b6d7ee748a7426952f9b852d5a935e5".to_vec())
        );
        assert_eq!(
            crate::authorization::execute_authorization(&manifest),
            authorization,
            "executing the guest gives the journal its receipt commits",
        );
        assert_eq!(
            authorization_guest_image_id(),
            image_id_bytes(crate::AUTHORIZATION_GUEST_ID)
        );
    }

    // --- One window, proven through every guest in the zkVM -----------------

    fn empty_input_source_witness() -> FnInput {
        FnInput {
            data: Vec::new(),
            values: Vec::new(),
            args: Vec::new(),
            storage: Default::default(),
        }
    }

    fn sequence_start_step() -> StepRecord {
        StepRecord {
            exec_index: 1,
            sequence_id: "main".to_string(),
            coordinates: CfsCoordinates(vec![]),
            kind: StepKind::SequenceStart {
                input_commitment: Sha256::digest(b"sequence-in").to_vec(),
                input_source_commitment: Sha256::digest(
                    empty_input_source_witness().source_witness_bytes(),
                )
                .to_vec(),
            },
            recur_progress_commitment: RecurProgressStack::new().commitment(),
            recur_state: None,
        }
    }

    /// The `program.bin` frame for a minimal program (no tiles, unit main).
    fn minimal_program_frame() -> Vec<u8> {
        let mut cfs = ControlFlowSchema::new("test");
        cfs.sequences.push(SequenceDef::new("main"));
        let manifest = raster_core::program::ProgramManifest {
            name: "test".to_string(),
            version: "0.0.0".to_string(),
            inputs: Default::default(),
            output: None,
        };
        raster_core::program::ProgramDefinition::assemble(manifest, cfs, Default::default())
            .expect("minimal program definition")
            .canonical_bytes()
    }

    /// A one-step window at the head of a trace whose only step diverges from
    /// its commitment.
    struct Window {
        frontier: SerializableFrontier,
        index_root: Vec<u8>,
        records: Vec<StepRecord>,
        fingerprint: Fingerprint,
        commitment: TraceCommitment,
        program_frame: Vec<u8>,
        parents: BTreeMap<u64, (StepRecord, raster_core::transition::StepRecordWitness)>,
        io: HashMap<StepRecord, StepIo>,
    }

    impl Window {
        fn new() -> Self {
            let fingerprint = Fingerprint::from(vec![0], BitPacker::new(64), 1);
            Self {
                frontier: SerializableFrontier {
                    position: 0,
                    leaf: EMPTY_LEAF.to_vec(),
                    ommers: Vec::new(),
                },
                index_root: coordinate_index_root(&BTreeMap::new()),
                records: vec![sequence_start_step()],
                fingerprint: fingerprint.clone(),
                commitment: TraceCommitment {
                    fingerprint,
                    revealed_items: vec![sequence_start_step()],
                    revealed_tail_roots: vec![[0u8; 32].into()],
                },
                program_frame: minimal_program_frame(),
                parents: BTreeMap::new(),
                io: HashMap::from([(
                    sequence_start_step(),
                    StepIo {
                        input_witness: Some(b"sequence-in".to_vec()),
                        input_source_witness: Some(empty_input_source_witness()),
                        ..StepIo::default()
                    },
                )]),
            }
        }
    }

    #[test]
    fn a_window_proves_through_fraud_window_and_every_assumption_is_needed() {
        let w = Window::new();
        let header = w.commitment.header();
        let slice = w.commitment.fingerprint_slice_witness(0, 1);
        let setup = WindowSetup {
            initial_frontier: &w.frontier,
            initial_storage_frontier: &w.frontier,
            initial_storage_index_root: &w.index_root,
            trace_window: &w.records,
            fingerprint: w.fingerprint.clone(),
            commitment_header: &header,
            fingerprint_slice: &slice,
            revealed_tail_roots: &w.commitment.revealed_tail_roots,
            program_frame: &w.program_frame,
            sequence_scope_parents: &w.parents,
            recorded_step_io: &w.io,
        };
        let manifest = ManifestedInputs {
            manifest_bytes: Vec::new(),
        };
        let (authorization_receipt, authorization) =
            authorize_external_inputs(&manifest, Default::default());
        let options = ProveOptions::default();
        let replays = ReplayJournals::new();
        let planned = plan_parts(&setup, options.segments);
        let transition = plan_transition_part(
            &setup,
            &TransitionInputs {
                replays: &replays,
                authorization_journal: &authorization,
                entrypoint_membership_witness: None,
                window_start_recur_progress: None,
            },
            SegmentLength::Window,
            options.zkvm_segment_po2,
        );
        let mut jobs = part_jobs(&planned, &w.program_frame, options.zkvm_segment_po2);
        jobs.extend(transition_part_jobs(
            &w.program_frame,
            &transition,
            options.zkvm_segment_po2,
        ));
        let segments = run_jobs(options.workers, jobs);
        let inputs = |authorization_journal| FraudWindowInputs {
            segments: &segments,
            replay_receipts: Vec::new(),
            replay_claims: replay_claims(&w.records, &replays),
            authorization_receipt: &authorization_receipt,
            authorization_journal,
        };

        let receipt = prove_fraud_window(&setup, inputs(&authorization), options.zkvm_segment_po2);
        let journal = verify_fraud_window_receipt(&receipt).unwrap();
        assert_eq!(journal.init_state, setup.init_transition());
        assert_eq!(
            journal.program_commitment,
            raster_core::program::commitment_of_bytes(&w.program_frame).to_vec()
        );
        assert_eq!(
            journal.input_manifest_commitment,
            authorization.input_manifest_commitment
        );
        assert_eq!(journal.refuted_trace_commitment, header.digest());
        assert_eq!(
            fraud_window_guest_image_id(),
            image_id_bytes(FRAUD_WINDOW_GUEST_ID)
        );

        // Every part segment, and the authorization, is an assumption FRAUD
        // WINDOW verifies against an id compiled into it: drop any one and the
        // guest refuses, though every journal still agrees.
        let count = fraud_window_proof(&setup, inputs(&authorization))
            .assumptions
            .len();
        assert_eq!(
            count, 5,
            "trace, control flow, storage, transition, authorization"
        );
        for dropped in 0..count {
            let mut missing = fraud_window_proof(&setup, inputs(&authorization));
            missing.assumptions.remove(dropped);
            assert!(
                prove_fraud_window_from(missing, options.zkvm_segment_po2).is_err(),
                "FRAUD WINDOW accepted a window missing assumption {dropped} of {count}",
            );
        }

        // An authorization journal its receipt doesn't commit is refused in
        // the guest, even past the host's own check.
        let mut tampered = fraud_window_proof(&setup, inputs(&authorization));
        tampered
            .params
            .authorization_journal
            .input_manifest_commitment = vec![9; 32];
        assert!(prove_fraud_window_from(tampered, options.zkvm_segment_po2).is_err());
    }
}
