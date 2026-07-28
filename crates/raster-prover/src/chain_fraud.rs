//! Host driver for the chain-fraud aggregation guest.
//!
//! `prove_chain_fraud` runs the guest over a `ChainFraudInput` (with the
//! stage-level evidence receipt as an assumption); `verify_chain_fraud_receipt`
//! performs the relying party's checks — receipt against the pinned
//! chain-fraud image id, then the committed inner image ids against the
//! known-good transition/authorization guests. See
//! `docs/proposals/chain-fraud-proof.md`.

use raster_core::chain::{ChainFaultKind, ChainFraudInput, ChainFraudJournal};

use crate::authorization::authorization_guest_image_id;
use crate::error::{BitPackerError, Result};
use crate::{CHAIN_FRAUD_GUEST_ELF, CHAIN_FRAUD_GUEST_ID, TRANSITION_GUEST_ID};

fn image_id_bytes(image_id: [u32; 8]) -> Vec<u8> {
    image_id
        .into_iter()
        .flat_map(|val| val.to_le_bytes())
        .collect()
}

/// The transition guest's image id, as the byte encoding journals carry.
pub fn transition_guest_image_id() -> Vec<u8> {
    image_id_bytes(TRANSITION_GUEST_ID)
}

/// Prove a chain fraud: verify the stage-level evidence receipt in-guest and
/// bind it to the named chain checkpoint. `evidence_receipt` must be the
/// receipt whose journal `input.evidence` carries (the stage fraud receipt
/// for `Execution`, the authorization receipt for `Link`).
pub fn prove_chain_fraud(
    input: &ChainFraudInput,
    evidence_receipt: risc0_zkvm::Receipt,
) -> risc0_zkvm::Receipt {
    let prover = risc0_zkvm::default_prover();
    let env = risc0_zkvm::ExecutorEnv::builder()
        .add_assumption(evidence_receipt)
        .write(input)
        .expect("chain fraud input is serializable")
        .build()
        .expect("chain fraud executor env");
    prover
        .prove(env, CHAIN_FRAUD_GUEST_ELF)
        .expect("chain fraud proving failed")
        .receipt
}

/// The relying party's verification: the receipt is a chain-fraud receipt
/// (pinned image id), and the inner image ids its journal commits are the
/// known-good guests — without this last step a receipt could have verified
/// its evidence against an attacker-compiled guest that commits arbitrary
/// journals naming its own image id.
pub fn verify_chain_fraud_receipt(receipt: &risc0_zkvm::Receipt) -> Result<ChainFraudJournal> {
    receipt
        .verify(CHAIN_FRAUD_GUEST_ID)
        .map_err(|e| BitPackerError::InvalidCommitment(format!("chain-fraud receipt: {e}")))?;
    let journal: ChainFraudJournal = receipt
        .journal
        .decode()
        .map_err(|e| BitPackerError::SerializationError(format!("chain-fraud journal: {e}")))?;

    match journal.fault {
        ChainFaultKind::Execution => {
            if journal.transition_image_id != transition_guest_image_id() {
                return Err(BitPackerError::InvalidCommitment(
                    "chain-fraud receipt verified an unknown transition guest".to_string(),
                ));
            }
        }
        ChainFaultKind::Link => {
            if journal.authorization_image_id != authorization_guest_image_id() {
                return Err(BitPackerError::InvalidCommitment(
                    "chain-fraud receipt verified an unknown authorization guest".to_string(),
                ));
            }
        }
    }

    Ok(journal)
}
