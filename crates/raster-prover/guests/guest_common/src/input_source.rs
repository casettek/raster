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
    if let Some(expected_input_source_commitment) = step_record.input_source_commitment() {
        let input_source_witness =
            input_source_witness.expect("Step input source witness is missing");
        assert_eq!(
            expected_input_source_commitment,
            &input_source_commitment(input_source_witness),
            "Step input source witness does not match recorded source commitment",
        );
    } else {
        // `ProgramStart`, `ProgramEnd` and `SequenceEnd` all report `None`
        // here. None of them binds a step input, so a witness supplied for one
        // is unbound by construction — nothing in the record commits to it.
        assert!(
            input_source_witness.is_none(),
            "Step {:?} declares no input source commitment, so it must not carry an input \
             source witness",
            step_record.coordinates,
        );
    }
}
