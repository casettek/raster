//! Verifies the program's `ProgramStart` step against the authorization
//! journal.
//!
//! `main`'s declared external arguments are loaded into a single storage
//! object whose commitment is a struct-of-commitments root over each
//! argument's individually-authorized commitment. That binding is the *only*
//! thing tying execution to the public manifest, so every proof chain must
//! establish it exactly once, by one of two routes:
//!
//! - [`verify_step`]: the window's trace contains the `ProgramStart` step
//!   itself (the trace's first step), so the binding is verified directly as
//!   it is replayed.
//! - [`verify_genesis_authorization`]: the window opened *after* the start,
//!   so a trace-inclusion witness proves the window's initial storage state
//!   already contains the binding at coordinates `[]`.
//!
//! Which route applies is not the host's choice to declare — the guest
//! derives it from the CFS and the supplied witness. Because `ProgramStart`
//! is always the first step, authorization is `Established` before any later
//! step runs (see `fraud_proof::FraudProofWindowContext`), so there is no
//! deferred debt to discharge at the end of the chain.

use raster_core::authorization::AuthorizationJournal;
use raster_core::cfs::{CfsCoordinates, CfsCursor};
use raster_core::input::{struct_commitments_root, verify_selection_witness, SelectionWitness};
use raster_core::trace::{ProgramEndStep, ProgramStartStep, StepKind, StepRecord};
use raster_core::transition::{EntrypointAuthorization, OutputAuthorization, StorageReadWitness};

use crate::checks::store::verify_storage_read_witness;

/// The coordinate `main`'s entry-argument binding always occupies: the
/// sequence root itself. The `ProgramStart` step loads the combined entry
/// object there (see the trace recorder), and every consuming
/// `InputBinding::EntryArgument` reaches it from there.
fn entrypoint_coordinates() -> CfsCoordinates {
    CfsCoordinates(vec![])
}

/// Recompute the struct-of-commitments root over `main`'s declared entry
/// arguments, in CFS declaration order, from each argument's individually
/// authorized commitment.
///
/// `names` must come from the CFS, never from a claimed record: the
/// authorization journal authorizes every name the public manifest
/// declares, so a root recomputed over a caller-chosen subset would verify
/// just as well as one over the declared set. Reuses the shared
/// `TreeValue::Struct` convention, so selecting into one argument composes
/// as one ordinary selection proof rather than a special case.
pub fn combined_root(names: &[String], authorization_journal: &AuthorizationJournal) -> Vec<u8> {
    let fields: Vec<(&str, &[u8])> = names
        .iter()
        .map(|name| {
            let commitment = authorization_journal
                .external_inputs_commitments
                .get(name)
                .unwrap_or_else(|| {
                    panic!("Missing authorized commitment for entry argument '{}'", name)
                });
            (name.as_str(), commitment.as_slice())
        })
        .collect();

    struct_commitments_root(fields.iter().copied()).to_vec()
}

/// Per-step check: the `ProgramStart` step binds exactly the arguments the
/// CFS declares, to exactly the commitments the authorization journal
/// authorizes, at the sequence root coordinate `[]`. Returns the
/// authorization state the chain reaches by having verified it.
pub fn verify_step(
    cfs_cursor: &CfsCursor,
    record: &StepRecord,
    program_start: &ProgramStartStep,
    authorization_journal: &AuthorizationJournal,
) -> EntrypointAuthorization {
    assert_eq!(
        record.coordinates,
        entrypoint_coordinates(),
        "ProgramStart step must bind at the sequence root coordinate",
    );

    let Some(declared_names) = cfs_cursor.main_entrypoint_names() else {
        // A program that declares no entry arguments still emits a
        // `ProgramStart` step; it must bind nothing.
        assert!(
            program_start.entry_arguments.is_empty(),
            "ProgramStart step binds entry arguments the CFS does not declare",
        );
        assert!(
            program_start.output_commitment.is_empty(),
            "ProgramStart step with no entry arguments must make no binding",
        );
        return EntrypointAuthorization::NotRequired;
    };

    assert_eq!(
        program_start.entry_arguments.as_slice(),
        declared_names,
        "ProgramStart step binds different entry arguments than the CFS declares",
    );

    let expected = combined_root(declared_names, authorization_journal);
    assert_eq!(
        program_start.output_commitment, expected,
        "ProgramStart binding does not match the authorized entry-argument commitments",
    );

    EntrypointAuthorization::Established
}

/// Genesis check: decide the chain's entry-argument authorization when the
/// window opens.
///
/// - CFS declares no entry arguments: `NotRequired` (and no witness may be
///   supplied, since there is nothing it could prove).
/// - Witness supplied: it must prove coordinates `[]` of the window's
///   initial storage state commits to the authorized combined root,
///   yielding `Established`.
/// - No witness: the binding is not yet in the initial storage state, so
///   this window must open at genesis with `ProgramStart` as its first step
///   — which [`verify_step`] then authorizes against the journal in the same
///   guest run. `Established`.
pub fn verify_genesis_authorization(
    cfs_cursor: &CfsCursor,
    init_storage_root: &[u8],
    init_storage_index_root: &[u8],
    authorization_journal: &AuthorizationJournal,
    membership_witness: Option<&StorageReadWitness>,
    first_step: &StepRecord,
) -> EntrypointAuthorization {
    let Some(names) = cfs_cursor.main_entrypoint_names() else {
        assert!(
            membership_witness.is_none(),
            "CFS declares no main entry arguments; entrypoint membership witness must not be provided",
        );
        return EntrypointAuthorization::NotRequired;
    };

    match membership_witness {
        Some(witness) => {
            let expected = combined_root(names, authorization_journal);
            verify_storage_read_witness(
                witness,
                init_storage_root,
                init_storage_index_root,
                &entrypoint_coordinates(),
                &expected,
            );
            EntrypointAuthorization::Established
        }
        None => {
            assert!(
                matches!(first_step.kind, StepKind::ProgramStart(_)),
                "Window opens with entry arguments unbound but its first step is not ProgramStart; \
                 supply an entrypoint membership witness for a window that opens after the program start",
            );
            EntrypointAuthorization::Established
        }
    }
}

/// Per-step check for the program's terminal `ProgramEnd` step: the committed
/// program output provably lives in committed storage. Returns the output
/// authorization the chain reaches by having verified it.
///
/// `main`'s returned value is a selection out of a committed storage object
/// (a verified tile's output): the read proves the object is present at its
/// coordinates in the current storage state, the selection proves it narrows
/// to the returned value, and `output_commitment` is pinned to that
/// selection's `selected_hash`. A unit program binds nothing. This reuses the
/// exact storage-read and selection machinery that verifies tile inputs.
pub fn verify_program_end(
    cfs_cursor: &CfsCursor,
    record: &StepRecord,
    program_end: &ProgramEndStep,
    current_storage_root: &[u8],
    current_storage_index_root: &[u8],
    read_witness: Option<&StorageReadWitness>,
    selection_witness: Option<&SelectionWitness>,
) -> OutputAuthorization {
    assert_eq!(
        record.coordinates,
        entrypoint_coordinates(),
        "ProgramEnd step must sit at the sequence root coordinate",
    );

    let produces_output = cfs_cursor.main_produces_output();

    let Some(output) = &program_end.output else {
        assert!(
            !produces_output,
            "CFS declares a program output but ProgramEnd binds none",
        );
        assert!(
            program_end.output_commitment.is_empty(),
            "ProgramEnd with no output must make no output commitment",
        );
        assert!(
            read_witness.is_none() && selection_witness.is_none(),
            "ProgramEnd with no output must carry no output witnesses",
        );
        return OutputAuthorization::NotRequired;
    };

    assert!(
        produces_output,
        "ProgramEnd binds an output the CFS does not declare",
    );

    // The output object is present at its coordinates in the current store.
    let read_witness = read_witness.expect("ProgramEnd output requires a storage read witness");
    verify_storage_read_witness(
        read_witness,
        current_storage_root,
        current_storage_index_root,
        &output.coordinates,
        &output.commitment,
    );

    // The selection narrows the committed object to the returned value.
    assert_eq!(
        output.commitment.as_slice(),
        output.selection.source_root_hash.as_slice(),
        "Program output object commitment must match the selection source root",
    );
    if output.selection.selected_len > 0 {
        let selection_witness =
            selection_witness.expect("ProgramEnd output requires a selection witness");
        assert!(
            verify_selection_witness(&output.selection, selection_witness),
            "Program output selection witness is invalid",
        );
    }

    // The committed output is exactly that selection's value.
    assert_eq!(
        program_end.output_commitment.as_slice(),
        output.selection.selected_hash.as_slice(),
        "ProgramEnd output commitment does not match the selected output hash",
    );

    OutputAuthorization::Established
}
