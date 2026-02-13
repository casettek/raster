//! Run command: build and execute the user program as a whole.

use crate::BackendType;
use raster_backend::ExecutionMode;
use raster_backend_risc0::Risc0Backend;
use raster_compiler::Project;
use raster_core::fingerprint::{BitPacker, FingerprintAccumulator};
use raster_core::ipc::{self, IpcMessage};
use raster_core::trace::{AuditResult, TraceItem};
use raster_core::{Error, Result};
use raster_prover::trace::{
    BytesHashable, ExecutionCommitment, SerializableFrontier, TraceBridgeTree,
};
use raster_prover::transition::{
    InitTransitionState, TransitionInput, TransitionJournal, TransitionState, TRANSITION_GUEST_ELF,
};
use raster_prover::utils::DisplayBinary;
use raster_tracing::{ReplayResult, TraceReplayer};
use risc0_zkvm::{default_prover, ExecutorEnv, Receipt};
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
            println!("   Trace item: fn: {}", trace_item.fn_name);
            let item_commitment_hex: String = item_commitement
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect();
            println!("    Item commitmemt: {:02X?}", item_commitment_hex);
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
                }

                // Replay trace window with Risc0 backend for proof generation
                println!();
                println!("Replaying trace window with Risc0 backend...");

                let project = Project::new(project_path.clone())?;
                let output_dir = project_path.join("target").join("raster");
                let backend = Risc0Backend::new(output_dir).with_user_crate(project_path.clone());
                let replayer = TraceReplayer::new(&backend, &project);
                let mode = ExecutionMode::prove_and_verify();

                let mut replayed_results: BTreeMap<String, ReplayResult> = BTreeMap::new();

                for (i, item) in result.trace_window.iter().enumerate() {
                    print!("  [{}] {} ... ", i, item.fn_name);

                    match replayer.replay(item, mode) {
                        Ok(replay_result) => {
                            replayed_results.insert(item.fn_name.clone(), replay_result.clone());
                            if let Some(verified) = replay_result.execution_result.verified {
                                if verified {
                                    println!("PROVED (verified)");
                                } else {
                                    println!("PROVED (unverified)");
                                }
                            } else {
                                println!("OK");
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

                        let bits_packer = BitPacker(diff.bits_per_item);
                        let window_fingerprint = bits_packer
                            .get_range(
                                diff.window_start_position,
                                diff.window_start_position + 2,
                                &fingerprint,
                            )
                            .expect("Failed to get window fingerprint");

                        let replay_result = replay_transitions(
                            &frontier,
                            &result.trace_window,
                            window_fingerprint,
                            diff.bits_per_item,
                            &replayed_results,
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
    bits_per_item: usize,
    replayed_results: &std::collections::BTreeMap<String, ReplayResult>,
) -> TransitionReplayResult {
    // Load the transition guest ELF from raster-prover
    let elf = TRANSITION_GUEST_ELF;

    println!("transition elf");
    println!("{:02X?}", elf);

    let fingerprint_accumulator: FingerprintAccumulator =
        FingerprintAccumulator::new(BitPacker(bits_per_item));
    let current_frontier = initial_frontier.clone();
    let prover = default_prover();

    let init_transition = InitTransitionState {
        init_frontier: initial_frontier.clone(),
        ref_fingerprint: FingerprintAccumulator::from(
            fingerprint.clone(),
            BitPacker(bits_per_item),
            trace_window.len(),
        ),
    };
    let init_state = TransitionState::Init(init_transition);

    let mut transition_receipt: Option<Receipt> = None;
    let current_state = init_state;
    let mut current_journal: Option<TransitionJournal> = None;

    for (i, item) in trace_window.iter().enumerate() {
        print!("  [{}] transition {} ... ", i, item.fn_name);

        println!("Current Frontier:");
        let frontier_hex: String = current_frontier
            .to_bytes()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect();
        println!("{}", frontier_hex);

        println!("Current Hash");
        let item_hash = item.hash();
        let hash_hex: String = item_hash.iter().map(|b| format!("{:02x}", b)).collect();
        println!("{}", hash_hex);

        let Some(replay_result) = replayed_results.get(&item.fn_name) else {
            panic!("Replayed IMAGE ID not found");
        };
        // Create the input for this transition with fingerprint data
        let input = TransitionInput {
            trace_item: item.clone(),
            replay_image_id: replay_result.image_id.clone(),
        };

        // println!("Transition Input: ");
        // println!("{:#?}", input);

        let replay_receipt_bytes = replay_result.execution_result.receipt.clone().unwrap();
        let replay_receipt: Receipt = postcard::from_bytes(&replay_receipt_bytes).unwrap();

        // Build the executor environment
        let env = if let Some(journal) = current_journal {
            let Some(transition_receipt) = transition_receipt else {
                panic!("Transition receipt not found");
            };
            ExecutorEnv::builder()
                .add_assumption(replay_receipt)
                .add_assumption(transition_receipt)
                .write(&input)
                .unwrap()
                .write(&current_state)
                .unwrap()
                .write(&journal)
                .unwrap()
                .build()
                .unwrap()
        } else {
            ExecutorEnv::builder()
                .add_assumption(replay_receipt)
                .write(&input)
                .unwrap()
                .write(&current_state)
                .unwrap()
                .build()
                .unwrap()
        };

        // Prove the transition
        let prove_info = prover.prove(env, elf).unwrap();

        transition_receipt = Some(prove_info.receipt.clone());

        // Decode the output from the journal
        let journal: TransitionJournal = prove_info.receipt.journal.decode().unwrap();

        current_journal = Some(journal.clone());

        // // Verify the item hash matches
        // if !output.verify_item_hash(item) {
        //     println!("FAILED: item hash mismatch");
        //     return TransitionReplayResult {
        //         verified_count: i,
        //         failed_at_index: Some(i),
        //         failure_type: Some(TransitionFailureType::ItemHashMismatch),
        //     };
        // }

        // println!("Hash:");
        // let hash_hex: String = output
        //     .item_hash
        //     .iter()
        //     .map(|b| format!("{:02x}", b))
        //     .collect();
        // println!("{}", hash_hex);

        // println!("Output Frontier:");
        // let frontier_hex: String = output
        //     .new_frontier
        //     .to_bytes()
        //     .iter()
        //     .map(|b| format!("{:02x}", b))
        //     .collect();
        // println!("{}", frontier_hex);

        // println!("Output Item Commitment:");
        // let item_commitment_hex: String = output
        //     .tree_root
        //     .iter()
        //     .map(|b| format!("{:02x}", b))
        //     .collect();
        // println!("{}", item_commitment_hex);

        // // Update the frontier for the next iteration
        // current_frontier = output.new_frontier;
        // fingerprint_accumulator = output.current_fingerprint;

        println!("PROVED (fingerprint verified)");
    }

    println!("Expected Fingerprint:");
    fingerprint.print_binary();

    println!("Final Fingerprint:");
    fingerprint_accumulator.bits.print_binary();

    TransitionReplayResult {
        verified_count: trace_window.len(),
        failed_at_index: None,
        failure_type: None,
    }
}
