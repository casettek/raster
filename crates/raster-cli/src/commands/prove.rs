//! Trace commitment, audit, and fraud-proof generation.

use crate::utils::authorization::build_manifested_inputs;
use raster_backend::ExecutionMode;
use raster_core::{
    cfs::ControlFlowSchema,
    merkle::EMPTY_LEAF,
    trace::{ExecStep, ExecTarget, StepKind, StepRecord},
    Error, Result,
};
use raster_prover::{
    authorization::authorize_external_inputs,
    commitment_file::CommitmentFile,
    replay::Replayer,
    trace::{
        commitment_header, commitment_slice_witness, CommitmentRead, FraudEvidence,
        FraudProofConfig, SerializableFrontier, TraceRead, TraceVerifier, VerificationResult,
    },
    transition::step_transitions_with,
};
use raster_runtime::TraceRecorder;

pub fn commit(
    trace: &impl TraceRead,
    commit_path: &str,
    fraud_proof_config: FraudProofConfig,
) -> Result<()> {
    let trace_commitment = CommitmentFile::build(trace, &EMPTY_LEAF, fraud_proof_config)
        .map_err(|e| Error::Other(e.to_string()))?;
    trace_commitment
        .write(std::path::Path::new(commit_path))
        .map_err(Error::Io)
}

pub fn verify(
    trace: &impl TraceRead,
    commit_path: &str,
    cfs: &ControlFlowSchema,
) -> Result<VerificationResult> {
    let trace_commitment = read_trace_commitment(commit_path)?;

    if trace.len() != trace_commitment.len() {
        return Err(Error::Other(
            "execution and commitment lengths differ".into(),
        ));
    }
    let mut trace_verifier = TraceVerifier::new(trace_commitment, &EMPTY_LEAF, cfs)
        .map_err(|e| Error::Other(e.to_string()))?;

    Ok(trace_verifier.verify(trace))
}

fn build_program_frame(cfs: &ControlFlowSchema, replayer: &Replayer) -> Vec<u8> {
    crate::program::reassemble_and_verify(replayer.project(), cfs, replayer)
        .unwrap_or_else(|e| panic!("Failed to assemble program definition: {e}"))
        .canonical_bytes()
}

pub fn prove(
    fraud_evidence: FraudEvidence,
    trace: &impl TraceRead,
    cfs: &ControlFlowSchema,
    trace_recorder: &TraceRecorder,
    replayer: &Replayer,
    input_manifest: Option<&str>,
    trace_commitment: &impl CommitmentRead,
) -> risc0_zkvm::Receipt {
    let window_start_index = usize::try_from(
        SerializableFrontier::from_bytes(&fraud_evidence.window.frontier)
            .expect("window frontier")
            .position,
    )
    .expect("window position");
    let mut storage = raster_runtime::tracing::proof_input::ProofStorageState::for_recorded();
    for step in trace.prefix(window_start_index).records() {
        storage.advance(&step, trace_recorder);
    }
    let (initial_storage_frontier, initial_index_root, entrypoint_membership_witness) = storage
        .initial(
            raster_core::cfs::CfsCursor::new(cfs.clone())
                .main_entrypoint_names()
                .is_some(),
        );
    let window_start_recur_progress = window_start_index.checked_sub(1).and_then(|index| {
        trace_recorder.recur_progress_after(trace.record(index).unwrap().exec_index)
    });
    let io = fraud_evidence
        .window
        .items
        .iter()
        .map(|step| {
            raster_runtime::tracing::proof_input::prepare_step_io(
                step,
                trace_recorder,
                &mut storage,
            )
        })
        .collect();
    prove_prepared(
        raster_prover::transition::PreparedFault {
            window: fraud_evidence.window,
            input_sources_witnesses: fraud_evidence.input_sources_witnesses,
            io,
            initial_storage_frontier,
            initial_index_root,
            entrypoint_membership_witness,
            window_start_recur_progress,
        },
        cfs,
        replayer,
        input_manifest,
        trace_commitment,
    )
}

pub fn prove_prepared(
    prepared: raster_prover::transition::PreparedFault,
    cfs: &ControlFlowSchema,
    replayer: &Replayer,
    input_manifest: Option<&str>,
    trace_commitment: &impl CommitmentRead,
) -> risc0_zkvm::Receipt {
    let raster_prover::transition::PreparedFault {
        window: fraud_window,
        input_sources_witnesses,
        io,
        initial_storage_frontier,
        initial_index_root,
        entrypoint_membership_witness,
        window_start_recur_progress,
    } = prepared;
    assert_eq!(io.len(), fraud_window.items.len());
    let io_by_step: std::collections::HashMap<_, _> = fraud_window
        .items
        .iter()
        .map(|step| step.exec_index)
        .zip(io)
        .collect();
    let mode = ExecutionMode::prove_and_verify();
    let mut step_number = 0usize;
    let mut evidence_for_step = |step_record: &StepRecord| {
        step_number += 1;
        println!(
            "RASTER_PROOF_PROGRESS {}",
            serde_json::json!({"step": step_number, "total": io_by_step.len(), "exec_index": step_record.exec_index})
        );
        let io = io_by_step
            .get(&step_record.exec_index)
            .expect("prepared step IO")
            .clone();
        let replay = if let StepKind::Exec(ExecStep {
            target: ExecTarget::Tile(tile_id),
            ..
        }) = &step_record.kind
        {
            Some(
                replayer
                    .replay(
                        tile_id,
                        io.input_witness.as_deref().unwrap_or_default(),
                        mode,
                    )
                    .expect("replay tile"),
            )
        } else {
            None
        };
        (io, replay)
    };

    let manifested_inputs = build_manifested_inputs(input_manifest)
        .unwrap_or_else(|e| panic!("Failed to load authorization source: {}", e));

    let (authorization_receipt, authorization_journal) =
        authorize_external_inputs(&manifested_inputs);

    if let Some(frontier) = SerializableFrontier::from_bytes(&fraud_window.frontier) {
        println!();
        println!("Replaying transition frontier with transition guest...");

        let program_frame = build_program_frame(cfs, replayer);

        // Bind the window to the commitment it refutes: the window's start is
        // the initial frontier's position (seed + one leaf per pre-window
        // step), and the slice witness proves the window fingerprint occurs
        // in the commitment there. The guest re-derives and checks both.
        let commitment_header = commitment_header(trace_commitment);
        let window_start =
            usize::try_from(frontier.position).expect("window start position overflows usize");
        let fingerprint_slice = commitment_slice_witness(
            trace_commitment,
            window_start,
            fraud_window.fingerprint.len(),
        );

        let Some(receipt) = step_transitions_with(
            &frontier,
            &initial_storage_frontier,
            &initial_index_root,
            &fraud_window.items,
            fraud_window.fingerprint,
            &commitment_header,
            &fingerprint_slice,
            &program_frame,
            &input_sources_witnesses,
            &mut evidence_for_step,
            &authorization_journal,
            &authorization_receipt,
            entrypoint_membership_witness.as_ref(),
            window_start_recur_progress,
            // Lets a divergence the packed fingerprint is blind to still be
            // proven — the tail's roots are revealed in full.
            trace_commitment.tail(),
        ) else {
            panic!("Failed to generate fraud proof");
        };

        receipt
            .verify(raster_prover::TRANSITION_GUEST_ID)
            .expect("verify final transition receipt");
        return receipt;
    }

    panic!("Failed to generate fraud proof");
}

pub fn read_trace_commitment(commit_path: &str) -> Result<CommitmentFile> {
    CommitmentFile::read(std::path::Path::new(commit_path)).map_err(Error::Io)
}
