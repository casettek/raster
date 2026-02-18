//! Run command: build and execute the user program as a whole.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use raster_backend::ExecutionMode;
use raster_backend_risc0::Risc0Backend;

use raster_compiler::Project;

use raster_core::trace::TraceItem;
use raster_core::trace::TraceWindow;
use raster_core::{Error, Result};

use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::replay::{ReplayResult, Replayer};
use raster_prover::trace::{
    SerializableFrontier, TraceCommitment, TraceVerifier, VerificationResult,
};
use raster_prover::transition::{replay_transitions, TransitionJournal};

use crate::BackendType;

pub fn run(
    backend_type: BackendType,
    input: Option<&str>,
    commit_flag: Option<&str>,
    audit_flag: Option<&str>,
) -> Result<()> {
    if backend_type != BackendType::Native {
        return Err(Error::Other(
            "Only the native backend is supported for running entire programs. \
             Use 'cargo raster run-tile' to execute individual tiles with the RISC0 backend."
                .into(),
        ));
    }

    let project_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let project = Project::new(project_path).expect("Failed to read project");

    println!("Building project...");

    // Build the project with cargo build --release
    let build_status = Command::new("cargo")
        .current_dir(&project.root_dir)
        .args(["build", "--release"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| Error::Other(format!("Failed to run cargo build: {}", e)))?;

    if !build_status.success() {
        return Err(Error::Other("cargo build failed".into()));
    }

    let binary_path = project.target_dir.join("release").join(&project.name);

    if !binary_path.exists() {
        return Err(Error::Other(format!(
            "Binary not found at: {}",
            binary_path.display()
        )));
    }

    let commit_path = commit_flag
        .or(audit_flag)
        .expect("Neither commit nor audit path was provided");

    println!();
    println!("Running {}...", &project.name);
    println!();

    // Build command with optional input argument
    let mut cmd = Command::new(&binary_path);
    cmd.current_dir(&project.root_dir);

    if let Some(input_json) = input {
        cmd.args(["--input", input_json]);
    }

    // Execute the binary and capture output
    let output = cmd
        .output()
        .map_err(|e| Error::Other(format!("Failed to execute binary: {}", e)))?;

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr);

        return Err(Error::Other(format!(
            "Program exited with code {}: {}",
            code, stderr
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    let mut trace_items: Vec<TraceItem> = Vec::new();

    for line in stdout.lines() {
        if let Some(parsed) = line.strip_prefix("[trace]").map(serde_json::from_str) {
            if let Ok(trace_item) = parsed {
                trace_items.push(trace_item);
            }
        }
    }

    if commit_flag.is_some() {
        if commit_path.starts_with("fraud") {
            fraud(&mut trace_items, commit_path)
        } else {
            commit(&trace_items, commit_path);
        }
    } else if audit_flag.is_some() {
        let verification_result = verify(&trace_items, commit_path);

        match verification_result {
            VerificationResult::Ok => println!("Verification Success"),
            VerificationResult::Fraud(fraud_window) => {
                let backend = Risc0Backend::new(project.output_dir.clone())
                    .with_user_crate(project.root_dir.clone());

                let replayer = Replayer::new(&backend, &project);
                prove(fraud_window, &replayer);
            }
        }
    }

    Ok(())
}

pub fn fraud(trace_items: &mut [TraceItem], commit_path: &str) {
    let mut commitment_file =
        std::fs::File::create(commit_path).expect("Failed to create commitemt file");

    if let Some(fraud_trace_item) = trace_items.last_mut() {
        fraud_trace_item.output_data = vec![0u8, 1u8];
    };

    let trace_commitment = TraceCommitment::from(trace_items, &EMPTY_TRIE_NODES[0]);

    let bytes = postcard::to_allocvec(&trace_commitment).unwrap();

    commitment_file
        .write_all(&bytes)
        .expect("Failed to save commitment");
}

pub fn commit(trace_items: &[TraceItem], commit_path: &str) {
    let mut commitment_file =
        std::fs::File::create(commit_path).expect("Failed to create commitemt file");

    let trace_commitment = TraceCommitment::from(trace_items, &EMPTY_TRIE_NODES[0]);
    let bytes = postcard::to_allocvec(&trace_commitment).unwrap();

    commitment_file
        .write_all(&bytes)
        .expect("Failed to save commitment");
}

pub fn verify(trace_items: &[TraceItem], commit_path: &str) -> VerificationResult {
    let trace_commitment = read_trace_commitment(commit_path);

    let actual_trace_commitment = TraceCommitment::from(trace_items, &EMPTY_TRIE_NODES[0]);

    let mut trace_verifier: TraceVerifier =
        TraceVerifier::new(trace_commitment, &EMPTY_TRIE_NODES[0]);

    for trace_item in trace_items {
        if let VerificationResult::Fraud(fraud_window) = trace_verifier.verify(trace_item) {
            println!("verification result: \nfraud: {:?}", fraud_window);
            return VerificationResult::Fraud(fraud_window);
        }
    }

    println!("verification result: ok");
    VerificationResult::Ok
}

pub fn prove(fraud_window: TraceWindow, replayer: &Replayer) {
    let mode = ExecutionMode::prove_and_verify();

    let mut replayed_results: BTreeMap<String, ReplayResult> = BTreeMap::new();

    for (i, item) in fraud_window.items.iter().enumerate() {
        print!("  [{}] {} ... ", i, item.fn_name);

        match replayer.replay(item, mode) {
            Ok(replay_result) => {
                replayed_results.insert(item.fn_name.clone(), replay_result.clone());
            }
            Err(e) => {
                println!("FAILED: {}", e);
            }
        }
    }

    if let Some(frontier) = SerializableFrontier::from_bytes(&fraud_window.frontier) {
        println!();
        println!("Replaying transition frontier with transition guest...");

        let Some(receipt) = replay_transitions(
            &frontier,
            &fraud_window.items,
            fraud_window.fingerprint,
            &replayed_results,
        ) else {
            panic!("Failed to generate fraud proof");
        };

        let final_journal: TransitionJournal = receipt.journal.decode().unwrap();
        println!("{:?}", final_journal);
    }
}

pub fn read_trace_commitment(commit_path: &str) -> TraceCommitment {
    let mut file = std::fs::File::open(commit_path).unwrap_or_else(|e| {
        panic!(
            "Failed to open expected commitment file '{}': {}",
            commit_path, e
        )
    });

    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap_or_else(|e| {
        panic!(
            "Failed to read expected commitment file '{}': {}",
            commit_path, e
        )
    });

    let trace_commitment: TraceCommitment =
        postcard::from_bytes(&bytes).expect("Failed to deserialize Trace Commitment");

    trace_commitment
}
