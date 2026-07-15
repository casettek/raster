//! Verifies `main`'s entry-argument binding against the authorization
//! journal.
//!
//! `main`'s declared external arguments are bound to a single internal-store
//! object whose commitment is a struct-of-commitments root over each
//! argument's individually-authorized commitment (see
//! `EntrypointOp::BindEntryArguments`). That binding is checked in exactly
//! two places:
//!
//! - [`verify_step`]: the ordinary per-step check, when this step's own
//!   `StepRecord` is the `Entrypoint` step that performs the binding.
//! - [`verify_genesis_authorization`]: once per fraud-proof chain, when a
//!   window's `TransitionState::Init` did *not* itself replay the binding
//!   step — a trace-inclusion witness proves the window's initial
//!   internal-store state already contains the same authorized binding at
//!   coordinates `[0]`, so the fact doesn't need to be re-derived at every
//!   step (see `fraud_proof::FraudProofWindowContext::proceed`).

use raster_core::authorization::AuthorizationJournal;
use raster_core::cfs::{CfsCoordinates, CfsCursor};
use raster_core::trace::{EntrypointOp, EntrypointRecord};
use raster_core::transition::InternalStoreReadWitness;

use crate::checks::store::verify_internal_store_read_witness;
use crate::merkle_tree::sha256_bytes;

/// Recompute the struct-of-commitments root over `main`'s declared entry
/// arguments, in CFS declaration order, from each argument's individually
/// authorized commitment. Mirrors the `TreeValue::Struct` hash convention
/// (`b"struct"` domain tag + child root hashes, concatenated) so a
/// selection into a specific argument composes as one ordinary selection
/// proof, not a special case.
pub fn combined_root(names: &[String], authorization_journal: &AuthorizationJournal) -> Vec<u8> {
    let commitments: Vec<&Vec<u8>> = names
        .iter()
        .map(|name| {
            authorization_journal
                .external_inputs_commitments
                .get(name)
                .unwrap_or_else(|| {
                    panic!("Missing authorized commitment for entry argument '{}'", name)
                })
        })
        .collect();

    let mut buf = Vec::with_capacity(b"struct".len() + commitments.iter().map(|c| c.len()).sum::<usize>());
    buf.extend_from_slice(b"struct");
    for commitment in &commitments {
        buf.extend_from_slice(commitment);
    }
    sha256_bytes(&buf)
}

/// Per-step check: this step's `StepRecord::Entrypoint` really commits to
/// the authorized combined root.
pub fn verify_step(record: &EntrypointRecord, authorization_journal: &AuthorizationJournal) {
    let EntrypointOp::BindEntryArguments { names } = &record.op;
    let expected = combined_root(names, authorization_journal);
    assert_eq!(
        record.output_commitment, expected,
        "Entrypoint binding does not match the authorized entry-argument commitments",
    );
}

/// Genesis check: establish (or refute) `entrypoint_authorized` for a fresh
/// fraud-proof chain. Returns `true` when the CFS declares no `main` entry
/// arguments at all (vacuously authorized — there is nothing to bind), and
/// otherwise requires `membership_witness` to prove coordinates `[0]` of
/// the window's initial internal-store state commits to the authorized
/// combined root.
pub fn verify_genesis_authorization(
    cfs_cursor: &CfsCursor,
    init_internal_store_root: &[u8],
    init_internal_store_index_root: &[u8],
    authorization_journal: &AuthorizationJournal,
    membership_witness: Option<&InternalStoreReadWitness>,
) -> bool {
    let Some(names) = cfs_cursor.main_entrypoint_names() else {
        assert!(
            membership_witness.is_none(),
            "CFS declares no main entry arguments; entrypoint membership witness must not be provided",
        );
        return true;
    };

    let witness = membership_witness.unwrap_or_else(|| {
        panic!("Missing entrypoint membership witness for a CFS that declares main entry arguments")
    });
    let expected = combined_root(names, authorization_journal);
    verify_internal_store_read_witness(
        witness,
        init_internal_store_root,
        init_internal_store_index_root,
        &CfsCoordinates(vec![0]),
        &expected,
    );
    true
}
