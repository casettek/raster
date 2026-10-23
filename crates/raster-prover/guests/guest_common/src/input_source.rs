//! The input source anchor: a step's `FnInput` witness is the one its record
//! commits to.

use raster_core::trace::{FnInput, StepRecord};

use crate::merkle_tree::sha256_bytes;

/// `sha256(FnInput::source_witness_bytes())`, with risc0's accelerated SHA. The
/// recorder hashes the same preimage with `sha2`.
pub fn input_source_commitment(input: &FnInput) -> Vec<u8> {
    sha256_bytes(&input.source_witness_bytes())
}

/// Hold a step's input source witness to the record: present and equal to its
/// recorded `input_source_commitment` exactly when the record declares one,
/// absent otherwise.
///
/// Every check that reads the witness — the CFS bindings in the control-flow
/// guest; the replay's input roots, the storage reads and recur in the
/// transition guest — is only as sound as this anchor. Each guest makes it
/// itself rather than rely on the other (`docs/proposals/control-flow-prover.md`
/// steps 3 and 6).
pub fn verify_input_source_anchored(
    step_record: &StepRecord,
    input_source_witness: Option<&FnInput>,
) {
    verify_input_source_commitment_anchored(
        step_record,
        input_source_witness.map(input_source_commitment).as_deref(),
    );
}

/// [`verify_input_source_anchored`] for a caller that has already hashed the
/// witness: `commitment` is `input_source_commitment(witness)`, `None` exactly
/// when the step carries no witness. The transition guest hashes the witness
/// once per step and hands the hash to every check that anchors it.
pub fn verify_input_source_commitment_anchored(
    step_record: &StepRecord,
    commitment: Option<&[u8]>,
) {
    if let Some(expected_input_source_commitment) = step_record.input_source_commitment() {
        let commitment = commitment.expect("Step input source witness is missing");
        assert_eq!(
            expected_input_source_commitment.as_slice(),
            commitment,
            "Step input source witness does not match recorded source commitment",
        );
    } else {
        // `ProgramStart`, `ProgramEnd` and `SequenceEnd` all report `None`
        // here. None of them binds a step input, so a witness supplied for one
        // is unbound by construction — nothing in the record commits to it.
        assert!(
            commitment.is_none(),
            "Step {:?} declares no input source commitment, so it must not carry an input \
             source witness",
            step_record.coordinates,
        );
    }
}
