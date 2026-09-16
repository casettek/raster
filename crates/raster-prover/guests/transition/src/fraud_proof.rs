//! The transition state machine.
//!
//! A fraud-proof window is proven one step per guest execution:
//!
//! 1. [`FraudProofWindowContext::establish`] attaches the step to the chain — either a
//!    genesis state (`Init`) or a recursively verified previous journal
//!    (`Next`) — and yields the [`LiveTransition`] to advance.
//! 2. [`LiveTransition::apply_verified_step`] verifies every recorded aspect
//!    of the step and folds it into the live state.
//! 3. [`LiveTransition::finalize`] compares the accumulated fingerprint with
//!    the committed window fingerprint and decides `Next` vs `Finished`.
//! 4. [`commit_journal`] commits the resulting [`TransitionJournal`].

use std::collections::BTreeMap;

use bridgetree::NonEmptyFrontier;
use risc0_zkvm::guest::env;

use raster_core::cfs::{CfsCoordinates, CfsCursor, SequenceChildItem};
use raster_core::coordinate_index::coordinate_index_root;
use raster_core::draft::{DraftId, TrackedDraftState};
use raster_core::fingerprint::{Fingerprint, FingerprintAccumulator};
use raster_core::program::{commitment_of_bytes, ImageId, ProgramDefinition};
use raster_core::recur_progress::RecurProgressStack;
use raster_core::trace::{ExecStep, ExecTarget, StepKind, StepRecord};
use raster_core::transition::{
    EntrypointAuthorization, FingerprintSliceWitness, InitTransition, OutputAuthorization,
    TraceCommitmentHeader, Transition, TransitionInput, TransitionJournal, TransitionState,
};

use crate::checks;
use crate::merkle_tree::{
    combine_merkle_level, deserialize_frontier, frontier_root, hash_trace_item, serialize_frontier,
    sha256_bytes, Bytes, EMPTY_LEAF,
};

/// Public parameters every step of the fraud proof runs under.
pub struct PublicParams {
    /// The program being proven, decoded from its `program.bin` frame. Its
    /// tile registry is the source of truth for each tile step's expected
    /// image id (see `docs/proposals/program-identity.md`).
    pub program: ProgramDefinition,
    /// `sha256(domain || program.bin)` — derived from the exact frame bytes
    /// this guest verifies against, committed to the journal, and asserted
    /// continuous across the window.
    pub program_commitment: Vec<u8>,
    pub cfs_cursor: CfsCursor,
    pub transition_image_id: Vec<u8>,
}

impl PublicParams {
    /// Reads the leading host inputs. The host write order is: the program
    /// definition frame (`program.bin` bytes), transition image id, transition
    /// input, transition state, and — only for `Next` steps — the previous
    /// journal (read in [`FraudProofWindowContext`]).
    ///
    /// The frame is hashed *before* decoding, so the committed
    /// `program_commitment` is bound to the exact bytes verified against — a
    /// host cannot name one program and verify against another.
    pub fn read() -> Self {
        let program_bytes: Vec<u8> = env::read();
        let program_commitment = commitment_of_bytes(&program_bytes).to_vec();
        let program = ProgramDefinition::decode(&program_bytes)
            .expect("host must supply a valid ProgramDefinition frame");
        let transition_image_id: Vec<u8> = env::read();
        let cfs_cursor = CfsCursor::new(program.cfs.clone());
        Self {
            program,
            program_commitment,
            cfs_cursor,
            transition_image_id,
        }
    }
}

/// The registry image id for a tile step, or `None` for a step that carries no
/// replay proof. Panics if a tile step's id is absent from the program
/// registry — an unregistered tile cannot be proven.
fn expected_tile_image_id<'a>(
    program: &'a ProgramDefinition,
    step_record: &StepRecord,
) -> Option<&'a ImageId> {
    match &step_record.kind {
        StepKind::Exec(ExecStep {
            target: ExecTarget::Tile(name),
            ..
        }) => Some(
            program
                .tile_image_id(name)
                .unwrap_or_else(|| panic!("tile '{name}' has no image id in the program registry")),
        ),
        _ => None,
    }
}

/// Where this step attaches, and the facts the window carries across its steps.
///
/// There is deliberately no "is this the first step" flag. Every window item is
/// compared against the committed fingerprint under one rule — **every item
/// before the last must match, and the last must diverge** — so the opening
/// step needs no special case. It used to have one, committed without any
/// comparison, which weakened every window by an item of margin and made a
/// one-item window unprovable in principle: its only item was the one never
/// examined. That was exactly the head of a trace.
pub struct FraudProofWindowContext {
    pub init_state: InitTransition,
    /// `TraceCommitmentHeader::digest()` of the `commit.bin` this window
    /// audits — derived at `Init` (after the slice check below), inherited
    /// from the recursively verified previous journal at `Next`.
    pub refuted_trace_commitment: Vec<u8>,
    /// Whether this window ends where that commitment ends. Derived at `Init`
    /// by the slice check, inherited at `Next`. See
    /// `TransitionJournal::window_is_terminal`.
    pub window_is_terminal: bool,
    /// The committed trace root for this window's final index, when it falls in
    /// the revealed tail. Derived at `Init`, inherited at `Next`, and read only
    /// by the terminal step. See `TransitionJournal::final_committed_root`.
    pub final_committed_root: Option<Vec<u8>>,
}

/// Refuse a window that opens inside a live recur site with no seed, naming
/// the cause.
///
/// **Diagnostics only, no soundness weight.** The advance-and-compare in
/// [`checks::cfs::advance_recur_progress`] remains the sole authority on
/// whether a seed is correct — a wrong seed advances to a different stack and
/// fails there, seeded or not.
///
/// What this buys is the error message. Without it, a missing seed surfaces as
/// a recur-progress commitment mismatch, which reads like a soundness
/// violation in the trace and points a reader at the guest rather than at the
/// host that failed to reconstruct the seed. That misreading is exactly what
/// let an unfilled parameter ship for weeks as a de-facto "refuse to open
/// mid-loop" rule — the design `recur-progress-commitment.md` §Problem had
/// explicitly rejected. See `window-seed-reconstruction.md` §Uncertainty 3.
///
/// Best-effort by construction: it fires on a step whose coordinates sit under
/// a recur site, and stays silent where the CFS cannot resolve them.
fn assert_seed_present_for_mid_loop_open(
    cfs_cursor: &CfsCursor,
    step_record: &StepRecord,
    seed: Option<&RecurProgressStack>,
) {
    if let Some(seed) = seed {
        if !seed.is_empty() {
            return;
        }
    }

    let coordinates = step_record.coordinates();
    // A *strict* prefix naming a recur site means this step executes inside
    // one. The full coordinate is excluded on purpose: a window opening on the
    // site's own `Start` opens before the frame is pushed, so the empty stack
    // is the true state there.
    let opens_inside_recur_site = (1..coordinates.len()).any(|depth| {
        matches!(
            cfs_cursor.try_get_item(&CfsCoordinates(coordinates[..depth].to_vec())),
            Some(SequenceChildItem::RecurTile(_)) | Some(SequenceChildItem::RecurSequence(_))
        )
    });

    assert!(
        !opens_inside_recur_site,
        "Window opens at {:?}, which executes inside a live recur site, but no \
         recur-progress seed was supplied. The host must reconstruct it from the \
         trace prefix (`TraceRecorder::recur_progress_after`); an empty stack here \
         is the positive claim \"no loop in flight\", which these coordinates \
         contradict.",
        coordinates
    );
}

impl FraudProofWindowContext {
    /// Attach the step to the fraud proof window context.
    ///
    /// - `Init`: start from the genesis state carried in the transition,
    ///   and independently decide what the chain owes for entry-argument
    ///   authorization — the guest never trusts a host-supplied claim about
    ///   the window's initial storage contents. The window's committed
    ///   fingerprint is bound to a named `commit.bin` here: the supplied
    ///   header is hashed into `refuted_trace_commitment` only after the
    ///   slice witness proves the window fingerprint occurs in that
    ///   commitment at the offset the initial frontier fixes.
    /// - `Next`: read the previous journal, recursively verify its receipt
    ///   against our own image id, and require state and manifest
    ///   continuity. Entry-argument authorization and the refuted-commitment
    ///   identity are inherited from the previous (recursively verified)
    ///   journal.
    pub fn proceed(
        params: &PublicParams,
        input: &TransitionInput,
        state: TransitionState,
        commitment_binding: Option<(TraceCommitmentHeader, FingerprintSliceWitness)>,
    ) -> (Self, LiveTransition) {
        match state {
            TransitionState::Init(init_transition) => {
                let (commitment_header, slice_witness) = commitment_binding
                    .expect("Init step requires the trace-commitment header and slice witness");
                let window_binding = assert_window_is_commitment_slice(
                    &init_transition,
                    &commitment_header,
                    &slice_witness,
                    input.revealed_tail_roots.as_ref(),
                );
                let refuted_trace_commitment = commitment_header.digest();
                let entrypoint_authorization = checks::entrypoint::verify_genesis_authorization(
                    &params.cfs_cursor,
                    &init_transition.init_storage_root,
                    &init_transition.init_storage_index_root,
                    &input.authorization_journal,
                    input.entrypoint_membership_witness.as_ref(),
                    &input.step_record,
                );
                // The output is bound by the trace's last step, `ProgramEnd`,
                // which no window can open after — so a fresh chain owes it
                // (`Pending`) until that step is verified, with no genesis
                // witness route.
                let output_authorization = if params.cfs_cursor.main_produces_output() {
                    OutputAuthorization::Pending
                } else {
                    OutputAuthorization::NotRequired
                };
                let live = LiveTransition::genesis(
                    &init_transition,
                    entrypoint_authorization,
                    output_authorization,
                )
                .seed_recur_progress(input.window_start_recur_progress.as_ref());
                assert_seed_present_for_mid_loop_open(
                    &params.cfs_cursor,
                    &input.step_record,
                    input.window_start_recur_progress.as_ref(),
                );
                (
                    Self {
                        init_state: init_transition,
                        refuted_trace_commitment,
                        window_is_terminal: window_binding.window_is_terminal,
                        final_committed_root: window_binding.final_committed_root,
                    },
                    live,
                )
            }
            TransitionState::Next(transition) => {
                let prev_journal: TransitionJournal = env::read();
                verify_previous_journal(&prev_journal, &params.transition_image_id);
                assert_state_continuity(&prev_journal, &transition);
                assert_manifest_continuity(&prev_journal, input);
                assert_program_continuity(&prev_journal, params);

                let live = LiveTransition::resume(
                    &transition,
                    prev_journal.entrypoint_authorization,
                    prev_journal.output_authorization,
                );
                (
                    Self {
                        init_state: prev_journal.init_state,
                        refuted_trace_commitment: prev_journal.refuted_trace_commitment,
                        window_is_terminal: prev_journal.window_is_terminal,
                        final_committed_root: prev_journal.final_committed_root,
                    },
                    live,
                )
            }
            TransitionState::Finished => {
                panic!("Finished Transition");
            }
        }
    }
}

/// Prove the window's committed fingerprint is a slice of the named
/// commitment — closing the framing gap where a challenger supplies a
/// "committed" window fingerprint that appears nowhere in the `commit.bin`
/// the receipt ends up blamed on.
///
/// The offset is never host-claimed: the initial trace frontier holds the
/// seed plus one leaf per pre-window step, so its position *is* the window's
/// start index `s`. The witness must then cover exactly the packed blocks
/// spanning items `[s, s + w)`, each proven against the header's
/// `fingerprint_root` at its derived index, and every window item's
/// fingerprint value must equal the value at its bit offset inside those
/// proven blocks.
///
/// Returns whether the window is **terminal** in that commitment — it ends
/// exactly where the committed fingerprint ends. Derived here because this is
/// the one place holding `s`, `w` and `header.fingerprint_len` at once; the
/// header is dropped when `proceed` returns, and `apply_verified_step` never
/// sees it. See `TransitionJournal::window_is_terminal`.
/// Hold a window that opens at trace index 0 to the genesis state.
///
/// Every other window carries a *margin*: the steps before the divergence must
/// reproduce the committed fingerprint, and that is the only thing tying the
/// challenger-supplied opening state to reality. A window opening at index 0
/// cannot have one — there are no steps before it — which is why the head of a
/// trace looked unprovable.
///
/// It does not need one. At index 0 the opening state is not the challenger's
/// to choose: the trace tree holds only the seed, which is the same public
/// constant for every program (`EMPTY_TRIE_NODES[0]` host-side, `EMPTY_LEAF`
/// here), storage is empty, and no draft or loop is in flight. Asserting that
/// removes the need for the margin rather than excusing its absence.
///
/// What the seed does *not* say is which program ran with which inputs — the
/// seed is program-independent. That comes from the window's first step being
/// `ProgramStart`, whose `output_commitment` must equal the combined root over
/// the authorization journal's entry-argument commitments
/// (`checks::entrypoint::verify_step`), and whose structural fields are pinned
/// by `verify_exec_index` and `verify_sequence_id`. Together those make the
/// record unique, which is what a one-item window needs.
fn assert_opens_at_genesis(init_transition: &InitTransition) {
    let frontier = &init_transition.init_frontier;
    assert!(
        frontier.position == 0 && frontier.leaf == EMPTY_LEAF && frontier.ommers.is_empty(),
        "A window opening at trace index 0 must open on the seed leaf alone",
    );

    let storage = &init_transition.init_storage_frontier;
    assert!(
        storage.position == 0 && storage.leaf == EMPTY_LEAF && storage.ommers.is_empty(),
        "A window opening at trace index 0 must open on empty storage",
    );
    assert!(
        init_transition.init_storage_index_root == coordinate_index_root(&BTreeMap::new()),
        "A window opening at trace index 0 must open on an empty coordinate index",
    );
    assert!(
        init_transition.active_drafts.is_empty(),
        "A window opening at trace index 0 cannot have a draft in flight",
    );
}

pub(crate) struct WindowBinding {
    /// Whether this window ends exactly where the commitment's fingerprint
    /// ends. See `TransitionJournal::window_is_terminal`.
    pub window_is_terminal: bool,
    /// The committed trace root for this window's final index, when that index
    /// falls inside the revealed tail. See
    /// `TransitionJournal::final_committed_root`.
    pub final_committed_root: Option<Vec<u8>>,
}

pub(crate) fn assert_window_is_commitment_slice(
    init_transition: &InitTransition,
    header: &TraceCommitmentHeader,
    slice_witness: &FingerprintSliceWitness,
    revealed_tail_roots: Option<&Vec<Vec<u8>>>,
) -> WindowBinding {
    let window_fingerprint = &init_transition.fingerprint;
    assert!(
        window_fingerprint.bits_packer == header.bits_packer,
        "Window fingerprint bit packing does not match the committed fingerprint's"
    );
    let bits_per_item = header.bits_packer.bits_per_item();
    let window_len = window_fingerprint.len();
    assert!(window_len > 0, "Window fingerprint is empty");

    // The window's *shape*, which nothing constrained before the header carried
    // `window_size`. `window_len` comes from the challenger's `Fingerprint::len`
    // — a metadata field `Fingerprint::from` stores verbatim without checking it
    // against `bits` — and `window_start` from the challenger's frontier
    // position, leaving only the upper bound below to hold them.
    //
    // A short window is the degenerate case rather than a merely unusual one:
    // counting what `finalize` compares over `L` items, item 0 is `First` and
    // never compared, items `1..L-2` must match, and item `L-1` must diverge.
    // At `L = 2` that is *zero* matching comparisons, so nothing ties the
    // challenger's opening state to reality and a window fabricated anywhere in
    // the trace reaches `Finished`.
    let declared_window_size =
        usize::try_from(header.window_size).expect("Window size overflows usize");

    let window_start = usize::try_from(init_transition.init_frontier.position)
        .expect("Window start position overflows usize");

    if window_len != declared_window_size {
        // A short window is legal in exactly one place, and it is not a
        // concession: a divergence inside the trace's first `window_size` steps
        // has no room for a full window behind it, and the rolling buffer
        // correctly yields `min(i + 1, w)` items starting at 0. So the honest
        // host can produce a short window *only* at trace index 0.
        assert!(
            window_start == 0 && window_len < declared_window_size,
            "Window declares {} items against a commitment built with a window of {}, and \
             only a window opening at trace index 0 may be short",
            window_len,
            declared_window_size,
        );
        // And there it needs no pre-divergence margin. The margin exists to
        // pin a *challenger-supplied* opening state to reality by replaying
        // forward from it; at index 0 the opening state is not supplied at all,
        // it is the public genesis constant. Assert that directly and the
        // missing margin stops mattering rather than being tolerated.
        assert_opens_at_genesis(init_transition);
    }
    let fingerprint_len =
        usize::try_from(header.fingerprint_len).expect("Fingerprint length overflows usize");
    assert!(
        window_start + window_len <= fingerprint_len,
        "Window range exceeds the committed fingerprint"
    );

    // The covering block range is derived, not supplied; a witness for any
    // other range fails here.
    let first_block = (window_start * bits_per_item) / 64;
    let last_block = ((window_start + window_len) * bits_per_item - 1) / 64;
    assert!(
        slice_witness.blocks.len() == last_block - first_block + 1,
        "Fingerprint slice witness does not cover exactly the window's blocks"
    );

    let mut proven_blocks: Vec<u64> = Vec::with_capacity(slice_witness.blocks.len());
    for (offset, block_witness) in slice_witness.blocks.iter().enumerate() {
        let expected_position = (first_block + offset) as u64;
        assert!(
            block_witness.position == expected_position,
            "Fingerprint block witness is at the wrong index"
        );
        let mut current = sha256_bytes(&block_witness.block.to_le_bytes());
        for (level, sibling) in block_witness.path_elems.iter().enumerate() {
            current = if ((block_witness.position >> level) & 1) == 0 {
                combine_merkle_level(level, &current, sibling)
            } else {
                combine_merkle_level(level, sibling, &current)
            };
        }
        assert!(
            current == header.fingerprint_root,
            "Fingerprint block inclusion proof is invalid"
        );
        proven_blocks.push(block_witness.block);
    }

    for item in 0..window_len {
        let bit_offset = (window_start + item) * bits_per_item - first_block * 64;
        let committed_value = header
            .bits_packer
            .try_get_at_bit_offset(bit_offset, &proven_blocks)
            .expect("Window item exceeds the proven fingerprint blocks");
        let window_value = window_fingerprint
            .bits_packer
            .try_get(item, &window_fingerprint.bits)
            .expect("Window fingerprint is shorter than its declared length");
        assert!(
            committed_value == window_value,
            "Window fingerprint diverges from the committed fingerprint slice"
        );
    }

    // The revealed tail, if the host supplied it. Binding is unconditional
    // once supplied; whether it is *useful* depends on where this window ends.
    let final_committed_root = revealed_tail_roots.and_then(|roots| {
        let roots_bytes =
            postcard::to_allocvec(roots).expect("revealed tail roots are serializable");
        assert!(
            sha256_bytes(&roots_bytes) == header.revealed_tail_roots_commitment,
            "Revealed tail roots do not match the commitment's tail-roots commitment",
        );
        assert!(
            roots.len() == declared_window_size,
            "Commitment reveals {} tail roots but declares a window of {}",
            roots.len(),
            declared_window_size,
        );

        // The tail covers the fingerprint's final `window_size` indices.
        let tail_start = fingerprint_len - roots.len();
        let final_index = window_start + window_len - 1;
        final_index
            .checked_sub(tail_start)
            .and_then(|offset| roots.get(offset))
            .cloned()
    });

    WindowBinding {
        // `<=` was asserted above; equality is the terminal case.
        window_is_terminal: window_start + window_len == fingerprint_len,
        final_committed_root,
    }
}

/// Recursively verify the previous transition receipt for this same guest.
fn verify_previous_journal(prev_journal: &TransitionJournal, transition_image_id: &[u8]) {
    let transition_image_id_digest =
        risc0_zkvm::sha::Digest::try_from(transition_image_id).expect("image_id must be 32 bytes");
    env::verify(
        transition_image_id_digest,
        &risc0_zkvm::serde::to_vec(prev_journal).unwrap(),
    )
    .expect("Failed to verify previous transition journal");
    assert!(
        transition_image_id == prev_journal.transition_image_id,
        "The transition image ID is not the same within the fraud proof"
    );
}

/// The state we resume from must be exactly the previous journal's output.
fn assert_state_continuity(prev_journal: &TransitionJournal, transition: &Transition) {
    let TransitionState::Next(prev_transition) = &prev_journal.current_state else {
        panic!("Provided Transition state does not align to fraud proof state");
    };
    assert!(
        prev_transition == transition,
        "Transition mismatch: the provided transition does not align with the fraud proof"
    );
}

/// Every step of the fraud proof must be authorized against the same manifest.
fn assert_manifest_continuity(prev_journal: &TransitionJournal, input: &TransitionInput) {
    assert!(
        input.authorization_journal.input_manifest_commitment
            == prev_journal.input_manifest_commitment,
        "Manifest commitment does not match"
    );
}

/// Every step of the fraud proof must prove execution of the same program —
/// the frame this step verifies against must hash to the previous journal's
/// committed program identity.
fn assert_program_continuity(prev_journal: &TransitionJournal, params: &PublicParams) {
    assert!(
        params.program_commitment == prev_journal.program_commitment,
        "Program commitment does not match across the fraud-proof window"
    );
}

/// The deserialized, in-progress twin of [`Transition`]: the state advanced
/// by applying one verified step.
pub struct LiveTransition {
    frontier: NonEmptyFrontier<Bytes>,
    storage_frontier: NonEmptyFrontier<Bytes>,
    storage_index_root: Vec<u8>,
    active_drafts: BTreeMap<DraftId, TrackedDraftState>,
    fingerprint_acc: FingerprintAccumulator,
    /// `None` only for the genesis state, where no coordinates are expected yet.
    next_expected_coordinates: Option<Vec<CfsCoordinates>>,
    /// How far this chain has got in tying `main`'s entry arguments to the
    /// authorization journal. Advances at most once, `Pending` ->
    /// `Established`, when an `Entrypoint` step is verified in this window.
    entrypoint_authorization: EntrypointAuthorization,
    /// How far this chain has got in tying the program's output to committed
    /// storage. Advances `Pending` -> `Established` when the `ProgramEnd` step
    /// is verified in this window.
    output_authorization: OutputAuthorization,
    /// Live recur sites. Seeded at a fresh `Init` from host-supplied state and
    /// validated by *advancing* it, never by believing it.
    recur_progress: RecurProgressStack,
}

impl LiveTransition {
    /// Genesis state for the first step of the window.
    fn genesis(
        init_transition: &InitTransition,
        entrypoint_authorization: EntrypointAuthorization,
        output_authorization: OutputAuthorization,
    ) -> Self {
        let frontier = deserialize_frontier(&init_transition.init_frontier)
            .expect("Invalid frontier in input");
        let storage_frontier = deserialize_frontier(&init_transition.init_storage_frontier)
            .expect("Invalid storage frontier in input");
        assert_eq!(
            frontier_root(&storage_frontier),
            init_transition.init_storage_root,
            "Initial storage root does not match initial storage frontier",
        );

        Self {
            frontier,
            storage_frontier,
            storage_index_root: init_transition.init_storage_index_root.clone(),
            active_drafts: init_transition.active_drafts.clone(),
            fingerprint_acc: FingerprintAccumulator::new(init_transition.fingerprint.bits_packer),
            next_expected_coordinates: None,
            entrypoint_authorization,
            output_authorization,
            // `InitTransition` carries no recur progress by design; a window
            // opening mid-loop supplies its seed separately, and that seed is
            // only ever accepted by reproducing the first step's own recorded
            // commitment. Absent means "no loop in flight" — itself a claim the
            // same check rejects if false.
            recur_progress: RecurProgressStack::new(),
        }
    }

    /// Resume from the state carried over from the previous (verified) step.
    fn resume(
        transition: &Transition,
        entrypoint_authorization: EntrypointAuthorization,
        output_authorization: OutputAuthorization,
    ) -> Self {
        let frontier =
            deserialize_frontier(&transition.frontier).expect("Invalid frontier in input");
        let storage_frontier = deserialize_frontier(&transition.storage_frontier)
            .expect("Invalid storage frontier in input");
        assert_eq!(
            frontier_root(&storage_frontier),
            transition.storage_root,
            "Transition storage root does not match transition storage frontier",
        );

        Self {
            frontier,
            storage_frontier,
            storage_index_root: transition.storage_index_root.clone(),
            active_drafts: transition.active_drafts.clone(),
            fingerprint_acc: transition.actual_fingerprint_acc.clone(),
            next_expected_coordinates: Some(transition.next_expected_coordinates.clone()),
            entrypoint_authorization,
            output_authorization,
            recur_progress: transition.recur_progress.clone(),
        }
    }

    /// Seed a fresh window's recur progress. Not trust: the very next
    /// `advance_recur_progress` must reproduce the step's recorded commitment.
    fn seed_recur_progress(mut self, seed: Option<&RecurProgressStack>) -> Self {
        if let Some(seed) = seed {
            self.recur_progress = seed.clone();
        }
        self
    }

    /// What this chain has established so far — read by `main` to commit the
    /// journal the next step inherits.
    pub fn entrypoint_authorization(&self) -> EntrypointAuthorization {
        self.entrypoint_authorization
    }

    pub fn output_authorization(&self) -> OutputAuthorization {
        self.output_authorization.clone()
    }

    /// Verify every recorded aspect of one step and advance the state:
    ///
    /// - the step's inputs obey the CFS bindings at its coordinates,
    /// - a `ProgramStart` step binds exactly the CFS-declared entry arguments
    ///   to their authorized commitments,
    /// - recorded IO commitments match the witnesses, and tile steps carry
    ///   a verified replay proof,
    /// - the storage transition is consistent with the recorded roots,
    /// - the step's coordinates are among the expected next coordinates,
    /// - the draft chain stays continuous,
    /// - the step record is appended to the trace frontier and fingerprint.
    pub fn apply_verified_step(
        mut self,
        program: &ProgramDefinition,
        cfs_cursor: &CfsCursor,
        input: &TransitionInput,
    ) -> Self {
        // Before `append_to_trace` advances it, the frontier's position is this
        // step's trace index — which is what fixes its `exec_index`.
        checks::cfs::verify_exec_index(self.frontier.position().into(), &input.step_record);
        checks::cfs::verify_sequence_id(cfs_cursor, &input.step_record);
        // The same pre-append frontier: its root is the root of the trace
        // prefix the parent record must be provably in.
        checks::cfs::verify_sequence_scope_parent(
            cfs_cursor,
            &input.step_record,
            input.sequence_scope_witness.as_ref(),
            &input.input_sources_witnesses,
            &frontier_root(&self.frontier),
        );
        checks::cfs::verify_step_record_inputs(
            cfs_cursor,
            &input.step_record,
            input.input_source_witness.as_ref(),
            input.sequence_scope_witness.as_ref(),
            input.replay_journal.as_ref(),
        );
        checks::cfs::advance_recur_progress(
            cfs_cursor,
            &mut self.recur_progress,
            &input.step_record,
            input.replay_journal.as_ref(),
            input.input_source_witness.as_ref(),
            input.output_witness.as_ref(),
            &input.storage_selection_witnesses,
        );
        if let StepKind::ProgramStart(program_start) = &input.step_record.kind {
            self.entrypoint_authorization = checks::entrypoint::verify_step(
                cfs_cursor,
                &input.step_record,
                program_start,
                &input.authorization_journal,
            );
        }
        if let StepKind::ProgramEnd(program_end) = &input.step_record.kind {
            // The output object lives in the current storage state (the
            // frontier reflects every prior step's writes; `ProgramEnd` adds
            // none), so verify the read against the current roots.
            let current_storage_root = frontier_root(&self.storage_frontier);
            self.output_authorization = checks::entrypoint::verify_program_end(
                cfs_cursor,
                &input.step_record,
                program_end,
                &current_storage_root,
                &self.storage_index_root,
                input.program_output_read_witness.as_ref(),
                input.program_output_selection_witness.as_ref(),
            );
        }
        checks::io::verify_step_record(
            &input.step_record,
            expected_tile_image_id(program, &input.step_record),
            input.replay_journal.as_ref(),
            input.input_witness.as_ref(),
            input.output_witness.as_ref(),
            input.input_source_witness.as_ref(),
        );
        let (_, _, next_index_root) = checks::store::verify_storage_transition(
            &input.step_record,
            input.input_source_witness.as_ref(),
            &input.storage_selection_witnesses,
            input.output_witness.as_ref(),
            input.storage_witness.as_ref(),
            &mut self.storage_frontier,
            &self.storage_index_root,
        );
        self.storage_index_root = next_index_root;
        // `get_next_expected_coordinates` both checks this step was expected
        // and computes the successors. `ProgramEnd` is the unique terminal
        // step: it must be expected, but nothing may follow it.
        let next_coordinates = checks::cfs::get_next_expected_coordinates(
            cfs_cursor,
            &input.step_record,
            self.next_expected_coordinates.as_ref(),
        );
        self.next_expected_coordinates = Some(
            if matches!(input.step_record.kind, StepKind::ProgramEnd(_)) {
                Vec::new()
            } else {
                next_coordinates
            },
        );
        checks::drafts::verify_draft_transition(
            &input.step_record,
            input.replay_journal.as_ref(),
            input.draft_transition_witness.as_ref(),
            &mut self.active_drafts,
        );
        self.append_to_trace(&input.step_record);
        self
    }

    /// Append the step record hash to the trace frontier and accumulate the
    /// resulting root into the actual fingerprint.
    fn append_to_trace(&mut self, step_record: &StepRecord) {
        let item_hash = hash_trace_item(step_record);
        self.frontier.append(Bytes(item_hash));
        let tree_root = frontier_root(&self.frontier);
        self.fingerprint_acc.append(&tree_root);
    }

    /// Decide the next machine state against the committed window fingerprint.
    ///
    /// The first window step is committed without comparison. Every later
    /// step must match the committed fingerprint at its index — except the
    /// final window item, which must diverge: that divergence is the fraud
    /// being proven, and it transitions the machine to `Finished`.
    ///
    /// Entry-argument authorization needs no deadline here: it is
    /// `Established` (or `NotRequired`) from the moment the window opens, so
    /// there is never an unauthorized chain able to reach `Finished`.
    pub fn finalize(
        self,
        committed_fingerprint: &Fingerprint,
        final_committed_root: Option<&Vec<u8>>,
    ) -> TransitionState {
        let actual_fingerprint: Fingerprint = self.fingerprint_acc.clone().into_fingerprint();
        let last_index = actual_fingerprint.len() - 1;
        let diverges = actual_fingerprint.diff_at_index(last_index, committed_fingerprint);

        // The window's own length says whether this is the last item: the
        // accumulator has one entry per step applied so far.
        if actual_fingerprint.len() == committed_fingerprint.len() {
            // The fingerprint keeps only `bits_per_item` bits of each trace
            // root, so a divergence it cannot see is a divergence that cannot
            // be proven this way. Inside the revealed tail the full root is
            // available and settles the question exactly: at
            // `window_size >= 128` — one bit per item — the packed entry agrees
            // with an honest run half the time even when the roots differ.
            //
            // The root is only ever an *additional* route to the same
            // conclusion. Root equality implies entry equality, so a window
            // that diverges on bits diverges on roots too; this widens what is
            // provable, never what is accepted as a match.
            let diverges_by_root = final_committed_root
                .is_some_and(|committed_root| frontier_root(&self.frontier) != *committed_root);
            assert!(
                diverges || diverges_by_root,
                "A fraud proof's final step must diverge from the commitment, by fingerprint \
                 entry or by revealed trace root",
            );
            TransitionState::Finished
        } else {
            assert!(
                !diverges,
                "Every window item before the last must match the commitment; a divergence \
                 here is not the one this window claims to prove",
            );
            let mut transition = self.into_transition();
            transition.actual_fingerprint_acc = FingerprintAccumulator::from(actual_fingerprint);
            TransitionState::Next(transition)
        }
    }

    /// Fold the live state back into the serializable [`Transition`].
    fn into_transition(self) -> Transition {
        Transition {
            frontier: serialize_frontier(&self.frontier),
            storage_frontier: serialize_frontier(&self.storage_frontier),
            storage_root: frontier_root(&self.storage_frontier),
            storage_index_root: self.storage_index_root,
            active_drafts: self.active_drafts,
            actual_fingerprint_acc: self.fingerprint_acc,
            next_expected_coordinates: self
                .next_expected_coordinates
                .expect("Step must produce next expected coordinates"),
            recur_progress: self.recur_progress,
        }
    }
}

/// Commit the step's journal: the window's init state, the advanced state,
/// and the image ids / manifest commitment the chain is verified against.
pub fn commit_journal(
    init_state: InitTransition,
    current_state: TransitionState,
    transition_image_id: Vec<u8>,
    program_commitment: Vec<u8>,
    refuted_trace_commitment: Vec<u8>,
    window_is_terminal: bool,
    final_committed_root: Option<Vec<u8>>,
    input: &TransitionInput,
    entrypoint_authorization: EntrypointAuthorization,
    output_authorization: OutputAuthorization,
) {
    let journal = TransitionJournal {
        init_state,
        current_state,
        transition_image_id,
        authorization_image_id: input.authorization_image_id.clone(),
        input_manifest_commitment: input
            .authorization_journal
            .input_manifest_commitment
            .clone(),
        program_commitment,
        refuted_trace_commitment,
        entrypoint_authorization,
        output_authorization,
        window_is_terminal,
        final_committed_root,
    };

    env::commit(&journal);
}
