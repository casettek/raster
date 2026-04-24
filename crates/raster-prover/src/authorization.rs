//! Host-side utilities for proving manifest-backed external input authorization.

use raster_core::authorization::{AuthorizationJournal, ManifestedInputs};

use crate::{AUTHORIZATION_GUEST_ELF, AUTHORIZATION_GUEST_ID};

fn image_id_bytes(image_id: [u32; 8]) -> Vec<u8> {
    image_id
        .into_iter()
        .flat_map(|val| val.to_le_bytes())
        .collect()
}

pub fn authorize_external_inputs(
    manifested_inputs: &ManifestedInputs,
) -> (risc0_zkvm::Receipt, AuthorizationJournal) {
    let prover = risc0_zkvm::default_prover();
    let mut builder = risc0_zkvm::ExecutorEnv::builder();
    builder.write(manifested_inputs).unwrap();
    let env = builder.build().unwrap();

    let receipt = prover.prove(env, &AUTHORIZATION_GUEST_ELF).unwrap().receipt;
    let journal: AuthorizationJournal = receipt.journal.decode().unwrap();

    (receipt, journal)
}

pub fn authorization_guest_image_id() -> Vec<u8> {
    image_id_bytes(AUTHORIZATION_GUEST_ID)
}
