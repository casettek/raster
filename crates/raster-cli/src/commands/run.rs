//! Run command: build and execute the user program as a whole.

use rand::seq::IteratorRandom;
use sha2::{Digest, Sha256};

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use raster_analysis::{Analyzer, Report};
use raster_backend::ExecutionMode;
use raster_backend_risc0::Risc0Backend;

use raster_compiler::{CfsBuilder, Project};

use raster_core::cfs::ControlFlowSchema;
use raster_core::coordinate_index::IncrementalCoordinateIndex;
use raster_core::draft::DraftTransitionWitness;
use raster_core::trace::{ExternalInput, FnInput, StepRecord, Trace, TraceEvent};
use raster_core::transition::{
    InternalStoreEntry, InternalStoreIndexValue, InternalStoreLogWitness, InternalStoreReadWitness,
    InternalStoreWitness, InternalStoreWriteWitness,
};
use raster_core::{Error, Result};

use raster_prover::authorization::authorize_external_inputs;
use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::replay::{ReplayResult, Replayer};
use raster_prover::trace::{
    Bytes, FraudEvidence, SerializableFrontier, TraceCommitment, TraceTree, TraceVerifier,
    VerificationResult,
};
use raster_prover::transition::step_transitions;
use raster_runtime::TraceRecorder;

use crate::commands::create_run_artifacts;
use crate::utils::authorization::{build_manifested_inputs, collect_external_input_commitments};
use crate::{BackendType, TraceFormat};

pub fn run(
    backend_type: BackendType,
    input: Option<&str>,
    input_manifest: Option<&str>,
    commit_flag: Option<&str>,
    audit_flag: Option<&str>,
    _verbose: bool,
    trace_format: TraceFormat,
    features: &[String],
    all_features: bool,
    no_default_features: bool,
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

    println!("Control Flow Schema build..");
    let cfs_builder = CfsBuilder::new(&project);
    let cfs = cfs_builder
        .build()
        .map_err(|e| Error::Other(format!("Failed to build CFS: {}", e)))?;

    println!("Building project...");

    let mut build_command = Command::new("cargo");
    build_command
        .current_dir(&project.root_dir)
        .args(["build", "--release"]);
    if no_default_features {
        build_command.arg("--no-default-features");
    }
    if all_features {
        build_command.arg("--all-features");
    }
    if !features.is_empty() {
        build_command.arg("--features");
        build_command.arg(features.join(","));
    }

    let build_status = build_command
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
    let artifacts = create_run_artifacts(trace_format)?;
    let trace_path = artifacts.trace_path.clone();
    let profile_path = artifacts.profile_path.clone();
    let profile_stream_path = artifacts.profile_stream_path.clone();
    println!();
    println!("Raster Run");
    println!("  Project: {}", &project.name);
    println!("  Run ID: {}", artifacts.run_id);
    println!("  Run artifacts dir: {}", artifacts.run_dir.display());
    println!("  Trace path: {}", trace_path.display());
    if profiling_enabled(features, all_features) {
        println!(
            "  Expected live profile stream: {}",
            profile_stream_path.display()
        );
        println!(
            "  Follow with: cargo raster analyze --follow {}",
            profile_stream_path.display()
        );
    }
    println!();

    let mut cmd = Command::new(&binary_path);
    cmd.current_dir(&project.root_dir);
    cmd.env(raster_runtime::TRACE_PATH_ENV, &trace_path);
    cmd.env(
        raster_runtime::TRACE_FORMAT_ENV,
        trace_format.as_runtime_str(),
    );
    cmd.env(raster_runtime::PROFILE_PATH_ENV, &profile_path);
    cmd.env(
        raster_runtime::PROFILE_STREAM_PATH_ENV,
        &profile_stream_path,
    );
    cmd.env(raster_runtime::PROFILE_RUN_ID_ENV, &artifacts.run_id);
    if let Some(input_json) = input {
        cmd.args(["--input", input_json]);
    }
    if let Some(manifest_json) = input_manifest {
        cmd.args(["--input-manifest", manifest_json]);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;

    let user_output = Arc::new(Mutex::new(Vec::new()));
    let reader_user_output = Arc::clone(&user_output);

    let stdout = child.stdout.take().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::Other, "Could not capture stdout")
    })?;

    let stderr = child.stderr.take().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::Other, "Could not capture stderr")
    })?;

    let mut handles = Vec::new();

    let stdout_handle = std::thread::spawn(move || {
        let stdout_reader = BufReader::new(stdout);

        for line in stdout_reader.lines() {
            if let Ok(line_str) = line {
                if let Some(output_line) = line_str.strip_prefix("[output]") {
                    let mut output_lock = reader_user_output.lock().unwrap();
                    output_lock.push(output_line.trim_start().to_string());
                }
            }
        }
    });
    handles.push(stdout_handle);

    let errors = Arc::new(Mutex::new(Vec::new()));
    let thread_errors = Arc::clone(&errors);
    let stderr_handle = std::thread::spawn(move || {
        let stderr_reader = BufReader::new(stderr);
        for line in stderr_reader.lines() {
            if let Ok(error_line) = line {
                let mut errors_lock = thread_errors.lock().unwrap();
                errors_lock.push(error_line);
            }
        }
    });
    handles.push(stderr_handle);

    let status = child.wait()?;
    for handle in handles {
        handle.join().expect("A child thread panicked");
    }
    println!("Process exited with: {}", status);

    let errors = Arc::try_unwrap(errors)
        .expect("Cant move list out of Mutex. Some thread still holding copy of Arc")
        .into_inner()
        .unwrap();

    let user_output = Arc::try_unwrap(user_output)
        .expect("Cant move list out of Mutex. Some thread still holding copy of Arc")
        .into_inner()
        .unwrap();

    if !user_output.is_empty() {
        println!();
        println!("Output:");
        for output_line in user_output {
            println!("  {output_line}");
        }
    }

    if !errors.is_empty() {
        println!("Error:");
        for error_line in errors {
            println!("{error_line}");
        }
    }

    let (mut trace, trace_recorder) = load_trace_from_file(&trace_path, trace_format, &cfs)?;

    if commit_flag.is_some() {
        let commit_path = commit_flag.expect("Commitment path was provided");

        // TODO: temprorary way to generate "fraud" trace commitment
        // prefix file with fraud_{NAME}
        //
        if commit_path.starts_with("fraud_") {
            fraud(&mut trace, commit_path)
        } else {
            commit(&trace, commit_path);
        }
    } else if audit_flag.is_some() {
        let commit_path = audit_flag.expect("Commitment path was provided");
        let verification_result = verify(&trace, commit_path, &cfs);

        match verification_result {
            VerificationResult::Ok => println!("Verification Success"),
            VerificationResult::Fraud(fraud_evidence) => {
                let backend = Risc0Backend::new(project.output_dir.clone())
                    .with_user_crate(project.root_dir.clone());
                let replayer = Replayer::new(&backend, &project);
                let fraud_proof = prove(
                    fraud_evidence,
                    &trace,
                    &cfs,
                    &trace_recorder,
                    &replayer,
                    input_manifest,
                );
                let fraud_proof_path = write_fraud_proof(&fraud_proof, commit_path);
                println!("Fraud proof generated: {}", fraud_proof_path.display());
            }
        }
    } else {
        // TODO: in case of just simple execution did printing out trace items is enough or save
        // them to file

        for step_record in trace {
            match step_record {
                StepRecord::TileExec(tile_exec_record) => {
                    println!("\nexec_index: {}", tile_exec_record.exec_index);
                    println!("sequence_id: {}", tile_exec_record.sequence_id);
                    println!("tile_coordinates: {:?}", tile_exec_record.coordinates,);

                    println!("tile_id: {}", tile_exec_record.tile_id);
                }
                StepRecord::RecurExec(recur_exec_record) => {
                    println!("\nexec_index: {}", recur_exec_record.exec_index);
                    println!("sequence_id: {}", recur_exec_record.sequence_id);
                    println!("recur_coordinates: {:?}", recur_exec_record.coordinates,);

                    println!("recur_id: {}", recur_exec_record.recur_id);
                }
                StepRecord::SequenceStart(sequence_start_record) => {
                    println!(
                        "[sequence start] sequence id: {}",
                        sequence_start_record.sequence_id
                    );
                    println!(
                        "sequence coordinates: {:?}",
                        sequence_start_record.coordinates
                    );
                }
                StepRecord::SequenceEnd(sequence_end_record) => {
                    println!(
                        "[sequence end] sequence id: {}",
                        sequence_end_record.sequence_id
                    );
                    println!(
                        "sequence coordinates: {:?}",
                        sequence_end_record.coordinates
                    );
                }
            }
        }
    }

    if profile_path.exists() {
        println!();
        println!("Profiling");
        if profile_stream_path.exists() {
            println!(
                "  Live profile stream saved to: {}",
                profile_stream_path.display()
            );
            println!(
                "  Follow with: cargo raster analyze --follow {}",
                profile_stream_path.display()
            );
        }
        println!("  Execution profile saved to: {}", profile_path.display());
        let analyzer = Analyzer::from_path(&profile_path)?;
        let metrics = analyzer.analyze()?;
        let report = Report::new(metrics);
        println!();
        println!("{}", report.to_text());
    }

    Ok(())
}

fn profiling_enabled(features: &[String], all_features: bool) -> bool {
    all_features
        || features.iter().any(|feature| {
            feature
                .split(|ch: char| ch == ',' || ch.is_whitespace())
                .any(|part| part == "profiling" || part.ends_with("/profiling"))
        })
}

fn load_trace_from_file(
    trace_path: &PathBuf,
    trace_format: TraceFormat,
    cfs: &ControlFlowSchema,
) -> Result<(Trace, TraceRecorder)> {
    let mut trace = Trace::new();
    let mut trace_recorder = TraceRecorder::new(cfs.clone());

    match trace_format {
        TraceFormat::Binary => {
            load_binary_trace_from_file(trace_path, &mut trace, &mut trace_recorder)?
        }
        TraceFormat::Json => {
            load_json_trace_from_file(trace_path, &mut trace, &mut trace_recorder)?
        }
    }

    Ok((trace, trace_recorder))
}

fn load_binary_trace_from_file(
    trace_path: &PathBuf,
    trace: &mut Trace,
    trace_recorder: &mut TraceRecorder,
) -> Result<()> {
    let mut file = std::fs::File::open(trace_path).map_err(raster_core::Error::Io)?;

    loop {
        let mut len_buf = [0u8; 4];
        let mut header_bytes_read = 0usize;
        let mut reached_clean_eof = false;
        while header_bytes_read < len_buf.len() {
            match file.read(&mut len_buf[header_bytes_read..]) {
                Ok(0) if header_bytes_read == 0 => {
                    reached_clean_eof = true;
                    break;
                }
                Ok(0) => {
                    return Err(raster_core::Error::Other(
                        "Trace stream ended with a partial frame header".into(),
                    ))
                }
                Ok(bytes_read) => header_bytes_read += bytes_read,
                Err(error) => return Err(raster_core::Error::Io(error)),
            }
        }
        if reached_clean_eof {
            break;
        }

        let frame_len = u32::from_le_bytes(len_buf) as usize;
        let mut frame = vec![0u8; frame_len];
        file.read_exact(&mut frame).map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                raster_core::Error::Other("Trace stream ended with a partial frame payload".into())
            } else {
                raster_core::Error::Io(error)
            }
        })?;

        let event: TraceEvent = postcard::from_bytes(&frame).map_err(|error| {
            raster_core::Error::Serialization(format!(
                "Failed to decode binary trace event '{}': {}",
                trace_path.display(),
                error
            ))
        })?;
        record_trace_event(trace, trace_recorder, event);
    }

    Ok(())
}

fn load_json_trace_from_file(
    trace_path: &PathBuf,
    trace: &mut Trace,
    trace_recorder: &mut TraceRecorder,
) -> Result<()> {
    let file = std::fs::File::open(trace_path).map_err(raster_core::Error::Io)?;
    let reader = BufReader::new(file);

    for (line_index, line) in reader.lines().enumerate() {
        let line = line.map_err(raster_core::Error::Io)?;
        if line.trim().is_empty() {
            continue;
        }

        let event: TraceEvent = serde_json::from_str(&line).map_err(|error| {
            raster_core::Error::Serialization(format!(
                "Failed to decode JSON trace event '{}' at line {}: {}",
                trace_path.display(),
                line_index + 1,
                error
            ))
        })?;
        record_trace_event(trace, trace_recorder, event);
    }

    Ok(())
}

fn record_trace_event(trace: &mut Trace, trace_recorder: &mut TraceRecorder, event: TraceEvent) {
    let step_record = trace_recorder.record(event);
    trace.push(step_record);
}

pub fn fraud(trace: &mut Trace, commit_path: &str) {
    let mut commitment_file =
        std::fs::File::create(commit_path).expect("Failed to create commitemt file");

    let mut rng = rand::rng();
    if let Some(fraud_step) = trace
        .iter_mut()
        .filter(|step_record| match step_record {
            StepRecord::TileExec(tile_exec_record) => {
                !tile_exec_record.external_input_commitment.is_empty()
            }
            StepRecord::RecurExec(recur_exec_record) => {
                !recur_exec_record.external_input_commitment.is_empty()
            }
            StepRecord::SequenceStart(_) | StepRecord::SequenceEnd(_) => false,
        })
        .choose(&mut rng)
    {
        match fraud_step {
            StepRecord::TileExec(tile_exec_record) => {
                tile_exec_record.output_commitment = vec![0u8, 1u8];
            }
            StepRecord::RecurExec(recur_exec_record) => {
                recur_exec_record.output_commitment = vec![0u8, 1u8];
            }
            StepRecord::SequenceStart(sequence_start_record) => {
                sequence_start_record.input_commitment.push(0);
            }
            StepRecord::SequenceEnd(sequence_end_record) => {
                sequence_end_record.output_commitment.push(0);
            }
        }
    };

    let trace_commitment = TraceCommitment::from(trace, &EMPTY_TRIE_NODES[0]);

    let bytes = postcard::to_allocvec(&trace_commitment).unwrap();

    commitment_file
        .write_all(&bytes)
        .expect("Failed to save commitment");
}

pub fn commit(trace: &Trace, commit_path: &str) {
    let mut commitment_file =
        std::fs::File::create(commit_path).expect("Failed to create commitemt file");

    let trace_commitment = TraceCommitment::from(trace, &EMPTY_TRIE_NODES[0]);
    let bytes = postcard::to_allocvec(&trace_commitment).unwrap();

    commitment_file
        .write_all(&bytes)
        .expect("Failed to save commitment");
}

pub fn verify(trace: &Trace, commit_path: &str, cfs: &ControlFlowSchema) -> VerificationResult {
    let trace_commitment = read_trace_commitment(commit_path);

    let mut trace_verifier = TraceVerifier::new(trace_commitment, &EMPTY_TRIE_NODES[0], cfs);

    trace_verifier.verify(trace)
}

pub fn fraud_proof_path(commit_path: &str) -> PathBuf {
    let path = PathBuf::from(commit_path);
    let mut file_name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("fraud-proof"));
    file_name.push(".fraud-proof");
    path.with_file_name(file_name)
}

pub fn write_fraud_proof(receipt: &risc0_zkvm::Receipt, commit_path: &str) -> PathBuf {
    let proof_path = fraud_proof_path(commit_path);
    let mut proof_file =
        std::fs::File::create(&proof_path).expect("Failed to create fraud proof file");
    let bytes = postcard::to_allocvec(receipt).expect("Failed to serialize fraud proof");

    proof_file
        .write_all(&bytes)
        .expect("Failed to save fraud proof");

    proof_path
}

#[derive(Debug, Clone)]
struct ProofInternalStoreState {
    frontier: SerializableFrontier,
    append_entries: Vec<InternalStoreEntry>,
    coordinate_index: IncrementalCoordinateIndex,
}

fn empty_internal_store_frontier() -> SerializableFrontier {
    SerializableFrontier {
        position: 0,
        leaf: EMPTY_TRIE_NODES[0].to_vec(),
        ommers: Vec::new(),
    }
}

fn internal_store_leaf_hash(entry: &InternalStoreEntry) -> Vec<u8> {
    Sha256::digest(entry.to_bytes()).to_vec()
}

fn build_internal_store_log_witness(
    append_entries: &[InternalStoreEntry],
    log_position: u64,
) -> InternalStoreLogWitness {
    let mut tree = TraceTree::new(1);
    tree.append(Bytes(EMPTY_TRIE_NODES[0].to_vec()));
    let mut marked_position = None;

    for (index, entry) in append_entries.iter().enumerate() {
        tree.append(Bytes(internal_store_leaf_hash(entry)));
        if u64::try_from(index).expect("append entry index overflow") + 1 == log_position {
            marked_position = tree.mark();
        }
    }

    let marked_position = marked_position.unwrap_or_else(|| {
        panic!(
            "Missing append-log position {} while building internal store log witness",
            log_position
        )
    });
    let auth_path = tree
        .witness(marked_position, 0)
        .expect("Failed to build internal store log witness");

    InternalStoreLogWitness {
        position: u64::from(marked_position),
        path_elems: auth_path.iter().map(|elem| elem.0.clone()).collect(),
    }
}

fn apply_internal_write_to_state(
    state: &mut ProofInternalStoreState,
    internal_write: &raster_runtime::InternalWriteRecord,
) {
    state.frontier = internal_write.frontier_after.clone();
    state.append_entries.push(internal_write.entry.clone());
    state.coordinate_index.insert(
        internal_write.entry.coordinates.clone(),
        InternalStoreIndexValue {
            log_position: internal_write.log_position,
            object_commitment: internal_write.entry.object_commitment.clone(),
        },
    );
}

fn internal_store_state_from_prefix(
    trace: &[StepRecord],
    trace_recorder: &TraceRecorder,
) -> ProofInternalStoreState {
    let mut state = ProofInternalStoreState {
        frontier: empty_internal_store_frontier(),
        append_entries: Vec::new(),
        coordinate_index: IncrementalCoordinateIndex::new(),
    };
    for step_record in trace {
        if let Some(internal_write) = trace_recorder
            .step_witness_at(step_record.coordinates())
            .and_then(|witness| witness.internal_write())
        {
            apply_internal_write_to_state(&mut state, &internal_write);
        }
    }

    state
}

pub fn prove(
    fraud_evidence: FraudEvidence,
    trace: &Trace,
    cfs: &ControlFlowSchema,
    trace_recorder: &TraceRecorder,
    replayer: &Replayer,
    input_manifest: Option<&str>,
) -> risc0_zkvm::Receipt {
    let mode = ExecutionMode::prove_and_verify();
    let FraudEvidence {
        window: fraud_window,
        input_sources_witnesses,
    } = fraud_evidence;
    let mut replayed_results: HashMap<StepRecord, ReplayResult> = HashMap::new();
    let mut recorded_step_io: HashMap<
        StepRecord,
        (
            Option<Vec<u8>>,
            Option<Vec<u8>>,
            Option<FnInput>,
            Option<FnInput>,
            ExternalInput,
            Option<InternalStoreWitness>,
            Option<DraftTransitionWitness>,
        ),
    > = HashMap::new();
    let window_start_index = fraud_window
        .items
        .first()
        .and_then(|first_item| {
            trace
                .iter()
                .position(|step_record| step_record == first_item)
        })
        .unwrap_or(trace.len());
    let mut current_internal_store_state =
        internal_store_state_from_prefix(&trace[..window_start_index], trace_recorder);
    let initial_internal_store_state = current_internal_store_state.clone();

    for step_record in &fraud_window.items {
        let step_witness = trace_recorder
            .step_witness_at(step_record.coordinates())
            .unwrap_or_else(|| {
                panic!(
                    "Missing recorded I/O for fraud window step at coordinates {:?}",
                    step_record.coordinates()
                )
            });
        let input_witness = step_witness.input_data();
        let output_witness = step_witness.output_data();
        let input_source_witness = step_witness.input_source_witness();
        let sequence_scope_witness =
            step_record
                .coordinates()
                .try_parent()
                .and_then(|(parent_coordinates, _)| {
                    trace_recorder
                        .step_witness_at(&parent_coordinates)
                        .and_then(|witness| witness.input_source_witness())
                });
        let external_input = step_witness.external_input();
        let draft_transition_witness = step_witness.draft_transition_witness();
        let before_state = current_internal_store_state.clone();
        let mut internal_read_witnesses = Vec::new();
        if let Some(input_source_witness_ref) = input_source_witness.as_ref() {
            for internal_meta in input_source_witness_ref.internal().values() {
                let index_witness = before_state
                    .coordinate_index
                    .membership_proof(&internal_meta.coordinates)
                    .unwrap_or_else(|| {
                        panic!(
                            "Missing coordinate-index witness for internal input at {:?}",
                            internal_meta.coordinates
                        )
                    });
                let entry = InternalStoreEntry {
                    coordinates: internal_meta.coordinates.clone(),
                    object_commitment: internal_meta.commitment.clone(),
                };
                let log_witness = build_internal_store_log_witness(
                    &before_state.append_entries,
                    index_witness.value.log_position,
                );
                internal_read_witnesses.push(InternalStoreReadWitness {
                    entry,
                    log_witness,
                    index_witness,
                });
            }
        }
        let mut internal_write_witness = None;
        if let Some(internal_write) = step_witness.internal_write() {
            let entry = internal_write.entry.clone();
            let index_non_membership_witness = before_state
                .coordinate_index
                .non_membership_proof(&entry.coordinates);
            apply_internal_write_to_state(&mut current_internal_store_state, &internal_write);
            let index_membership_witness = current_internal_store_state
                .coordinate_index
                .membership_proof(&entry.coordinates)
                .expect("Missing coordinate-index membership proof after write");
            internal_write_witness = Some(InternalStoreWriteWitness {
                entry,
                index_non_membership_witness,
                index_membership_witness,
            });
        }
        let internal_store_witness =
            if internal_read_witnesses.is_empty() && internal_write_witness.is_none() {
                None
            } else {
                Some(InternalStoreWitness {
                    reads: internal_read_witnesses,
                    write: internal_write_witness,
                })
            };
        recorded_step_io.insert(
            step_record.clone(),
            (
                input_witness.clone(),
                output_witness,
                input_source_witness,
                sequence_scope_witness,
                external_input,
                internal_store_witness,
                draft_transition_witness,
            ),
        );

        if let StepRecord::TileExec(record) = step_record {
            let replay_input = input_witness.unwrap_or_default();
            match replayer.replay(record, replay_input.as_slice(), mode) {
                Ok(replay_result) => {
                    replayed_results.insert(step_record.clone(), replay_result);
                }
                Err(e) => {
                    println!("FAILED to replay: {}", e);
                }
            }
        }
    }

    let manifested_inputs = build_manifested_inputs(
        input_manifest,
        collect_external_input_commitments(&recorded_step_io),
    )
    .unwrap_or_else(|e| panic!("Failed to load authorization source: {}", e));

    let (authorization_receipt, authorization_journal) =
        authorize_external_inputs(&manifested_inputs);

    if let Some(frontier) = SerializableFrontier::from_bytes(&fraud_window.frontier) {
        println!();
        println!("Replaying transition frontier with transition guest...");

        let Some(receipt) = step_transitions(
            &frontier,
            &initial_internal_store_state.frontier,
            &initial_internal_store_state.coordinate_index.root(),
            &fraud_window.items,
            fraud_window.fingerprint,
            &cfs,
            &input_sources_witnesses,
            &recorded_step_io,
            &replayed_results,
            &authorization_journal,
            &authorization_receipt,
        ) else {
            panic!("Failed to generate fraud proof");
        };

        return receipt;
    }

    panic!("Failed to generate fraud proof");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiling_enabled_detects_feature_flags() {
        assert!(profiling_enabled(&[], true));
        assert!(profiling_enabled(&["profiling".to_string()], false));
        assert!(profiling_enabled(&["raster/profiling".to_string()], false));
        assert!(profiling_enabled(&["foo,profiling".to_string()], false));
        assert!(!profiling_enabled(&["foo".to_string()], false));
    }
}
