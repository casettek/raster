//! Run command: build and execute the user program as a whole.

use rand::seq::IndexedMutRandom;
use raster_backend::backend::HexString;
use rayon::prelude::*;

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use raster_backend::{Backend, ExecutionMode};
use raster_backend_risc0::Risc0Backend;

use raster_compiler::tile::TileDiscovery;
use raster_compiler::{CfsBuilder, Project};

use raster_core::cfs::{CfsCoordinates, CfsCursor, ControlFlowSchema};
use raster_core::manifest::{ExternalInputEntry, InputDocument};
use raster_core::tile::TileId;
use raster_core::trace::{ExternalInput, StepRecord, Trace, TraceEvent, TraceWindow};
use raster_core::transition::{AuthorizedExternalInputs, TransitionJournal};
use raster_core::{Error, Result};

use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::replay::{ReplayResult, Replayer};
use raster_prover::trace::{
    FraudEvidence, SerializableFrontier, TraceCommitment, TraceVerifier, VerificationResult,
};
use raster_prover::transition::step_transitions;
use raster_runtime::{TraceRecorder, TRACE_EVENT_PREFIX};

use crate::BackendType;

fn authorized_external_inputs(input: Option<&str>) -> Result<AuthorizedExternalInputs> {
    let Some(raw_input) = input else {
        return Ok(AuthorizedExternalInputs::default());
    };

    let input_json = if Path::new(raw_input).is_file() {
        fs::read_to_string(raw_input).map_err(|e| {
            Error::Other(format!(
                "Failed to read authorized external input source '{}': {}",
                raw_input, e
            ))
        })?
    } else {
        raw_input.to_string()
    };

    let document: InputDocument = serde_json::from_str(&input_json).map_err(|e| {
        Error::Serialization(format!(
            "Failed to parse authorized external input source as JSON: {}",
            e
        ))
    })?;

    let commitments = document
        .into_iter()
        .filter_map(|(name, value)| {
            let entry: ExternalInputEntry = serde_json::from_value(value).ok()?;
            let data_hash = entry.data_hash?;
            Some((name, data_hash.into_bytes()))
        })
        .collect();

    Ok(AuthorizedExternalInputs { commitments })
}

pub fn run(
    backend_type: BackendType,
    input: Option<&str>,
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
                let _fraud_proof = prove(fraud_evidence, &cfs, &trace_recorder, &replayer, input);
                println!("Faurd proof generated");
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
    if let Some(fraud_step) = trace.choose_mut(&mut rng) {
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

pub fn prove(
    fraud_evidence: FraudEvidence,
    cfs: &ControlFlowSchema,
    trace_recorder: &TraceRecorder,
    replayer: &Replayer,
    input: Option<&str>,
) -> risc0_zkvm::Receipt {
    let mode = ExecutionMode::prove_and_verify();
    let FraudEvidence {
        window: fraud_window,
        witness,
    } = fraud_evidence;
    let authorized_external_inputs = authorized_external_inputs(input)
        .unwrap_or_else(|e| panic!("Failed to load authorized external commitments: {}", e));

    let mut replayed_results: HashMap<StepRecord, ReplayResult> = HashMap::new();
    let mut recorded_step_io: HashMap<
        StepRecord,
        (Option<Vec<u8>>, Option<Vec<u8>>, ExternalInput),
    > = HashMap::new();

    for step_record in &fraud_window.items {
        let (recorded_input, recorded_output, external_input) = trace_recorder
            .io_data_at(step_record.coordinates())
            .unwrap_or_else(|| {
                panic!(
                    "Missing recorded I/O for fraud window step at coordinates {:?}",
                    step_record.coordinates()
                )
            });
        let recorded_input = recorded_input.clone();
        recorded_step_io.insert(
            step_record.clone(),
            (recorded_input.clone(), recorded_output, external_input),
        );

        if let StepRecord::TileExec(record) = step_record {
            let replay_input = recorded_input.unwrap_or_default();
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

    if let Some(frontier) = SerializableFrontier::from_bytes(&fraud_window.frontier) {
        println!();
        println!("Replaying transition frontier with transition guest...");

        let Some(receipt) = step_transitions(
            &frontier,
            &fraud_window.items,
            fraud_window.fingerprint,
            &cfs,
            &witness,
            &recorded_step_io,
            &replayed_results,
            &authorized_external_inputs,
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
