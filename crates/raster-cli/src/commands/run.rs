//! Run command: build and execute the user program as a whole.

use crate::BackendType;
use raster_backend::ExecutionMode;
use raster_backend_risc0::Risc0Backend;
use raster_compiler::Project;
use raster_core::ipc::{self, IpcMessage};
use raster_core::trace::{AuditResult, TraceItem};
use raster_core::{Error, Result};
use raster_prover::guest::{TransitionInput, TransitionOutput, TRANSITION_GUEST_ELF};
use raster_prover::trace::{
    BytesHashable, ExecutionCommitment, SerializableFrontier, TraceBridgeTree,
};
use raster_tracing::TraceReplayer;
use risc0_zkvm::{default_prover, ExecutorEnv};
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

    // Extract binary name from Cargo.toml
    let binary_name = extract_binary_name(&project_path)
        .ok_or_else(|| Error::Other("Could not determine binary name from Cargo.toml".into()))?;

    println!("Building project...");

    // Build the project with cargo build --release
    let build_status = Command::new("cargo")
        .current_dir(&project_path)
        .args(["build", "--release"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| Error::Other(format!("Failed to run cargo build: {}", e)))?;

    if !build_status.success() {
        return Err(Error::Other("cargo build failed".into()));
    }

    // Find the target directory using cargo metadata
    let target_dir = find_target_path(&project_path).unwrap_or_else(|| project_path.join("target"));
    let binary_path = target_dir.join("release").join(&binary_name);

    if !binary_path.exists() {
        return Err(Error::Other(format!(
            "Binary not found at: {}",
            binary_path.display()
        )));
    }

    println!();
    println!("Running {}...", binary_name);
    println!();

    // Build command with optional input argument
    let mut cmd = Command::new(&binary_path);
    cmd.current_dir(&project_path);

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

    let execution_commitment = ExecutionCommitment::from(
        &trace_items,
        &raster_prover::precomputed::EMPTY_TRIE_NODES[0],
    );

    // TEMP: add item SHA's
    // Print trace items as pretty JSON
    if !trace_items.is_empty() {
        println!("Trace ({} tile executions):", trace_items.len());
        for (trace_item, item_commitement) in trace_items.iter().zip(execution_commitment.0.iter())
        {
            if let Ok(pretty) = serde_json::to_string_pretty(&trace_item) {
                // Indent each line of the pretty JSON
                for line in pretty.lines() {
                    println!("  {}", line);
                }
            }
            let item_commitment_hex: String = item_commitement
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect();
            println!("Item commitemt: {:02X?}", item_commitment_hex);
            println!();
        }
    }

    // Handle audit result if present
    if let Some(result) = audit_result {
        if result.success {
            println!("Audit verification passed",);
        } else {
            println!("Audit verification FAILED!");
            if let Some(ref diff) = result.diff {
                println!("  Divergence detected at trace index: {}", diff.index);
                if !diff.frontier.is_empty() {
                    // Display frontier as hex for debugging/replay purposes
                    let frontier_hex: String =
                        diff.frontier.iter().map(|b| format!("{:02x}", b)).collect();
                    let ser_frontier = SerializableFrontier::from_bytes(&diff.frontier);
                    let frontier = ser_frontier
                        .expect("Can't deserialized frontier")
                        .to_frontier()
                        .expect("Can't reconstruct frontier");
                    let mut tree = TraceBridgeTree::from_frontier(1, frontier);
                    tree.append(raster_prover::trace::Bytes(trace_items[diff.index].hash()));
                    let Some(root) = tree.root(0) else {
                        panic!("Can't get tree root");
                    };
                    let root_hex: String = root.0.iter().map(|b| format!("{:02x}", b)).collect();

                    println!("  Root (hex): {}", root_hex);
                    println!("  Frontier (hex): {}", frontier_hex);
                }
            }
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
                    println!("      Input data (base64): {}", item.input_data);
                    println!("      Output data (base64): {}", item.output_data);
                }

                // Replay trace window with Risc0 backend for proof generation
                println!();
                println!("Replaying trace window with Risc0 backend...");

                let project = Project::new(project_path.clone())?;
                let output_dir = project_path.join("target").join("raster");
                let backend = Risc0Backend::new(output_dir).with_user_crate(project_path.clone());
                let replayer = TraceReplayer::new(&backend, &project);
                let mode = ExecutionMode::prove_and_verify();

                for (i, item) in result.trace_window.iter().enumerate() {
                    print!("  [{}] {} ... ", i, item.fn_name);
                    match replayer.replay(item, mode) {
                        Ok(replay_result) => {
                            if let Some(verified) = replay_result.execution_result.verified {
                                if verified {
                                    println!("PROVED (verified)");
                                } else {
                                    println!("PROVED (unverified)");
                                }
                            } else {
                                println!("OK");
                            }
                            if let Some(output_matched) = replay_result.output_matched {
                                if output_matched {
                                    println!("      Output matches recorded trace");
                                } else {
                                    println!("      WARNING: Output differs from recorded trace!");
                                }
                            }
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

                        let replay_result = replay_transitions(
                            &frontier,
                            &result.trace_window,
                            fingerprint,
                            diff.window_start_position,
                            diff.bits_per_item,
                        );

                        if replay_result.failed_at_index.is_none() {
                            println!(
                                    "All {} transitions proved successfully with fingerprint verification.",
                                    result.trace_window.len()
                                );
                        } else if let Some(failed_idx) = replay_result.failed_at_index {
                            println!();
                            println!("Transition replay failed at index {}.", failed_idx);
                            println!(
                                "  Transitions verified before failure: {}",
                                replay_result.verified_count
                            );

                            // Show the exact trace item where fingerprint diverged
                            if let Some(failed_item) = result.trace_window.get(failed_idx) {
                                println!();
                                println!(">>> DIVERGENT TRACE ITEM (index {}):", failed_idx);
                                println!("    Function: {}", failed_item.fn_name);
                                if let Some(ref desc) = failed_item.desc {
                                    println!("    Description: {}", desc);
                                }
                                if !failed_item.inputs.is_empty() {
                                    println!("    Inputs:");
                                    for inp in &failed_item.inputs {
                                        println!("      - {}: {}", inp.name, inp.ty);
                                    }
                                }
                                if let Some(ref output_type) = failed_item.output_type {
                                    println!("    Output type: {}", output_type);
                                }
                                println!("    Input data (base64): {}", failed_item.input_data);
                                println!("    Output data (base64): {}", failed_item.output_data);

                                // Show failure type
                                if let Some(ref failure_type) = replay_result.failure_type {
                                    println!();
                                    match failure_type {
                                        TransitionFailureType::FingerprintMismatch => {
                                            println!(
                                                "    Failure: Fingerprint mismatch at position {}",
                                                diff.window_start_position + failed_idx
                                            );
                                        }
                                        TransitionFailureType::ItemHashMismatch => {
                                            println!("    Failure: Item hash mismatch");
                                        }
                                        TransitionFailureType::ProvingError(msg) => {
                                            println!("    Failure: Proving error - {}", msg);
                                        }
                                    }
                                }
                            }

                            // Show remaining items from the window
                            let remaining_start = failed_idx + 1;
                            if remaining_start < result.trace_window.len() {
                                println!();
                                println!(
                                    ">>> REMAINING TRACE ITEMS ({} items not processed):",
                                    result.trace_window.len() - remaining_start
                                );
                                for (offset, item) in
                                    result.trace_window[remaining_start..].iter().enumerate()
                                {
                                    let idx = remaining_start + offset;
                                    println!();
                                    println!("    [{}] {}", idx, item.fn_name);
                                    if let Some(ref desc) = item.desc {
                                        println!("        Description: {}", desc);
                                    }
                                    if !item.inputs.is_empty() {
                                        println!("        Inputs:");
                                        for inp in &item.inputs {
                                            println!("          - {}: {}", inp.name, inp.ty);
                                        }
                                    }
                                    if let Some(ref output_type) = item.output_type {
                                        println!("        Output type: {}", output_type);
                                    }
                                }
                            } else {
                                println!();
                                println!(
                                    ">>> No remaining trace items (failure occurred at last item)."
                                );
                            }
                        }
                    }
                }
            }

            return Err(Error::Other("Audit verification failed".into()));
        }
    }

    Ok(())
}

/// Find the Cargo target directory for a project.
/// Handles both workspace members and standalone projects.
fn find_target_path(project_path: &std::path::Path) -> Option<PathBuf> {
    // Run cargo metadata to get the target directory
    let output = Command::new("cargo")
        .current_dir(project_path)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let meta: serde_json::Value = serde_json::from_str(&stdout).ok()?;

    meta.get("target_directory")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
}

/// Extract the binary name from a Cargo.toml file.
fn extract_binary_name(project_path: &std::path::Path) -> Option<String> {
    let cargo_toml = std::fs::read_to_string(project_path.join("Cargo.toml")).ok()?;

    // Simple parsing: look for name = "..." in [package] section
    let mut in_package = false;
    for line in cargo_toml.lines() {
        let trimmed = line.trim();
        if trimmed == "[package]" {
            in_package = true;
            continue;
        }
        if trimmed.starts_with('[') {
            in_package = false;
            continue;
        }
        if in_package && trimmed.starts_with("name") {
            if let Some(start) = line.find('"') {
                let rest = &line[start + 1..];
                if let Some(end) = rest.find('"') {
                    return Some(rest[..end].to_string());
                }
            }
        }
    }
    None
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

/// Replay trace transitions using the transition guest to prove merkle tree state transitions.
///
/// For each trace item in the window:
/// 1. Create a TransitionInput with the current frontier, trace item, and fingerprint data
/// 2. Execute the transition guest in the RISC0 zkVM
/// 3. Verify the output (including fingerprint) and update the frontier for the next iteration
///
/// # Arguments
/// * `initial_frontier` - The frontier state before the first trace item
/// * `trace_window` - The trace items to replay
/// * `fingerprint` - The packed fingerprint u64s for verification
/// * `window_start_position` - The starting position in the fingerprint for the first item
/// * `bits_per_item` - Bits per fingerprint item
///
/// # Returns
/// A `TransitionReplayResult` with details about success or failure
fn replay_transitions(
    initial_frontier: &SerializableFrontier,
    trace_window: &[raster_core::trace::TraceItem],
    fingerprint: Vec<u64>,
    window_start_position: usize,
    bits_per_item: usize,
) -> TransitionReplayResult {
    // Load the transition guest ELF from raster-prover
    let elf = TRANSITION_GUEST_ELF;

    let mut current_frontier = initial_frontier.clone();
    let prover = default_prover();

    for (i, item) in trace_window.iter().enumerate() {
        print!("  [{}] transition {} ... ", i, item.fn_name);

        // Create the input for this transition with fingerprint data
        let input = TransitionInput::new(
            current_frontier.clone(),
            item.clone(),
            fingerprint.clone(),
            window_start_position + i,
            bits_per_item,
        );

        // Build the executor environment
        let env = match ExecutorEnv::builder().write(&input) {
            Ok(builder) => match builder.build() {
                Ok(env) => env,
                Err(e) => {
                    println!("FAILED: {}", e);
                    return TransitionReplayResult {
                        verified_count: i,
                        failed_at_index: Some(i),
                        failure_type: Some(TransitionFailureType::ProvingError(e.to_string())),
                    };
                }
            },
            Err(e) => {
                println!("FAILED: {}", e);
                return TransitionReplayResult {
                    verified_count: i,
                    failed_at_index: Some(i),
                    failure_type: Some(TransitionFailureType::ProvingError(e.to_string())),
                };
            }
        };

        // Prove the transition
        let prove_info = match prover.prove(env, elf) {
            Ok(info) => info,
            Err(e) => {
                println!("FAILED: {}", e);
                return TransitionReplayResult {
                    verified_count: i,
                    failed_at_index: Some(i),
                    failure_type: Some(TransitionFailureType::ProvingError(e.to_string())),
                };
            }
        };

        // Decode the output from the journal
        let output: TransitionOutput = match prove_info.receipt.journal.decode() {
            Ok(out) => out,
            Err(e) => {
                println!("FAILED: {}", e);
                return TransitionReplayResult {
                    verified_count: i,
                    failed_at_index: Some(i),
                    failure_type: Some(TransitionFailureType::ProvingError(e.to_string())),
                };
            }
        };

        // Verify the item hash matches
        if !output.verify_item_hash(item) {
            println!("FAILED: item hash mismatch");
            return TransitionReplayResult {
                verified_count: i,
                failed_at_index: Some(i),
                failure_type: Some(TransitionFailureType::ItemHashMismatch),
            };
        }

        // Verify fingerprint
        if !output.fingerprint_verified {
            println!("FAILED: fingerprint mismatch");
            return TransitionReplayResult {
                verified_count: i,
                failed_at_index: Some(i),
                failure_type: Some(TransitionFailureType::FingerprintMismatch),
            };
        }

        // Update the frontier for the next iteration
        current_frontier = output.new_frontier;

        println!("PROVED (fingerprint verified)");
    }

    TransitionReplayResult {
        verified_count: trace_window.len(),
        failed_at_index: None,
        failure_type: None,
    }
}
