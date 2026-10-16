//! TEMP(fraud-probe): verify a single-program `*.fraud-proof` receipt.
//!
//! usage: verify_fraud_proof <receipt.fraud-proof> <refuted commit.bin>

use raster_core::trace::TraceCommitment;
use raster_core::transition::{TransitionJournal, TransitionState};
use raster_prover::trace::TraceCommitmentExt;
use raster_prover::TRANSITION_GUEST_ID;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let receipt_bytes = std::fs::read(&args[1]).expect("read receipt");
    let commit_bytes = std::fs::read(&args[2]).expect("read commit.bin");

    let receipt: risc0_zkvm::Receipt = postcard::from_bytes(&receipt_bytes).expect("decode receipt");
    println!("receipt kind: {}", match &receipt.inner {
        risc0_zkvm::InnerReceipt::Composite(_) => "composite",
        risc0_zkvm::InnerReceipt::Succinct(_) => "succinct",
        risc0_zkvm::InnerReceipt::Groth16(_) => "groth16",
        risc0_zkvm::InnerReceipt::Fake(_) => "FAKE (dev mode)",
        _ => "other",
    });

    match receipt.verify(TRANSITION_GUEST_ID) {
        Ok(()) => println!("receipt verifies against transition image id: OK"),
        Err(error) => {
            println!("receipt verification FAILED: {error}");
            std::process::exit(1);
        }
    }

    let journal: TransitionJournal = receipt.journal.decode().expect("decode journal");
    let finished = matches!(journal.current_state, TransitionState::Finished);
    println!("journal state: {}", if finished { "Finished (fraud proven)" } else { "NOT Finished" });

    let commitment: TraceCommitment = postcard::from_bytes(&commit_bytes).expect("decode commit.bin");
    let expected = commitment.header().digest();
    let binds = journal.refuted_trace_commitment == expected;
    println!(
        "refuted commitment: {} ({})",
        hex::encode(&journal.refuted_trace_commitment),
        if binds { "matches the given commit.bin" } else { "DOES NOT match the given commit.bin" }
    );
    println!("program commitment: {}", hex::encode(&journal.program_commitment));
    println!("transition image id in journal matches: {}", {
        let id: Vec<u8> = TRANSITION_GUEST_ID.iter().flat_map(|w| w.to_le_bytes()).collect();
        id == journal.transition_image_id
    });

    if !(finished && binds) {
        std::process::exit(1);
    }
}
