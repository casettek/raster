//! Run command: build and execute the user program as a whole.

use rand::seq::IteratorRandom;
use raster_backend::backend::HexString;
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use raster_backend::{Backend, ExecutionMode};
use raster_backend_risc0::Risc0Backend;

use raster_compiler::{CfsBuilder, Project};

use raster_core::cfs::{CfsCoordinates, CfsCursor, ControlFlowSchema};
use raster_core::coordinate_index::{
    coordinate_index_membership_proof, coordinate_index_non_membership_proof, coordinate_index_root,
};
use raster_core::draft::DraftTransitionWitness;
use raster_core::trace::{ExternalInput, FnInput, StepRecord, Trace, TraceEvent, TraceWindow};
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
use raster_runtime::{TraceRecorder, TRACE_EVENT_PREFIX};

use crate::utils::authorization::{build_manifested_inputs, collect_external_input_commitments};
use crate::BackendType;

pub fn run(
    backend_type: BackendType,
    input: Option<&str>,
    input_manifest: Option<&str>,
    commit_flag: Option<&str>,
    audit_flag: Option<&str>,
    verbose: bool,
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

    let mut cmd = Command::new(&binary_path);
    cmd.current_dir(&project.root_dir);
    if let Some(input_json) = input {
        cmd.args(["--input", input_json]);
    }
    if let Some(manifest_json) = input_manifest {
        cmd.args(["--input-manifest", manifest_json]);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;

    let mut trace: Arc<Mutex<Trace>> = Arc::new(Mutex::new(Trace::new()));
    let mut reader_trace = Arc::clone(&trace);

    let mut log = Arc::new(Mutex::new(Vec::new()));
    let mut reader_log = Arc::clone(&log);

    let stdout = child.stdout.take().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::Other, "Could not capture stdout")
    })?;

    let stderr = child.stderr.take().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::Other, "Could not capture stderr")
    })?;

    let mut handles = Vec::new();

    let trace_recorder = Arc::new(Mutex::new(TraceRecorder::new(cfs.clone())));
    let stdout_trace_recorder = Arc::clone(&trace_recorder);
    let stdout_handle = std::thread::spawn(move || {
        let stdout_reader = BufReader::new(stdout);

        for line in stdout_reader.lines() {
            if let Ok(line_str) = line {
                if let Some(raw_event) = line_str.strip_prefix(TRACE_EVENT_PREFIX) {
                    if let Ok(trace_event) = serde_json::from_str::<TraceEvent>(raw_event) {
                        let step_record = {
                            let mut trace_recorder = stdout_trace_recorder.lock().unwrap();
                            trace_recorder.record(trace_event)
                        };
                        let mut trace_lock = reader_trace.lock().unwrap();
                        trace_lock.push(step_record);
                    }
                }
                if let Some(debug_line) = line_str.strip_prefix("[debug]") {
                    let mut log_lock = reader_log.lock().unwrap();
                    log_lock.push(debug_line.to_string());
                }
            }
        }
    });
    handles.push(stdout_handle);

    let mut errors = Arc::new(Mutex::new(Vec::new()));
    let mut thread_errors = Arc::clone(&errors);
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

    let log = Arc::try_unwrap(log)
        .expect("Cant move list out of Mutex. Some thread still holding copy of Arc")
        .into_inner()
        .unwrap();

    let trace_recorder = Arc::try_unwrap(trace_recorder)
        .expect("Cant move list out of Mutex. Some thread still holding copy of Arc")
        .into_inner()
        .unwrap();

    if verbose {
        if !log.is_empty() {
            println!("Verbose log:");
            for log_line in log {
                println!("{log_line}");
            }
        }
    }

    if !errors.is_empty() {
        println!("Error:");
        for error_line in errors {
            println!("{error_line}");
        }
    }

    let mut trace = Arc::try_unwrap(trace)
        .expect("Cant move list out of Mutex. Some thread still holding copy of Arc")
        .into_inner()
        .unwrap();

    // Build all project tiles with Risc0
    // TODO: Risc0 build can be perfomed in parallel to native user program execution

    println!("Build Risc0 artifacts...");
    let backend =
        Risc0Backend::new(project.output_dir.clone()).with_user_crate(project.root_dir.clone());

    let mut cfs_cursor = CfsCursor::new(cfs.clone());

    let mut trace_coordinates: Vec<CfsCoordinates> = Vec::new();
    let mut current_coordinates: CfsCoordinates;
    for step_record in trace.iter() {
        match step_record {
            StepRecord::TileExec(tile_record) => {
                trace_coordinates.push(tile_record.coordinates.clone());
            }
            StepRecord::SequenceEnd(sequence_end_record) => {
                trace_coordinates.push(sequence_end_record.coordinates.clone());
            }
            StepRecord::SequenceStart(sequence_start_record) => {
                trace_coordinates.push(sequence_start_record.coordinates.clone());
            }
        }
    }

    // let tiles_discovery = TileDiscovery::new(&project);
    // let tiles = tiles_discovery.tiles;
    //
    // let artifact_registry: HashMap<TileId, HexString> = tiles
    //     .into_par_iter()
    //     .map(|tile| {
    //         let tile_metadata = tile.to_metadata();
    //         let content_hash = tile.to_content_hash();
    //
    //         let artifact = backend.compile_tile(&tile_metadata, content_hash).unwrap();
    //
    //         // Return a tuple of (Key, Value)
    //         (tile_metadata.id, artifact.artifact_id())
    //     })
    //     .collect();
    //
    // for (tile_id, image_id) in &artifact_registry {
    //     println!("Registered: {:?}", tile_id);
    // }

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

    Ok(())
}

pub fn fraud(trace: &mut Trace, commit_path: &str) {
    let mut commitment_file =
        std::fs::File::create(commit_path).expect("Failed to create commitemt file");

    let mut rng = rand::rng();
    if let Some(fraud_step) = trace
        .iter_mut()
        .filter(|step_record| {
            matches!(
                step_record,
                StepRecord::TileExec(tile_exec_record)
                    if !tile_exec_record.external_input_commitment.is_empty()
            )
        })
        .choose(&mut rng)
    {
        match fraud_step {
            StepRecord::TileExec(tile_exec_record) => {
                tile_exec_record.output_commitment = vec![0u8, 1u8];
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
    coordinate_index: BTreeMap<CfsCoordinates, InternalStoreIndexValue>,
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
    let previous = state.coordinate_index.insert(
        internal_write.entry.coordinates.clone(),
        InternalStoreIndexValue {
            log_position: internal_write.log_position,
            object_commitment: internal_write.entry.object_commitment.clone(),
        },
    );
    assert!(
        previous.is_none(),
        "Duplicate internal store coordinates {:?} while building proof witness",
        internal_write.entry.coordinates
    );
}

fn internal_store_state_from_prefix(
    trace: &[StepRecord],
    trace_recorder: &TraceRecorder,
) -> ProofInternalStoreState {
    let mut state = ProofInternalStoreState {
        frontier: empty_internal_store_frontier(),
        append_entries: Vec::new(),
        coordinate_index: BTreeMap::new(),
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
                let index_witness = coordinate_index_membership_proof(
                    &before_state.coordinate_index,
                    &internal_meta.coordinates,
                )
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
            let index_non_membership_witness = coordinate_index_non_membership_proof(
                &before_state.coordinate_index,
                &entry.coordinates,
            );
            let mut after_index = before_state.coordinate_index.clone();
            after_index.insert(
                entry.coordinates.clone(),
                InternalStoreIndexValue {
                    log_position: internal_write.log_position,
                    object_commitment: entry.object_commitment.clone(),
                },
            );
            let index_membership_witness =
                coordinate_index_membership_proof(&after_index, &entry.coordinates)
                    .expect("Missing coordinate-index membership proof after write");
            internal_write_witness = Some(InternalStoreWriteWitness {
                entry,
                index_non_membership_witness,
                index_membership_witness,
            });
            apply_internal_write_to_state(&mut current_internal_store_state, &internal_write);
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
            &coordinate_index_root(&initial_internal_store_state.coordinate_index),
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
