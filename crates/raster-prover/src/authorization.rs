//! Host-side utilities for proving manifest-backed external input authorization.

use raster_core::authorization::{AuthorizationJournal, ManifestedInputs};

use crate::segments::ZkvmSegmentPo2;
use crate::{AUTHORIZATION_GUEST_ELF, AUTHORIZATION_GUEST_ID};

fn image_id_bytes(image_id: [u32; 8]) -> Vec<u8> {
    image_id
        .into_iter()
        .flat_map(|val| val.to_le_bytes())
        .collect()
}

/// The authorization journal for `manifested_inputs`, by executing the guest
/// without proving it: what the TRANSITION part reads before the receipt
/// exists. FRAUD WINDOW holds the proven receipt to it.
pub fn execute_authorization(manifested_inputs: &ManifestedInputs) -> AuthorizationJournal {
    let mut builder = risc0_zkvm::ExecutorEnv::builder();
    builder.write(manifested_inputs).unwrap();
    let env = builder.build().unwrap();
    risc0_zkvm::default_executor()
        .execute(env, AUTHORIZATION_GUEST_ELF)
        .unwrap()
        .journal
        .decode()
        .unwrap()
}

pub fn authorize_external_inputs(
    manifested_inputs: &ManifestedInputs,
    zkvm_segment_po2: ZkvmSegmentPo2,
) -> (risc0_zkvm::Receipt, AuthorizationJournal) {
    let prover = risc0_zkvm::default_prover();
    let mut builder = zkvm_segment_po2.env_builder();
    builder.write(manifested_inputs).unwrap();
    let env = builder.build().unwrap();

    let receipt = prover.prove(env, &AUTHORIZATION_GUEST_ELF).unwrap().receipt;
    let journal: AuthorizationJournal = receipt.journal.decode().unwrap();

    (receipt, journal)
}

pub fn authorization_guest_image_id() -> Vec<u8> {
    image_id_bytes(AUTHORIZATION_GUEST_ID)
}
