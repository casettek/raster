//! TEMP(fraud-probe): verify a single-program `*.fraud-proof` receipt.
//!
//! usage: verify_fraud_proof <receipt.fraud-proof> <refuted commit.bin>

use raster_core::trace::TraceCommitment;
use raster_prover::trace::TraceCommitmentExt;
use raster_prover::transition::verify_fraud_proof_receipt;

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

    // The receipt, `Finished`, and the transition and authorization guests the
    // journal names — see `docs/issues/authorization-image-id-unpinned.md`.
    let journal = match verify_fraud_proof_receipt(&receipt) {
        Ok(journal) => {
            println!("receipt verifies; Finished; transition and authorization guests pinned: OK");
            journal
        }
        Err(error) => {
            println!("receipt verification FAILED: {error}");
            std::process::exit(1);
        }
    };

    let commitment: TraceCommitment = postcard::from_bytes(&commit_bytes).expect("decode commit.bin");
    let expected = commitment.header().digest();
    let binds = journal.refuted_trace_commitment == expected;
    println!(
        "refuted commitment: {} ({})",
        hex::encode(&journal.refuted_trace_commitment),
        if binds { "matches the given commit.bin" } else { "DOES NOT match the given commit.bin" }
    );
    println!("program commitment: {}", hex::encode(&journal.program_commitment));

    if !binds {
        std::process::exit(1);
    }
}
