//! Run command: build and execute the user program as a whole.

use crate::BackendType;

use raster_backend::ExecutionMode;
use raster_backend_risc0::Risc0Backend;
use raster_compiler::Project;
use raster_core::fingerprint::BitPacker;
use raster_core::ipc::{self, IpcMessage};
use raster_core::trace::AuditResult;
use raster_core::trace::TraceItem;
use raster_core::{Error, Result};

use raster_prover::replay::{ReplayResult, Replayer};

use raster_prover::trace::{
    BytesHashable, ExecutionCommitment, SerializableFrontier, TraceBridgeTree,
};
use raster_prover::transition::{replay_transitions, TransitionJournal};

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Run the user program with the specified backend.
pub fn run(
    backend_type: BackendType,
    input: Option<&str>,
    commit: Option<&str>,
    audit: Option<&str>,
) -> Result<()> {
    // Only native backend is supported for whole-program execution
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

    println!();
    println!("Running {}...", &project.name);
    println!();

    // Build command with optional input argument
    let mut cmd = Command::new(&binary_path);
    cmd.current_dir(&project.root_dir);

    if let Some(input_json) = input {
        cmd.args(["--input", input_json]);
    }

    if let Some(commit_path) = commit {
        cmd.args(["--commit", commit_path]);
    } else if let Some(audit_path) = audit {
        cmd.args(["--audit", audit_path]);
    }

    // Execute the binary and capture output
    let output = cmd
        .output()
        .map_err(|e| Error::Other(format!("Failed to execute binary: {}", e)))?;

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr);
        println!("stderr: {}", stderr);

        return Err(Error::Other(format!(
            "Program exited with code {}: {}",
            code, stderr
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Separate trace items, audit results, and regular program output
    let mut trace_items: Vec<TraceItem> = Vec::new();
    let mut audit_result: Option<AuditResult> = None;
    let mut program_output: Vec<&str> = Vec::new();

    for line in stdout.lines() {
        match ipc::parse_line(line) {
            IpcMessage::Trace(item) => {
                trace_items.push(item);
            }
            IpcMessage::Audit(result) => {
                audit_result = Some(result);
            }
            IpcMessage::Output(_) | IpcMessage::Unknown(_) => {
                program_output.push(line);
            }
        }
    }

    // Print program output first
    if !program_output.is_empty() {
        println!("Output:");
        for line in &program_output {
            println!("  {}", line);
        }
        println!();
    }

    // Handle audit result if present
    if let Some(result) = audit_result {
        if result.success {
            println!("Audit verification passed",);
        } else {
            println!("Audit verification FAILED!");

            // Display trace window for debugging context
            if !result.trace_window.is_empty() {
                println!();
                println!(
                    "Trace window ({} items leading up to divergence):",
                    result.trace_window.len()
                );
                for (i, item) in result.trace_window.iter().enumerate() {
                    println!();
                    println!("  [{}] {}", i, item.fn_name);
                    if let Some(ref desc) = item.desc {
                        println!("      Description: {}", desc);
                    }
                    if !item.inputs.is_empty() {
                        println!("      Inputs:");
                        for input in &item.inputs {
                            println!("        - {}: {}", input.name, input.ty);
                        }
                    }
                    if let Some(ref output_type) = item.output_type {
                        println!("      Output type: {}", output_type);
                    }
                }

                // Replay trace window with Risc0 backend for proof generation
                println!();
                println!("Replaying trace window with Risc0 backend...");

                let backend =
                // TODO: Restructure backend <-> project relations
                    Risc0Backend::new(project.output_dir.clone()).with_user_crate(project.root_dir.clone());

                let replayer = Replayer::new(&backend, &project);
                let mode = ExecutionMode::prove_and_verify();

                let mut replayed_results: BTreeMap<String, ReplayResult> = BTreeMap::new();

                for (i, item) in result.trace_window.iter().enumerate() {
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

                // Replay transition frontier with the transition guest
                if let Some(ref diff) = result.diff {
                    if let Some(frontier) = SerializableFrontier::from_bytes(&diff.frontier) {
                        println!();
                        println!("Replaying transition frontier with transition guest...");

                        // Parse fingerprint bytes as little-endian u64s
                        let fingerprint: Vec<u64> = diff
                            .fingerprint
                            .chunks_exact(8)
                            .map(|chunk| u64::from_le_bytes(chunk.try_into().unwrap()))
                            .collect();

                        let bits_packer = BitPacker(diff.bits_per_item);
                        let window_fingerprint = bits_packer
                            .get_range(
                                diff.window_start_position,
                                diff.window_start_position + 2,
                                &fingerprint,
                            )
                            .expect("Failed to get window fingerprint");

                        let Some(receipt) = replay_transitions(
                            &frontier,
                            &result.trace_window,
                            window_fingerprint,
                            diff.bits_per_item,
                            &replayed_results,
                        ) else {
                            panic!("Failed to generate fraud proof");
                        };

                        let final_journal: TransitionJournal = receipt.journal.decode().unwrap();
                        println!("{:?}", final_journal);
                    }
                }
            }

            return Err(Error::Other("Audit verification failed".into()));
        }
    }

    Ok(())
}

/// Result of a transition replay operation.
#[derive(Debug)]
pub struct TransitionReplayResult {
    /// Number of transitions successfully proved and verified
    pub verified_count: usize,
    /// If fingerprint verification failed, the index in trace_window where it failed
    pub failed_at_index: Option<usize>,
    /// The type of failure if any
    pub failure_type: Option<TransitionFailureType>,
}

/// Type of transition failure.
#[derive(Debug)]
pub enum TransitionFailureType {
    /// Item hash mismatch
    ItemHashMismatch,
    /// Fingerprint mismatch
    FingerprintMismatch,
    /// Proving error
    ProvingError(String),
}
