//! Live executor integration; opt in with --ignored (requires the RISC Zero toolchain).
use raster_prover::trace::TraceCommitmentExt;
use serde_json::Value;
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
fn run(project: &Path, args: &[&str], docker_log: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cargo-raster"));
    command
        .current_dir(project)
        .env("RISC0_DEV_MODE", "0")
        .env("DOCKER_HOST", "unix:///does-not-exist")
        .env("ESTIMATE_DOCKER_LOG", docker_log)
        .env(
            "PATH",
            format!(
                "{}:{}",
                docker_log.parent().unwrap().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .args(["raster"])
        .args(args);
    if args.first() == Some(&"run") {
        // Native fixture builds do not need the protocol proving guests.
        command.env("RISC0_SKIP_BUILD", "1");
        command
            .arg("--input")
            .arg(docker_log.parent().unwrap().join("input.json"))
            .arg("--input-manifest")
            .arg(docker_log.parent().unwrap().join("input_manifest.json"));
    }
    command.output().unwrap()
}
fn success(o: Output) -> String {
    assert!(
        o.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}
fn dir(stdout: &str) -> PathBuf {
    stdout
        .lines()
        .find_map(|s| s.strip_prefix("Run artifacts dir: "))
        .unwrap()
        .into()
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
#[ignore = "requires native and RISC Zero guest builds; no Docker or real proving"]
fn recursive_window_full_partial_no_fault_and_failure_reports() {
    let root = root();
    // Stable generated sources retain guest-build caches between explicit runs.
    let project = root.join("target/fault-estimate-fixture");
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("Cargo.toml"),
        format!(
            r#"
[package]
name="fault-estimate-fixture"
version="0.1.0"
edition="2021"
[workspace]
[[bin]]
name="gen_input"
path="tools/gen_input.rs"
[features]
default=["std"]
std=["raster/std"]
[dependencies]
raster = {{ path = {:?}, default-features = false }}
serde={{version="1",default-features=false,features=["derive","alloc"]}}
serde_json={{version="1",default-features=false,features=["alloc"]}}
"#,
            root.join("crates/raster")
        ),
    )
    .unwrap();
    fs::write(
        project.join("src/lib.rs"),
        r#"
#![no_std]
extern crate alloc;
use raster::prelude::*;
use serde::{Serialize, Deserialize};
#[derive(Clone, Serialize, Deserialize, raster::Selectable)]
pub struct Values { pub values: List<u64> }
#[tile(kind = recur)]
pub fn accumulate(input: RecurInput<u64>, state: RecurState<u64>) -> RecurState<u64> {
    let mut state = state;
    *state = state.wrapping_add(input.into_value());
    state
}
#[tile]
pub fn finish(value: u64) -> u64 { value }
"#,
    )
    .unwrap();
    fs::write(
        project.join("src/main.rs"),
        r#"
use fault_estimate_fixture::*;
use raster::prelude::*;
#[sequence]
fn main(source: Values) -> u64 {
    let values = select!(List<u64>, source.values);
    let total = call_recur!(tile = accumulate, input = values, state = 0u64, args = ());
    call!(finish, total)
}
"#,
    )
    .unwrap();
    let temp = tempfile::tempdir_in(root.join("target")).unwrap();
    fs::create_dir_all(project.join("tools")).unwrap();
    fs::write(project.join("tools/gen_input.rs"), r#"
use fault_estimate_fixture::Values;
fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let data = dir.join("values.rastered");
    let index = dir.join("values.rindex");
    let hash = raster::write_raster_files(&Values { values: (1..=16).collect::<Vec<u64>>().into() }, &data, &index).unwrap();
    std::fs::write(dir.join("input.json"),serde_json::to_vec(&serde_json::json!({"source":{"path":data,"index_path":index,"load_preference":"read"}})).unwrap()).unwrap();
    std::fs::write(dir.join("input_manifest.json"),serde_json::to_vec(&serde_json::json!({"source":{"type":"sha256","encoding":"raster","commitment":hash}})).unwrap()).unwrap();
}
"#).unwrap();
    success(
        Command::new("cargo")
            .env("RISC0_SKIP_BUILD", "1")
            .env("RISC0_GUEST_LOGFILE", "/dev/null")
            .current_dir(&project)
            .args(["run", "--release", "--bin", "gen_input", "--"])
            .arg(temp.path())
            .output()
            .unwrap(),
    );
    let docker_log = temp.path().join("docker.log");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let docker = temp.path().join("docker");
        fs::write(
            &docker,
            "#!/bin/sh\necho invoked >> \"$ESTIMATE_DOCKER_LOG\"\nexit 98\n",
        )
        .unwrap();
        fs::set_permissions(docker, fs::Permissions::from_mode(0o755)).unwrap();
    }
    success(run(&project, &["build", "--backend", "risc0"], &docker_log));
    let honest = temp.path().join("honest.bin");
    success(run(
        &project,
        &[
            "run",
            "--commit",
            honest.to_str().unwrap(),
            "--fraud-proof-window-size",
            "4",
        ],
        &docker_log,
    ));
    let mut commitment: raster_prover::trace::TraceCommitment =
        postcard::from_bytes(&fs::read(&honest).unwrap()).unwrap();
    let bit = 10 * commitment.fingerprint.bits_packer.bits_per_item();
    commitment.fingerprint.bits[bit / 64] ^= 1u64 << (bit % 64);
    let disputed = temp.path().join("disputed.bin");
    fs::write(&disputed, postcard::to_allocvec(&commitment).unwrap()).unwrap();
    let mut full = None;
    for limit in [None, Some("1"), Some("2"), Some("99")] {
        let mut args = vec!["run", "--audit", disputed.to_str().unwrap(), "--estimate"];
        if let Some(n) = limit {
            args.extend(["--estimate-steps", n]);
        }
        let output = success(run(&project, &args, &docker_log));
        assert_eq!(
            output
                .matches("RASTER_AUDIT_PROGRESS {\"phase\":\"audit\"}")
                .count(),
            1
        );
        assert!(!output.contains("\"phase\":\"detecting\""));
        assert!(!output.contains("\"phase\":\"capturing\""));
        let run_dir = dir(&output);
        let report = read(&run_dir.join("fault-estimate.json"));
        let count = match limit {
            Some("1") => 1,
            Some("2") => 2,
            _ => 4,
        };
        assert_eq!(report["measured_step_count"], count);
        assert_eq!(report["actual_step_count"], 4);
        assert_eq!(report["complete"], count == 4);
        assert_eq!(
            report["outcome"],
            if count == 4 { "estimated" } else { "partial" }
        );
        assert!(report["error"].is_null());
        assert!(report["audit_duration_ms"].as_f64().unwrap() > 0.0);
        assert!(report.get("detection_duration_ms").is_none());
        assert!(report.get("capture_duration_ms").is_none());
        assert!(!run_dir.join("fault.receipt").exists());
        assert!(!run_dir.join("trace.bin").exists());
        if let Some(ref previous) = full {
            let previous: &Value = previous;
            for (a, b) in report["steps"]
                .as_array()
                .unwrap()
                .iter()
                .zip(previous["steps"].as_array().unwrap())
            {
                assert_eq!(
                    a["transition"]["user_cycles"],
                    b["transition"]["user_cycles"]
                );
                assert_eq!(a["tile"]["user_cycles"], b["tile"]["user_cycles"]);
            }
        } else {
            full = Some(report);
        }
    }
    let no_fault = success(run(
        &project,
        &["run", "--audit", honest.to_str().unwrap(), "--estimate"],
        &docker_log,
    ));
    assert_eq!(
        read(&dir(&no_fault).join("fault-estimate.json"))["outcome"],
        "no-fault"
    );
    assert_eq!(
        no_fault
            .matches("RASTER_AUDIT_PROGRESS {\"phase\":\"audit\"}")
            .count(),
        1
    );
    // A source/identity mismatch must publish an incomplete error, not panic or prove.
    let lock = project.join("Raster.lock");
    let original = fs::read(&lock).unwrap();
    fs::write(&lock, b"invalid lock").unwrap();
    let failure_path = temp.path().join("failure.json");
    let output = run(
        &project,
        &[
            "run",
            "--audit",
            disputed.to_str().unwrap(),
            "--estimate",
            "--report",
            failure_path.to_str().unwrap(),
        ],
        &docker_log,
    );
    fs::write(&lock, original).unwrap();
    assert!(!output.status.success());
    let failure = read(&failure_path);
    assert_eq!(failure["outcome"], "error");
    assert_eq!(failure["complete"], false);
    assert!(failure["error"]["phase"].is_string());
    assert!(!docker_log.exists(), "estimation must never call Docker");
    if std::env::var_os("RASTER_TEST_REAL_PROOF").is_some() {
        let receipt = temp.path().join("real.receipt");
        let report = temp.path().join("real-proof.json");
        success(
            Command::new(env!("CARGO_BIN_EXE_cargo-raster"))
                .current_dir(&project)
                .env("RISC0_DEV_MODE", "0")
                .env("RISC0_SKIP_BUILD", "1")
                .args(["raster", "run", "--audit"])
                .arg(&disputed)
                .arg("--input")
                .arg(temp.path().join("input.json"))
                .arg("--input-manifest")
                .arg(temp.path().join("input_manifest.json"))
                .arg("--proof-output")
                .arg(&receipt)
                .arg("--report")
                .arg(&report)
                .output()
                .unwrap(),
        );
        let proven = read(&report);
        assert_eq!(proven["outcome"], "proved");
        assert_eq!(proven["verified"], true);
        let verified_receipt: risc0_zkvm::Receipt =
            postcard::from_bytes(&fs::read(&receipt).unwrap()).unwrap();
        verified_receipt
            .verify(raster_prover::TRANSITION_GUEST_ID)
            .unwrap();
        let journal: raster_core::transition::TransitionJournal =
            verified_receipt.journal.decode().unwrap();
        assert_eq!(
            journal.refuted_trace_commitment,
            commitment.header().digest().to_vec()
        );
        let retained = tempfile::Builder::new()
            .prefix("single-pass-proof-")
            .tempdir_in(root.join("target"))
            .unwrap()
            .keep();
        fs::copy(&receipt, retained.join("fault.receipt")).unwrap();
        fs::copy(&report, retained.join("report.json")).unwrap();
        fs::copy(&disputed, retained.join("disputed.bin")).unwrap();
        println!("Verified proof artifacts: {}", retained.display());
    }
}
