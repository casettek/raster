use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use raster_analysis::replay_profile::{InvocationLocation, ReplayProfile};
use raster_core::cfs::CfsCoordinates;

fn example() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/hello-tiles")
        .canonicalize()
        .unwrap()
}

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cargo-raster"))
        .current_dir(example())
        .args(["raster"])
        .args(args)
        .output()
        .unwrap()
}

fn scratch() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = example().join(format!("target/replay-profile-test-{nonce}"));
    fs::create_dir_all(&path).unwrap();
    path
}

fn assert_ok(output: &Output) -> String {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn reported_path(stdout: &str, label: &str) -> PathBuf {
    PathBuf::from(
        stdout
            .lines()
            .find_map(|line| line.trim().strip_prefix(label))
            .unwrap(),
    )
}

#[test]
fn unknown_tiles_are_rejected_before_build_or_native_execution() {
    for args in [
        vec!["run", "--zkvm-profile", "not_a_real_tile"],
        vec!["run", "--profile", "replay", "--tile", "not_a_real_tile"],
        vec!["run", "--profile", "zkvm", "--tile", "not_a_real_tile"],
    ] {
        let output = cli(&args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("not_a_real_tile"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Building project"));
    }
}

#[test]
fn incompatible_profile_modes_fail_before_project_discovery() {
    for args in [
        vec!["--profile", "native", "--tile", "a"],
        vec![
            "--profile",
            "replay",
            "--tile",
            "a",
            "--audit",
            "commit.bin",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_cargo-raster"))
            .current_dir(std::env::temp_dir())
            .args(["raster", "run"])
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("--profile replay"), "{stderr}");
        assert!(!stderr.contains("panicked"), "{stderr}");
        assert!(output.stdout.is_empty());
    }
}

/// Exercise the documented workflow outside Raster's Cargo workspace. This
/// consumer deliberately has no `profiling` forwarding feature of its own.
#[test]
#[ignore = "builds a separate consumer project and requires the RISC Zero guest toolchain"]
fn separate_repository_native_then_replay_workflow() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("raster-profile-consumer-{nonce}"));
    fs::create_dir_all(root.join("src")).unwrap();
    let raster = example()
        .join("../../crates/raster")
        .canonicalize()
        .unwrap();
    fs::write(
        root.join("Cargo.toml"),
        format!(
            r#"[package]
name = "profile-consumer"
version = "0.1.0"
edition = "2021"

[features]
default = ["std"]
std = ["raster/std"]

[dependencies]
raster = {{ path = {raster:?}, default-features = false }}
"#
        ),
    )
    .unwrap();
    fs::write(
        root.join("src/lib.rs"),
        r#"#![no_std]
use raster::prelude::*;
#[tile]
pub fn double(x: u64) -> u64 { x * 2 }
"#,
    )
    .unwrap();
    fs::write(
        root.join("src/main.rs"),
        r#"use raster::prelude::*;
use profile_consumer::*;
#[sequence]
fn main() -> u64 {
    let first = call!(double, 21u64);
    call!(double, first)
}
"#,
    )
    .unwrap();
    let run = |args: &[&str]| {
        assert_ok(
            &Command::new(env!("CARGO_BIN_EXE_cargo-raster"))
                .current_dir(&root)
                .env_remove("CARGO_TARGET_DIR")
                .args(["raster"])
                .args(args)
                .output()
                .unwrap(),
        )
    };

    let native = run(&["run", "--profile", "native"]);
    assert!(native.contains("Profile Summary\n  Type: native"));
    assert!(native.contains("Analyze with: cargo raster analyze"));
    assert!(!native.contains("exec_index:"));
    let native_path = reported_path(&native, "Execution profile saved to: ");
    let native_stream = reported_path(&native, "Live profile stream saved to: ");
    assert!(native_stream.exists());
    let metrics = run(&["analyze", native_path.to_str().unwrap(), "--format", "json"]);
    let metrics: serde_json::Value = serde_json::from_str(&metrics).unwrap();
    assert_eq!(metrics["tile_metrics"]["double"]["invocations"], 2);
    assert!(
        metrics["tile_metrics"]["double"]["total_duration_ns"]
            .as_u64()
            .unwrap()
            > 0
    );

    let legacy = run(&["run", "--features", "raster/profiling", "--verbose"]);
    assert!(legacy.contains("Type: native"));
    assert!(legacy.contains("exec_index:"));

    let replay = run(&["run", "--profile", "replay", "--tile", "double,double"]);
    assert!(replay.contains("Profile Summary\n  Type: replay"));
    assert!(replay.contains("Analyze with: cargo raster analyze"));
    assert!(!replay.contains("exec_index:"));
    let replay_path = reported_path(&replay, "Execution profile saved to: ");
    let json = run(&["analyze", replay_path.to_str().unwrap(), "--format", "json"]);
    let profile: ReplayProfile = serde_json::from_str(&json).unwrap();
    assert!(profile.complete);
    assert_eq!(profile.tiles.len(), 1);
    assert_eq!(profile.tiles["double"].invocations, 2);
    assert_eq!(profile.tiles["double"].profiled_invocations, 2);
    assert!(profile.total_guest_cycles > 0);
    assert_eq!(
        fs::read(native_path.parent().unwrap().join("output.bin")).unwrap(),
        fs::read(replay_path.parent().unwrap().join("output.bin")).unwrap(),
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn analyze_renders_replay_json_and_preserves_native_profiles() {
    let scratch = scratch();
    let path = scratch.join("profile.json");
    let mut profile = ReplayProfile::new("test-run".into(), ["tile".into()]);
    profile.tiles.get_mut("tile").unwrap().invocations = 1;
    profile
        .record(
            "tile",
            InvocationLocation {
                exec_index: 1,
                coordinates: CfsCoordinates(vec![2, 1]),
            },
            100,
        )
        .unwrap();
    profile.complete = true;
    fs::write(&path, serde_json::to_vec(&profile).unwrap()).unwrap();
    let text = assert_ok(&cli(&["analyze", path.to_str().unwrap()]));
    assert!(text.contains("Profile Summary\n  Type: replay"));
    assert!(text.contains("Hot Tiles"));
    assert!(text.contains("Total profiled-invocation guest cycles: 100"));
    assert!(text.contains("[2, 1]"));
    let json = assert_ok(&cli(&[
        "analyze",
        path.to_str().unwrap(),
        "--format",
        "json",
    ]));
    let decoded: ReplayProfile = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.run_id, "test-run");
    assert!(decoded.complete);
    assert_eq!(decoded.kind, "replay-profile");
    assert_eq!(decoded.version, 2);
    assert_eq!(decoded.invocation_limit, Some(128));

    // Old saved reports are still readable, regardless of their filename.
    let legacy_path = scratch.join("zkvm-profile.json");
    profile.kind = "zkvm-profile".into();
    profile.version = 1;
    let mut legacy_value = serde_json::to_value(&profile).unwrap();
    legacy_value
        .as_object_mut()
        .unwrap()
        .remove("invocation_limit");
    fs::write(&legacy_path, serde_json::to_vec(&legacy_value).unwrap()).unwrap();
    let legacy = assert_ok(&cli(&["analyze", legacy_path.to_str().unwrap()]));
    assert!(legacy.contains("Type: replay"));
    assert!(legacy.contains("Total profiled-invocation guest cycles: 100"));
    assert!(!legacy.contains("Limit: first"));
    let legacy_json = assert_ok(&cli(&[
        "analyze",
        legacy_path.to_str().unwrap(),
        "--format",
        "json",
    ]));
    let legacy_decoded: ReplayProfile = serde_json::from_str(&legacy_json).unwrap();
    assert_eq!(legacy_decoded.invocation_limit, None);
    assert_eq!(
        legacy_decoded.total_guest_cycles,
        decoded.total_guest_cycles
    );
    profile.kind = "replay-profile".into();
    profile.version = 999;
    fs::write(&path, serde_json::to_vec(&profile).unwrap()).unwrap();
    assert!(!cli(&["analyze", path.to_str().unwrap()]).status.success());
    fs::write(
        &path,
        br#"{"version":3,"run_id":"native-run","program_total_duration_ns":100,"records":[]}"#,
    )
    .unwrap();
    let native = assert_ok(&cli(&["analyze", path.to_str().unwrap()]));
    assert!(native.contains("Profile Summary"));
    assert!(native.contains("native-run"));
    assert!(native.contains("Type: native"));
    let json = assert_ok(&cli(&[
        "analyze",
        path.to_str().unwrap(),
        "--format",
        "json",
    ]));
    let native_metrics: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(native_metrics["run_id"], "native-run");
    fs::remove_dir_all(scratch).unwrap();
}

/// Real executor coverage and timing, explicitly opt-in because it builds
/// guest ELFs. No proofs are generated. Run with --ignored --nocapture.
#[test]
#[ignore = "requires the RISC Zero guest toolchain and builds release fixtures"]
fn profiles_ordinary_and_recursive_invocations_and_measures_executor_overhead() {
    use raster_backend_risc0::Risc0Backend;
    use raster_compiler::Project;
    use raster_core::trace::TraceEvent;
    use raster_prover::replay::Replayer;

    let scratch = scratch();
    let generated = Command::new("cargo")
        .current_dir(example())
        .args([
            "run",
            "--release",
            "--features",
            "gen-input",
            "--bin",
            "gen_input",
            "--",
        ])
        .arg(&scratch)
        .output()
        .unwrap();
    assert_ok(&generated);
    let input = scratch.join("input.json");
    let manifest = scratch.join("input_manifest.json");
    let baseline_commit = scratch.join("baseline.bin");
    // Build outside the native timing interval.
    assert_ok(
        &Command::new("cargo")
            .current_dir(example())
            .args(["build", "--release"])
            .output()
            .unwrap(),
    );
    let native_start = Instant::now();
    let native = assert_ok(&cli(&[
        "run",
        "--input",
        input.to_str().unwrap(),
        "--input-manifest",
        manifest.to_str().unwrap(),
        "--trace-format",
        "json",
        "--commit",
        baseline_commit.to_str().unwrap(),
        "--fraud-proof-window-size",
        "8",
    ]));
    let native_elapsed = native_start.elapsed();
    let trace = fs::read_to_string(reported_path(&native, "Trace path: ")).unwrap();
    let selected = ["greet", "build_recur_draft_greeting"];
    let mut witnesses = std::collections::BTreeMap::<String, Vec<(Vec<u8>, Vec<u8>)>>::new();
    for line in trace.lines() {
        let event: TraceEvent = serde_json::from_str(line).unwrap();
        if let TraceEvent::TileExec(record) | TraceEvent::RecurTileIterationExec(record) = event {
            if selected.contains(&record.fn_name.as_str()) {
                witnesses.entry(record.fn_name.clone()).or_default().push((
                    record.input_data().unwrap().to_vec(),
                    record.output_data().unwrap().to_vec(),
                ));
            }
        }
    }
    // A fresh artifact directory makes preparation a cold build; this does
    // not delete or invalidate any developer's existing cached artifacts.
    let mut project = Project::new(example()).unwrap();
    project.output_dir = scratch.join("cold-guests");
    let backend =
        Risc0Backend::new(project.output_dir.clone()).with_user_crate(project.root_dir.clone());
    let replayer = Replayer::new(&backend, &project);
    let mut expected = std::collections::BTreeMap::new();
    println!(
        "Native run + trace/fingerprint (cached native build): {:.3}s",
        native_elapsed.as_secs_f64()
    );
    for tile in selected {
        let start = Instant::now();
        let prepared = replayer.prepare_profile(tile).unwrap();
        let compilation = start.elapsed();
        let start = Instant::now();
        let mut cycles = Vec::new();
        for (input, output) in &witnesses[tile] {
            cycles.push(prepared.profile(input, output).unwrap());
        }
        let replay = start.elapsed();
        println!("{tile}: cold compilation {:.3}s; {} replays {:.3}s ({:.3}ms/invocation); guest cycles {:?}",
            compilation.as_secs_f64(), cycles.len(), replay.as_secs_f64(),
            replay.as_secs_f64() * 1000.0 / cycles.len() as f64, cycles);
        expected.insert(tile, cycles);
    }
    let profile_commit = scratch.join("profiled.bin");
    let profiled = assert_ok(&cli(&[
        "run",
        "--input",
        input.to_str().unwrap(),
        "--input-manifest",
        manifest.to_str().unwrap(),
        "--profile",
        "replay",
        "--tile",
        "greet,build_recur_draft_greeting,count_to",
        "--tile",
        "greet",
        "--commit",
        profile_commit.to_str().unwrap(),
        "--fraud-proof-window-size",
        "8",
    ]));
    let report: ReplayProfile = serde_json::from_slice(
        &fs::read(reported_path(&profiled, "Execution profile saved to: ")).unwrap(),
    )
    .unwrap();
    assert!(report.complete);
    assert_eq!(report.tiles.len(), 3);
    assert_eq!(report.tiles["count_to"].invocations, 0);
    assert!(report.tiles["count_to"].image_id.is_none());
    assert!(!profiled.contains("tile_coordinates:"));
    for tile in selected {
        let stats = &report.tiles[tile];
        assert!(
            stats.invocations > 1,
            "fixture should exercise multiple invocations of {tile}"
        );
        assert_eq!(stats.invocations, expected[tile].len() as u64);
        assert_eq!(stats.profiled_invocations, stats.invocations);
        assert_eq!(stats.total_guest_cycles, expected[tile].iter().sum::<u64>());
        assert_eq!(stats.max_guest_cycles, expected[tile].iter().copied().max());
        assert!(stats.image_id.is_some());
    }
    assert_eq!(
        fs::read(baseline_commit).unwrap(),
        fs::read(profile_commit).unwrap()
    );
    fs::remove_dir_all(scratch).unwrap();
}
