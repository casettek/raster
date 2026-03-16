//! Run command: build and execute the user program as a whole.

use raster_backend::backend::HexString;
use raster_core::cfs::{CfsCoordinates, CfsCursor};
use rayon::prelude::*;

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use raster_backend::{Backend, ExecutionMode};
use raster_backend_risc0::Risc0Backend;
use raster_core::tile::TileId;

use raster_compiler::tile::TileDiscovery;
use raster_compiler::{CfsBuilder, Project};

use raster_core::trace::{SequenceStartRecord, StepRecord, TileExecRecord, Trace, TraceWindow};
use raster_core::{Error, Result};

use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::replay::{ReplayResult, Replayer};
use raster_prover::trace::{
    SerializableFrontier, TraceCommitment, TraceVerifier, VerificationResult,
};
use raster_prover::transition::{step_transitions, TransitionJournal};

use crate::BackendType;

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

    let stdout_handle = std::thread::spawn(move || {
        let stdout_reader = BufReader::new(stdout);

        for line in stdout_reader.lines() {
            if let Ok(line_str) = line {
                if let Some(parsed) = line_str
                    .strip_prefix("[trace]")
                    .map(serde_json::from_str::<StepRecord>)
                {
                    if let Ok(step_record) = parsed {
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
                println!("trace tile coordinates: {:?}", tile_record.coordinates);
                println!("cfs coordinates: {:?}", cfs_cursor.coordinates());
                current_coordinates = tile_record.coordinates.clone();

                let next_coordinates_options =
                    cfs_cursor.try_get_next_coordinates(&cfs_cursor.coordinates());
                println!("next coordiates options: {:?}", next_coordinates_options);

                if !cfs_cursor.is_next_coordinates(&current_coordinates) {
                    panic!("Bad coordinate transition");
                }
                cfs_cursor.set_coordinates(current_coordinates);

                trace_coordinates.push(tile_record.coordinates.clone());
            }
            StepRecord::SequenceEnd(sequence_end_record) => {
                println!(
                    "trace sequence end coordinates: {:?}",
                    sequence_end_record.sequence_coordinates
                );
                println!("cfs coordinates: {:?}", cfs_cursor.coordinates());
                current_coordinates = sequence_end_record.sequence_coordinates.clone();

                let next_coordinates_options =
                    cfs_cursor.try_get_next_coordinates(&cfs_cursor.coordinates());
                println!("next coordiates options: {:?}", next_coordinates_options);

                if !cfs_cursor.is_next_coordinates(&current_coordinates) {
                    panic!("Bad coordinate transition");
                }
                cfs_cursor.set_coordinates(current_coordinates);

                trace_coordinates.push(sequence_end_record.sequence_coordinates.clone());
            }
            StepRecord::SequenceStart(sequence_start_record) => {
                println!(
                    "trace sequence start coordinates: {:?}",
                    sequence_start_record.sequence_coordinates
                );
                println!("cfs coordinates: {:?}", cfs_cursor.coordinates());

                current_coordinates = sequence_start_record.sequence_coordinates.clone();

                let next_coordinates_options =
                    cfs_cursor.try_get_next_coordinates(&cfs_cursor.coordinates());
                println!("next coordiates options: {:?}", next_coordinates_options);

                if !cfs_cursor.is_next_coordinates(&current_coordinates) {
                    panic!("Bad coordinate transition");
                }
                cfs_cursor.set_coordinates(current_coordinates);

                trace_coordinates.push(sequence_start_record.sequence_coordinates.clone());
            }
        }
        println!("");
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
        let verification_result = verify(&trace, commit_path);

        match verification_result {
            VerificationResult::Ok => println!("Verification Success"),
            VerificationResult::Fraud(fraud_window) => {
                let replayer = Replayer::new(&backend, &project);
                let _fraud_proof = prove(fraud_window, &replayer);
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

                    println!("tile_id: {}", tile_exec_record.fn_call_record.fn_name);
                }
                StepRecord::SequenceStart(sequence_start_record) => {
                    println!(
                        "[sequence start] sequence id: {}",
                        sequence_start_record.sequence_id
                    );
                    println!(
                        "sequence coordinates: {:?}",
                        sequence_start_record.sequence_coordinates
                    );
                }
                StepRecord::SequenceEnd(sequence_end_record) => {
                    println!(
                        "[sequence end] sequence id: {}",
                        sequence_end_record.sequence_id
                    );
                    println!(
                        "sequence coordinates: {:?}",
                        sequence_end_record.sequence_coordinates
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

    if let Some(fraud_step) = trace.last_mut() {
        match fraud_step {
            StepRecord::TileExec(tile_exec_record) => {
                tile_exec_record.fn_call_record.output_data = vec![0u8, 1u8];
            }
            StepRecord::SequenceStart(sequence_start_record) => {
                sequence_start_record.input_data.push(0);
            }
            StepRecord::SequenceEnd(sequence_end_record) => {
                sequence_end_record.output_data.push(0);
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

pub fn verify(trace: &Trace, commit_path: &str) -> VerificationResult {
    let trace_commitment = read_trace_commitment(commit_path);

    let actual_trace_commitment = TraceCommitment::from(trace, &EMPTY_TRIE_NODES[0]);

    let mut trace_verifier: TraceVerifier =
        TraceVerifier::new(trace_commitment, &EMPTY_TRIE_NODES[0]);

    for step_record in trace.iter() {
        if let VerificationResult::Fraud(fraud_window) = trace_verifier.verify(step_record) {
            println!("verification result: \nfraud: {:?}", fraud_window);
            return VerificationResult::Fraud(fraud_window);
        }
    }

    VerificationResult::Ok
}

pub fn prove(fraud_window: TraceWindow, replayer: &Replayer) -> risc0_zkvm::Receipt {
    let mode = ExecutionMode::prove_and_verify();

    let mut replayed_results: BTreeMap<String, ReplayResult> = BTreeMap::new();

    for (i, step_record) in fraud_window.items.iter().enumerate() {
        println!("prove: {:?}", step_record);
        match step_record {
            StepRecord::TileExec(tile_exec) => match replayer.replay(tile_exec, mode) {
                Ok(replay_result) => {
                    replayed_results.insert(
                        tile_exec.fn_call_record.fn_name.clone(),
                        replay_result.clone(),
                    );
                }
                Err(e) => {
                    println!("FAILED: {}", e);
                }
            },
            StepRecord::SequenceEnd(item) => {
                println!("SequenceEnd proove")
            }
            StepRecord::SequenceStart(item) => {
                println!("SequenceStart proove")
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
            &replayed_results,
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
