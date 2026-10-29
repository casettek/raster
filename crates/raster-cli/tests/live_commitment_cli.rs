//! Real native execution, with scratch initialization made impossible.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned()
}
fn cli(project: &Path, args: &[&str], scratch: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cargo-raster"))
        .current_dir(project)
        .env("RASTER_SCRATCH_DIR", scratch)
        .args(["raster"])
        .args(args)
        .output()
        .unwrap()
}
fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn run_dir(stdout: &str) -> PathBuf {
    stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("Run artifacts dir: "))
        .unwrap()
        .into()
}
#[test]
fn live_recurrence_and_drafts_match_saved_trace_without_scratch() {
    let temp = tempfile::tempdir_in(root().join("target")).unwrap();
    let blocker = temp.path().join("not-a-directory");
    fs::write(&blocker, b"forbidden").unwrap();
    let forbidden = blocker.join("scratch");
    let project = root().join("examples/hello-tiles");
    success(
        Command::new("cargo")
            .current_dir(&project)
            .args([
                "run",
                "--release",
                "--features",
                "gen-input",
                "--bin",
                "gen_input",
                "--",
            ])
            .arg(temp.path())
            .arg("8")
            .output()
            .unwrap(),
    );
    let input = temp.path().join("input.json");
    let manifest = temp.path().join("input_manifest.json");
    let commit = temp.path().join("live.bin");
    let args = [
        "run",
        "--input",
        input.to_str().unwrap(),
        "--input-manifest",
        manifest.to_str().unwrap(),
        "--commit",
        commit.to_str().unwrap(),
        "--fraud-proof-window-size",
        "2",
    ];
    let stdout = success(cli(&project, &args, &forbidden));
    let dir = run_dir(&stdout);
    assert!(!dir.join("trace.bin").exists());
    let expected = fs::read(&commit).unwrap();
    for format in ["binary", "json"] {
        let mut saved = args.to_vec();
        saved.extend(["--save-trace", "--trace-format", format]);
        let stdout = success(cli(&project, &saved, &forbidden));
        assert_eq!(expected, fs::read(&commit).unwrap());
        let dir = run_dir(&stdout);
        let trace = dir.join(if format == "binary" {
            "trace.bin"
        } else {
            "trace.ndjson"
        });
        assert!(trace.is_file());
        if format == "binary" {
            assert_live_evidence(&project, &input, &manifest, &trace, temp.path(), &forbidden);
        }

        let rebuilt = temp.path().join("rebuilt.bin");
        success(cli(
            &project,
            &[
                "trace",
                "commit",
                "--project",
                project.to_str().unwrap(),
                "--trace",
                trace.to_str().unwrap(),
                "--input",
                input.to_str().unwrap(),
                "--input-manifest",
                manifest.to_str().unwrap(),
                "--output",
                rebuilt.to_str().unwrap(),
                "--fraud-proof-window-size",
                "2",
                "--trace-format",
                format,
            ],
            &temp.path().join("scratch"),
        ));
        assert_eq!(expected, fs::read(&rebuilt).unwrap());
        let report = temp.path().join("audit.json");
        let proof = temp.path().join("unused.receipt");
        success(cli(
            &project,
            &[
                "trace",
                "audit",
                "--project",
                project.to_str().unwrap(),
                "--trace",
                trace.to_str().unwrap(),
                "--input",
                input.to_str().unwrap(),
                "--input-manifest",
                manifest.to_str().unwrap(),
                "--commitment",
                commit.to_str().unwrap(),
                "--proof-output",
                proof.to_str().unwrap(),
                "--report",
                report.to_str().unwrap(),
                "--trace-format",
                format,
            ],
            &temp.path().join("scratch"),
        ));
        let report: serde_json::Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
        assert_eq!(report["outcome"], "no-fault");
        assert!(!proof.exists());
    }
    let audit = success(cli(
        &project,
        &[
            "run",
            "--input",
            input.to_str().unwrap(),
            "--input-manifest",
            manifest.to_str().unwrap(),
            "--audit",
            commit.to_str().unwrap(),
        ],
        &forbidden,
    ));
    assert!(audit.contains("Verification Success"));
    let receipt = temp.path().join("previous.receipt");
    fs::write(&receipt, b"previous verified receipt").unwrap();
    let invalid_input = temp.path().join("invalid-input.json");
    fs::write(&invalid_input, b"{}").unwrap();
    let report = temp.path().join("audit-error.json");
    let failed = cli(
        &project,
        &[
            "run",
            "--audit",
            commit.to_str().unwrap(),
            "--input",
            invalid_input.to_str().unwrap(),
            "--input-manifest",
            manifest.to_str().unwrap(),
            "--proof-output",
            receipt.to_str().unwrap(),
            "--report",
            report.to_str().unwrap(),
        ],
        &forbidden,
    );
    assert!(!failed.status.success());
    assert_eq!(fs::read(&receipt).unwrap(), b"previous verified receipt");
    assert!(!report.exists());
    #[cfg(unix)]
    {
        use std::{
            io::{BufRead, BufReader},
            os::unix::process::{CommandExt, ExitStatusExt},
            process::Stdio,
        };
        let mut child = Command::new(env!("CARGO_BIN_EXE_cargo-raster"))
            .current_dir(&project)
            .process_group(0)
            .env("RASTER_SCRATCH_DIR", &forbidden)
            .args([
                "raster",
                "run",
                "--audit",
                commit.to_str().unwrap(),
                "--input",
                input.to_str().unwrap(),
                "--input-manifest",
                manifest.to_str().unwrap(),
                "--proof-output",
                receipt.to_str().unwrap(),
                "--report",
                report.to_str().unwrap(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap());
        let mut reached_native = false;
        let mut cancelled_dir = None;
        for line in reader.lines() {
            let line = line.unwrap();
            if let Some(path) = line.strip_prefix("Run artifacts dir: ") {
                cancelled_dir = Some(PathBuf::from(path));
            }
            if line.starts_with("RASTER_AUDIT_PROGRESS ") {
                reached_native = true;
                assert!(Command::new("kill")
                    .args(["-TERM", "--", &format!("-{}", child.id())])
                    .status()
                    .unwrap()
                    .success());
                break;
            }
        }
        let status = child.wait().unwrap();
        assert!(reached_native);
        assert_eq!(status.signal(), Some(15));
        assert_eq!(fs::read(&receipt).unwrap(), b"previous verified receipt");
        assert!(!report.exists());
        if let Some(dir) = cancelled_dir {
            assert!(!dir.join("trace.bin").exists());
            for entry in fs::read_dir(dir).unwrap().flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".audit-evidence-")
                {
                    fs::remove_file(entry.path()).unwrap();
                }
            }
        }
    }

    let destination_directory = temp.path().join("directory-not-file");
    fs::create_dir(&destination_directory).unwrap();
    let mut unwritable = args.to_vec();
    let destination = unwritable
        .iter()
        .position(|arg| *arg == "--commit")
        .unwrap()
        + 1;
    unwritable[destination] = destination_directory.to_str().unwrap();
    assert!(!cli(&project, &unwritable, &forbidden).status.success());
    assert!(destination_directory.is_dir());
    assert_eq!(expected, fs::read(&commit).unwrap());
    // A valid execution with an oversized window must not replace the previous result.
    let mut invalid = args.to_vec();
    *invalid.last_mut().unwrap() = "1024";
    assert!(!cli(&project, &invalid, &forbidden).status.success());
    assert_eq!(expected, fs::read(&commit).unwrap());
}

#[test]
fn chain_live_commitments_and_selected_reruns_need_no_scratch() {
    let temp = tempfile::tempdir_in(root().join("target")).unwrap();
    let blocker = temp.path().join("not-a-directory");
    fs::write(&blocker, b"forbidden").unwrap();
    let forbidden = blocker.join("scratch");
    let manifest = root().join("examples/chain-example/Raster.toml");
    let run = temp.path().join("run");
    fs::create_dir(&run).unwrap();
    let args = [
        "chain",
        "run",
        manifest.to_str().unwrap(),
        "--run",
        run.to_str().unwrap(),
    ];
    success(cli(&root(), &args, &forbidden));
    let commit = run.join("aggregate/commit.bin");
    let expected = fs::read(&commit).unwrap();
    for stage in ["normalize", "aggregate", "report"] {
        assert!(run.join(stage).join("commit.bin").is_file());
        assert!(!run.join(stage).join("trace.bin").exists());
    }
    let mut selected = args.to_vec();
    selected.extend(["--stage", "aggregate", "--save-trace"]);
    success(cli(&root(), &selected, &forbidden));
    assert_eq!(expected, fs::read(&commit).unwrap());
    assert!(run.join("aggregate/trace.bin").is_file());
    assert!(!run.join("report").exists());
    selected.pop();
    success(cli(&root(), &selected, &forbidden));
    assert!(!run.join("aggregate/trace.bin").exists());
    assert_eq!(expected, fs::read(&commit).unwrap());
}

fn assert_live_evidence(
    project: &Path,
    input: &Path,
    manifest: &Path,
    saved_trace: &Path,
    temp: &Path,
    forbidden: &Path,
) {
    use raster_core::{
        merkle::EMPTY_LEAF,
        trace::{Trace, TraceEvent},
    };
    use raster_prover::{
        commitment_file::CommitmentBuilder,
        trace::{FraudProofConfig, TraceVerifier, VerificationResult},
        transition::AuditHandoff,
    };
    use raster_runtime::tracing::publishers::audit::*;
    let project = raster_compiler::Project::new(project.to_owned()).unwrap();
    let cfs = raster_compiler::CfsBuilder::new(&project).build().unwrap();
    // Independently rebuild the saved records; raw event payloads are only used
    // by this compatibility oracle, never by live audit.
    let mut recorder = raster_runtime::TraceRecorder::for_commitment_with_storage(
        cfs.clone(),
        raster_runtime::WorkingStorage::Memory,
    );
    recorder
        .set_external_input(input.to_str(), manifest.to_str())
        .unwrap();
    let mut recorded = raster_runtime::TraceRecorder::new(cfs.clone());
    recorded
        .set_external_input(input.to_str(), manifest.to_str())
        .unwrap();
    let bytes = fs::read(saved_trace).unwrap();
    let mut remaining = bytes.as_slice();
    let mut records = Vec::new();
    while !remaining.is_empty() {
        let size = u32::from_le_bytes(remaining[..4].try_into().unwrap()) as usize;
        let event: TraceEvent = postcard::from_bytes(&remaining[4..4 + size]).unwrap();
        let step = recorder.record(event.clone());
        assert_eq!(step, recorded.record(event));
        records.push(step);
        remaining = &remaining[4 + size..];
    }
    let trace = Trace(records);
    assert!(trace.0.len() > 128, "fixture must exercise window 128");
    for window in [2, 128] {
        for target in [0, window - 1, window, trace.0.len() / 2, trace.0.len() - 1] {
            let mut wrong = trace.0.clone();
            wrong[target].exec_index += 10000;
            let mut builder = CommitmentBuilder::in_memory(
                &EMPTY_LEAF,
                FraudProofConfig::from_window_size(window).unwrap(),
            );
            for step in wrong {
                builder.push(step).unwrap();
            }
            let commitment = builder.finish().unwrap();
            let disputed = temp.join("disputed.bin");
            commitment.write(&disputed).unwrap();
            let mut verifier = TraceVerifier::new(commitment, &EMPTY_LEAF, &cfs).unwrap();
            let VerificationResult::Fraud(expected) = verifier.verify(&trace) else {
                panic!("expected divergence")
            };
            let handoff = temp.join("handoff.bin");
            let output = Command::new(project.target_dir.join("release").join(&project.name))
                .current_dir(&project.root_dir)
                .args([
                    "--input",
                    input.to_str().unwrap(),
                    "--input-manifest",
                    manifest.to_str().unwrap(),
                ])
                .env("RASTER_AUTH", "1")
                .env("RASTER_SCRATCH_DIR", forbidden)
                .env(AUDIT_PATH_ENV, &disputed)
                .env(AUDIT_PROJECT_ENV, &project.root_dir)
                .env(AUDIT_OUTPUT_ENV, &handoff)
                .env(AUDIT_BINDING_ENV, "test-binding")
                .env_remove("RASTER_TRACE_PATH")
                .env_remove("RASTER_COMMIT_PATH")
                .env_remove("RASTER_OUTPUT_DIR")
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(DIVERGENCE_EXIT_CODE),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let result: AuditHandoff =
                raster_core::bincode::deserialize(&fs::read(&handoff).unwrap()).unwrap();
            assert_eq!(result.binding, "test-binding");
            let fault = result.fault.unwrap();
            assert_eq!(
                raster_core::bincode::serialize(&fault.window).unwrap(),
                raster_core::bincode::serialize(&expected.window).unwrap()
            );
            assert_eq!(
                fault.input_sources_witnesses.len(),
                expected.input_sources_witnesses.len()
            );
            for (key, witness) in &expected.input_sources_witnesses {
                assert_eq!(
                    raster_core::bincode::serialize(
                        fault.input_sources_witnesses.get(key).unwrap()
                    )
                    .unwrap(),
                    raster_core::bincode::serialize(witness).unwrap()
                );
            }
            assert_eq!(fault.io.len(), fault.window.items.len());
            let start = result.steps - fault.window.items.len();
            let mut storage =
                raster_runtime::tracing::proof_input::ProofStorageState::for_recorded();
            for step in &trace.0[..start] {
                storage.advance(step, &recorded);
            }
            let (frontier, index_root, entrypoint) = storage.initial(
                raster_core::cfs::CfsCursor::new(cfs.clone())
                    .main_entrypoint_names()
                    .is_some(),
            );
            assert_eq!(
                raster_core::bincode::serialize(&fault.initial_storage_frontier).unwrap(),
                raster_core::bincode::serialize(&frontier).unwrap()
            );
            assert_eq!(fault.initial_index_root, index_root);
            assert_eq!(
                raster_core::bincode::serialize(&fault.entrypoint_membership_witness).unwrap(),
                raster_core::bincode::serialize(&entrypoint).unwrap()
            );
            let recur = start
                .checked_sub(1)
                .and_then(|i| recorded.recur_progress_after(trace.0[i].exec_index));
            assert_eq!(
                raster_core::bincode::serialize(&fault.window_start_recur_progress).unwrap(),
                raster_core::bincode::serialize(&recur).unwrap()
            );
            for (step, io) in fault.window.items.iter().zip(&fault.io) {
                let expected_io = raster_runtime::tracing::proof_input::prepare_step_io(
                    step,
                    &recorded,
                    &mut storage,
                );
                let actual_json = serde_json::to_value(io).unwrap();
                let expected_json = serde_json::to_value(&expected_io).unwrap();
                for (field, value) in actual_json.as_object().unwrap() {
                    assert!(
                        value == &expected_json[field],
                        "IO mismatch at {} field {}",
                        step.exec_index,
                        field
                    );
                }
                assert!(
                    raster_core::bincode::serialize(io).unwrap()
                        == raster_core::bincode::serialize(&expected_io).unwrap()
                );
            }

            assert!(result.steps <= trace.0.len());
            assert!(!temp.join("fault.receipt").exists());
        }
    }
}
