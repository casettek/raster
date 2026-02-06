//! RISC0 Init guest entry point.
//!
//! This guest creates the initial commitment and produces the first chain link
//! for the Transition proving pipeline.

#![no_main]
#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use raster_prover::guest::init;
use raster_prover::guest::types::InitInput;

risc0_zkvm::guest::entry!(main);

fn main() {
    // Read the serialized InitInput from the host
    let input_len: u32 = risc0_zkvm::guest::env::read();
    let mut input_bytes = alloc::vec![0u8; input_len as usize];
    risc0_zkvm::guest::env::read_slice(&mut input_bytes);

    // Deserialize the input
    let input: InitInput = bincode::deserialize(&input_bytes)
        .expect("Failed to deserialize InitInput");

    // Execute the Init guest logic
    let output = init::execute(input);

    // Serialize and commit the output to the journal
    let output_bytes = bincode::serialize(&output)
        .expect("Failed to serialize InitOutput");
    risc0_zkvm::guest::env::commit_slice(&output_bytes);
}
