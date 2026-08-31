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

/// Prove a chain fraud: verify the stage-level evidence in-guest and bind it to
/// the named chain checkpoint.
///
/// `evidence_receipt` is the receipt whose journal `input.evidence` carries —
/// the authorization receipt for `Link`. It is `None` for `Shape`, which
/// verifies no inner receipt at all: everything that fault compares is read out
/// of the `ChainCommitment` bytes the guest hashes for itself, so there is no
/// assumption to discharge. One entry point rather than two, so the two cannot
/// drift on the thing that must not drift — what the guest reads.
pub fn prove_chain_fraud(
    input: &ChainFraudInput,
    evidence_receipt: Option<risc0_zkvm::Receipt>,
) -> risc0_zkvm::Receipt {
    let prover = risc0_zkvm::default_prover();
    let mut builder = risc0_zkvm::ExecutorEnv::builder();
    if let Some(receipt) = evidence_receipt {
        builder.add_assumption(receipt);
    }
    let env = builder
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
        ChainFaultKind::Link => {
            if journal.authorization_image_id != authorization_guest_image_id() {
                return Err(BitPackerError::InvalidCommitment(
                    "chain-fraud receipt verified an unknown authorization guest".to_string(),
                ));
            }
        }
        // A positive assertion, not a fallthrough. A `Shape` receipt verified
        // nothing, so a populated image id would be a claim about a recursion
        // that never happened — and a relying party that pins ids would read it
        // as meaningful. The absence has to be checked to mean anything.
        ChainFaultKind::Shape => {
            if !journal.authorization_image_id.is_empty() || !journal.transition_image_id.is_empty()
            {
                return Err(BitPackerError::InvalidCommitment(
                    "chain-fraud receipt claims a shape fault but names an inner guest; a shape \
                     fault verifies no receipt"
                        .to_string(),
                ));
            }
        }
    }

    Ok(journal)
}
