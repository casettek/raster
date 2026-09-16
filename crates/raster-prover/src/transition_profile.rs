//! Execution-only measurement of a representative continuation step.
//!
//! The predecessor is a modeled assumption, NOT a proven transition. Nothing
//! from this module may be used as a fault proof or a canonical program identity.

use std::collections::BTreeMap;

use raster_core::authorization::{AuthorizationJournal, ManifestedInputs};
use raster_core::draft::TrackedDraftState;
use raster_core::fingerprint::{BitPacker, FingerprintAccumulator};
use raster_core::program::ProgramDefinition;
use raster_core::recur_progress::RecurProgressStack;
use raster_core::transition::{
    EntrypointAuthorization, InitTransition, OutputAuthorization, SerializableFrontier, Transition,
    TransitionInput, TransitionJournal, TransitionState,
};
use raster_core::{Error, Result};
use risc0_zkvm::{default_executor, ExecutorEnv, ExitCode, ReceiptClaim};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::authorization::authorization_guest_image_id;
use crate::trace::{
    serializable_frontier_from_trace_frontier, serializable_frontier_into_trace_frontier, Bytes,
    BytesHashable, FraudProofConfig, TraceTree,
};
use crate::{AUTHORIZATION_GUEST_ELF, TRANSITION_GUEST_ELF, TRANSITION_GUEST_ID};

pub const CONTEXT_KIND: &str = "representative-continuation";
pub const CONTEXT_VERSION: u32 = 1;
pub const CONTEXT_WINDOW_SIZE: usize = 128;

/// Real local state captured before the sampled native step.
#[derive(Debug, Clone)]
pub struct TransitionProfileContext {
    pub frontier: SerializableFrontier,
    pub storage_frontier: SerializableFrontier,
    pub storage_root: Vec<u8>,
    pub storage_index_root: Vec<u8>,
    pub recur_progress: RecurProgressStack,
}

pub struct ProfileAuthorization {
    pub journal: AuthorizationJournal,
    claim: ReceiptClaim,
}

fn error(message: impl std::fmt::Display) -> Error {
    Error::Other(message.to_string())
}

fn journal_bytes(value: &impl Serialize) -> Result<Vec<u8>> {
    Ok(risc0_zkvm::serde::to_vec(value)
        .map_err(error)?
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect())
}

fn claim(image_id: &[u8], journal: Vec<u8>) -> Result<ReceiptClaim> {
    let image_id = risc0_zkvm::sha::Digest::try_from(image_id).map_err(error)?;
    Ok(ReceiptClaim::ok(image_id, journal))
}

pub fn transition_image_id() -> String {
    hex::encode(transition_image_bytes())
}

fn transition_image_bytes() -> Vec<u8> {
    TRANSITION_GUEST_ID
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect()
}

/// Executed once per run, without proving. Its cycles are not tile overhead.
pub fn profile_authorization(input: &ManifestedInputs) -> Result<ProfileAuthorization> {
    let mut builder = ExecutorEnv::builder();
    builder.stdout(std::io::sink());
    builder.write(input).map_err(error)?;
    let session = default_executor()
        .execute(builder.build().map_err(error)?, AUTHORIZATION_GUEST_ELF)
        .map_err(error)?;
    if session.exit_code != ExitCode::Halted(0) {
        return Err(error("Authorization guest did not halt successfully"));
    }
    let journal = session.journal.decode().map_err(error)?;
    let claim = claim(&authorization_guest_image_id(), session.journal.bytes)?;
    Ok(ProfileAuthorization { journal, claim })
}

fn root(frontier: &SerializableFrontier) -> Result<Vec<u8>> {
    let frontier = serializable_frontier_into_trace_frontier(frontier.clone())
        .ok_or_else(|| error("Invalid profiling frontier"))?;
    TraceTree::from_frontier(1, frontier)
        .root(0)
        .map(|root| root.0)
        .ok_or_else(|| error("Missing profiling tree root"))
}

/// Execute the production transition ELF's ordinary `Next` path. The selected
/// replay's journal and all local witnesses are real; prior-chain claims are
/// deliberately unresolved. No proving API is called and no receipt is returned.
pub fn profile_transition(
    program: &ProgramDefinition,
    context: &TransitionProfileContext,
    input: TransitionInput,
    authorization: &ProfileAuthorization,
) -> Result<u64> {
    let (state, previous, expected_frontier, expected_fingerprint) =
        continuation(program, context, &input)?;
    let mut builder = ExecutorEnv::builder();
    builder.stdout(std::io::sink());
    builder.add_assumption(authorization.claim.clone());
    let replay = input
        .replay_journal
        .as_ref()
        .ok_or_else(|| error("Missing replay journal"))?;
    let tile = match &input.step_record.kind {
        raster_core::trace::StepKind::Exec(raster_core::trace::ExecStep {
            target: raster_core::trace::ExecTarget::Tile(tile),
            ..
        }) => tile,
        _ => {
            return Err(error(
                "Only tile invocations have transition overhead samples",
            ))
        }
    };
    let image_id = program
        .tile_image_id(tile)
        .ok_or_else(|| error("Missing sampled tile image"))?;
    builder.add_assumption(claim(
        image_id,
        postcard::to_allocvec(replay).map_err(error)?,
    )?);
    builder.add_assumption(claim(&transition_image_bytes(), journal_bytes(&previous)?)?);
    builder.write(&program.canonical_bytes()).map_err(error)?;
    builder.write(&transition_image_bytes()).map_err(error)?;
    builder.write(&input).map_err(error)?;
    builder.write(&state).map_err(error)?;
    builder.write(&previous).map_err(error)?;
    let session = default_executor()
        .execute(builder.build().map_err(error)?, TRANSITION_GUEST_ELF)
        .map_err(error)?;
    if session.exit_code != ExitCode::Halted(0) {
        return Err(error("Transition guest did not halt successfully"));
    }
    let result: TransitionJournal = session.journal.decode().map_err(error)?;
    let TransitionState::Next(next) = &result.current_state else {
        return Err(error(
            "Profile transition must remain a nonterminal continuation",
        ));
    };
    let storage = input
        .step_record
        .storage_roots()
        .ok_or_else(|| error("Missing storage roots"))?;
    let mut expected_drafts = BTreeMap::new();
    if let Some(draft) = replay.draft_transition.as_ref() {
        let witness = input
            .draft_transition_witness
            .as_ref()
            .ok_or_else(|| error("Missing sampled draft witness"))?;
        let (_, root) =
            raster_core::draft::apply_draft_ops(&witness.pre_state, &draft.ops).map_err(error)?;
        expected_drafts.insert(
            draft.draft_id,
            TrackedDraftState {
                schema_hash: draft.schema_hash,
                root,
            },
        );
    }
    let next_coordinates = raster_core::cfs::CfsCursor::new(program.cfs.clone())
        .try_get_next_coordinates(input.step_record.coordinates())
        .ok_or_else(|| error("Missing sample successor coordinates"))?;
    if next.frontier != expected_frontier
        || next.storage_root != storage.root_after
        || next.storage_index_root != storage.index_root_after
        || root(&next.storage_frontier)? != storage.root_after
        || next.recur_progress.commitment() != input.step_record.recur_progress_commitment
        || next.active_drafts != expected_drafts
        || next.next_expected_coordinates != next_coordinates
        || next.actual_fingerprint_acc.fingerprint() != &expected_fingerprint
        || result.program_commitment != program.commitment().to_vec()
        || result.transition_image_id != transition_image_bytes()
        || result.authorization_image_id != authorization_guest_image_id()
        || result.input_manifest_commitment != authorization.journal.input_manifest_commitment
        || result.refuted_trace_commitment != previous.refuted_trace_commitment
        || result.window_is_terminal
        || journal_bytes(&result.init_state)? != journal_bytes(&previous.init_state)?
        || result.entrypoint_authorization != previous.entrypoint_authorization
        || result.output_authorization != previous.output_authorization
    {
        return Err(error(
            "Transition profiling journal does not match expected state",
        ));
    }
    Ok(session.cycles())
}

fn continuation(
    program: &ProgramDefinition,
    context: &TransitionProfileContext,
    input: &TransitionInput,
) -> Result<(
    TransitionState,
    TransitionJournal,
    SerializableFrontier,
    raster_core::fingerprint::Fingerprint,
)> {
    let config = FraudProofConfig::from_window_size(CONTEXT_WINDOW_SIZE).map_err(error)?;
    let mut acc = FingerprintAccumulator::new(BitPacker::new(config.bits_per_item));
    acc.append(&root(&context.frontier)?);
    let mut frontier = serializable_frontier_into_trace_frontier(context.frontier.clone())
        .ok_or_else(|| error("Invalid sample trace frontier"))?;
    frontier.append(Bytes(input.step_record.try_hash().map_err(error)?));
    let expected_frontier = serializable_frontier_from_trace_frontier(frontier);
    let mut window = acc.clone();
    window.append(&root(&expected_frontier)?);
    let expected_fingerprint = window.clone().into_fingerprint();
    // Only the second slot is compared. The other slots model payload shape;
    // this fingerprint is never published as an actual trace commitment.
    for _ in 2..CONTEXT_WINDOW_SIZE {
        window.append(&[0; 32]);
    }
    let mut drafts = BTreeMap::new();
    if let Some(draft) = input
        .replay_journal
        .as_ref()
        .and_then(|journal| journal.draft_transition.as_ref())
    {
        drafts.insert(
            draft.draft_id,
            TrackedDraftState {
                schema_hash: draft.schema_hash,
                root: draft.root_before,
            },
        );
    }
    let transition = Transition {
        frontier: context.frontier.clone(),
        storage_frontier: context.storage_frontier.clone(),
        storage_root: context.storage_root.clone(),
        storage_index_root: context.storage_index_root.clone(),
        active_drafts: drafts.clone(),
        actual_fingerprint_acc: acc,
        next_expected_coordinates: vec![input.step_record.coordinates.clone()],
        recur_progress: context.recur_progress.clone(),
    };
    let state = TransitionState::Next(transition);
    let cursor = raster_core::cfs::CfsCursor::new(program.cfs.clone());
    let previous = TransitionJournal {
        init_state: InitTransition {
            init_frontier: context.frontier.clone(),
            init_storage_frontier: context.storage_frontier.clone(),
            init_storage_root: context.storage_root.clone(),
            init_storage_index_root: context.storage_index_root.clone(),
            active_drafts: drafts,
            fingerprint: window.into_fingerprint(),
        },
        current_state: state.clone(),
        transition_image_id: transition_image_bytes(),
        authorization_image_id: authorization_guest_image_id(),
        input_manifest_commitment: input
            .authorization_journal
            .input_manifest_commitment
            .clone(),
        program_commitment: program.commitment().to_vec(),
        refuted_trace_commitment: Sha256::digest(b"raster/profile/representative-continuation/v1")
            .to_vec(),
        entrypoint_authorization: if program.manifest.inputs.is_empty() {
            EntrypointAuthorization::NotRequired
        } else {
            EntrypointAuthorization::Established
        },
        output_authorization: if cursor.main_produces_output() {
            OutputAuthorization::Pending
        } else {
            OutputAuthorization::NotRequired
        },
        window_is_terminal: false,
    };
    Ok((state, previous, expected_frontier, expected_fingerprint))
}
