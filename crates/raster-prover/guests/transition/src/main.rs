//! RISC0 Transition guest entry point.
//!
//! This guest verifies a Replay proof, compares output, updates the Merkle tree,
//! validates against the fingerprint, and produces the next chain link.

#![no_main]
#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use raster_prover::guest::transition::TransitionContext;
use raster_prover::guest::types::TransitionInput;

risc0_zkvm::guest::entry!(main);

fn main() {
    // Read the serialized TransitionInput from the host
    let input_len: u32 = risc0_zkvm::guest::env::read();
    let mut input_bytes = alloc::vec![0u8; input_len as usize];
    risc0_zkvm::guest::env::read_slice(&mut input_bytes);

    // Deserialize the input
    let input: TransitionInput = bincode::deserialize(&input_bytes)
        .expect("Failed to deserialize TransitionInput");

    // Create the Transition context
    let context = TransitionContext::new(input);

    // Execute the Transition logic with RISC0's verify function
    let output = context
        .execute(|receipt_bytes| {
            // Deserialize the receipt
            let receipt: risc0_zkvm::Receipt = bincode::deserialize(receipt_bytes)
                .map_err(|_| ())?;

            // Verify the receipt using RISC0's guest verification
            // This proves that the Replay guest executed correctly
            risc0_zkvm::guest::env::verify(receipt.inner.claim()?.digest(), &receipt.journal.bytes)
                .map_err(|_| ())?;

            // Extract image ID from the receipt's claim
            let claim = receipt.inner.claim().map_err(|_| ())?;
            let image_id_digest = claim.pre.digest();
            let mut image_id = [0u8; 32];
            image_id.copy_from_slice(image_id_digest.as_bytes());

            // Return the image ID and journal bytes
            Ok((image_id, receipt.journal.bytes.clone()))
        })
        .expect("Transition execution failed");

    // Serialize and commit the output to the journal
    let output_bytes = bincode::serialize(&output).expect("Failed to serialize TransitionOutput");
    risc0_zkvm::guest::env::commit_slice(&output_bytes);
}
