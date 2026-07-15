//! Verifies `main`'s entry-argument binding against the authorization
//! journal.
//!
//! `main`'s declared external arguments are loaded into a single storage
//! object whose commitment is a struct-of-commitments root over each
//! argument's individually-authorized commitment (see
//! `EntrypointOp::BindEntryArguments`). Because that binding is the *only*
//! thing tying execution to the public manifest, every proof chain must
//! establish it exactly once, by one of two routes:
//!
//! - [`verify_step`]: the window's trace contains the `Entrypoint` step
//!   itself, so the binding is verified directly as it is replayed.
//! - [`verify_genesis_authorization`]: the window opened *after* the
//!   binding, so a trace-inclusion witness proves the window's initial
//!   storage state already contains it at coordinates `[0]`.
//!
//! Which route applies is not the host's choice to declare — the guest
//! derives it from the CFS and the supplied witness, and
//! `EntrypointAuthorization` carries the outcome across the chain so a
//! window that opens at genesis (`Pending`) cannot finish without a
//! discharging `Entrypoint` step. See `fraud_proof::FraudProofWindowContext`.

use raster_core::authorization::AuthorizationJournal;
use raster_core::cfs::{CfsCoordinates, CfsCursor};
use raster_core::input::struct_commitments_root;
use raster_core::trace::{EntrypointOp, EntrypointStep, StepRecord};
use raster_core::transition::{EntrypointAuthorization, StorageReadWitness};

use crate::checks::store::verify_storage_read_witness;

/// The coordinate `main`'s entry-argument binding always occupies: the
/// leading item of `main`'s item list (see `CfsBuilder`, which prepends the
/// `Entrypoint` item, and `FlowResolver::resolve_with_entry_arguments`,
/// which binds every declared name to item index 0).
fn entrypoint_coordinates() -> CfsCoordinates {
    CfsCoordinates(vec![0])
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

/// Per-step check: this step's `StepRecord::Entrypoint` binds exactly the
/// arguments the CFS declares, to exactly the commitments the authorization
/// journal authorizes. Returns the state the chain reaches by having
/// verified it.
pub fn verify_step(
    cfs_cursor: &CfsCursor,
    record: &StepRecord,
    entrypoint: &EntrypointStep,
    authorization_journal: &AuthorizationJournal,
) -> EntrypointAuthorization {
    let declared_names = cfs_cursor.main_entrypoint_names().unwrap_or_else(|| {
        panic!("Entrypoint step recorded for a CFS that declares no main entry arguments")
    });
    let EntrypointOp::BindEntryArguments { names } = &entrypoint.op;
    assert_eq!(
        names.as_slice(),
        declared_names,
        "Entrypoint step binds different entry arguments than the CFS declares",
    );
    assert_eq!(
        record.coordinates,
        entrypoint_coordinates(),
        "Entrypoint step must bind at main's leading item coordinate",
    );

    let expected = combined_root(declared_names, authorization_journal);
    assert_eq!(
        entrypoint.output_commitment, expected,
        "Entrypoint binding does not match the authorized entry-argument commitments",
    );

    EntrypointAuthorization::Established
}

/// Genesis check: decide what a fresh proof chain starts out owing.
///
/// - CFS declares no entry arguments: `NotRequired` (and no witness may be
///   supplied, since there is nothing it could prove).
/// - No witness supplied: `Pending` — the window opens at or before the
///   binding, and an `Entrypoint` step inside it must discharge this.
/// - Witness supplied: it must prove coordinates `[0]` of the window's
///   initial storage state commits to the authorized combined root,
///   yielding `Established`.
pub fn verify_genesis_authorization(
    cfs_cursor: &CfsCursor,
    init_storage_root: &[u8],
    init_storage_index_root: &[u8],
    authorization_journal: &AuthorizationJournal,
    membership_witness: Option<&StorageReadWitness>,
) -> EntrypointAuthorization {
    let Some(names) = cfs_cursor.main_entrypoint_names() else {
        assert!(
            membership_witness.is_none(),
            "CFS declares no main entry arguments; entrypoint membership witness must not be provided",
        );
        return EntrypointAuthorization::NotRequired;
    };

    let Some(witness) = membership_witness else {
        return EntrypointAuthorization::Pending;
    };

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
