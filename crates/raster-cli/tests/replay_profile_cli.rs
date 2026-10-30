//! `cargo raster replay` through the real CLI
//! (`docs/proposals/tile-cost-estimator.md` §5, §6 step 6).
//!
//! The error paths and `replay report` run without a zkVM. The end-to-end test
//! replays hello-tiles in the RISC Zero executor, so it is ignored by default:
//! it needs the RISC Zero toolchain, and its first run compiles every tile
//! guest (about ten minutes). Run it with `-- --ignored`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use raster_analysis::replay_profile::{ReplayProfile, ReplayedCall, REPLAY_PROFILE_FILE};
use raster_core::cfs::CfsCoordinates;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root should exist")
        .to_path_buf()
}

fn hello_tiles_dir() -> PathBuf {
    workspace_root().join("examples/hello-tiles")
}

fn cargo_raster(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cargo-raster"))
        .current_dir(cwd)
        .arg("raster")
        .args(args)
        .output()
        .expect("cargo-raster should execute")
}

fn scratch(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("raster-replay-{tag}-{nanos}"));
    fs::create_dir_all(&dir).expect("scratch dir should be creatable");
    dir
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Everything the command printed, for asserting on error messages.
fn printed(output: &Output) -> String {
    format!(
        "{}{}",
        stdout(output),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_ok(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} should succeed\n{}",
        printed(output)
    );
}

fn path_arg(path: &Path) -> &str {
    path.to_str().expect("scratch paths are UTF-8")
}

#[test]
fn replay_profile_needs_a_completed_authenticated_run() {
    let run_dir = scratch("no-manifest");
    let output = cargo_raster(&run_dir, &["replay", "profile", path_arg(&run_dir)]);
    assert!(!output.status.success());
    assert!(
        printed(&output).contains("run.json not found"),
        "{}",
        printed(&output)
    );
    fs::remove_dir_all(run_dir).unwrap();
}

#[test]
fn replay_profile_rejects_a_malformed_run_manifest() {
    let run_dir = scratch("bad-manifest");
    fs::write(run_dir.join("run.json"), b"{").unwrap();
    let output = cargo_raster(&run_dir, &["replay", "profile", path_arg(&run_dir)]);
    assert!(!output.status.success());
    assert!(
        printed(&output).contains("malformed"),
        "{}",
        printed(&output)
    );
    fs::remove_dir_all(run_dir).unwrap();
}

#[test]
fn replay_report_prints_a_saved_profile_from_the_file_or_its_run_directory() {
    let run_dir = scratch("report");
    let mut profile = ReplayProfile::new(
        "run-7",
        ["greet".to_string(), "never_called".to_string()],
        10,
    );
    profile.complete = true;
    let greet = profile.tiles.get_mut("greet").unwrap();
    greet.record_size(120);
    greet.image_id = Some("ab".repeat(32));
    greet.replayed.push(ReplayedCall {
        exec_index: 4,
        coordinates: CfsCoordinates(vec![2, 1]),
        input_bytes: 100,
        output_bytes: 20,
        byte_rank: 1,
        user_cycles: 12_345,
        segments: 1,
        padded_cycles: 1 << 16,
        exec_ns: 20_000_000,
    });
    let file = run_dir.join(REPLAY_PROFILE_FILE);
    fs::write(&file, profile.to_json().unwrap()).unwrap();

    let expected = format!("{}\n", profile.to_text());
    for path in [&file, &run_dir] {
        let output = cargo_raster(&run_dir, &["replay", "report", path_arg(path)]);
        assert_ok(&output, "replay report");
        assert_eq!(stdout(&output), expected);
    }
    fs::remove_dir_all(run_dir).unwrap();
}

#[test]
fn replay_report_refuses_files_that_are_not_a_replay_profile() {
    let run_dir = scratch("not-a-report");
    let file = run_dir.join("profile.json");
    fs::write(&file, br#"{"kind": "zkvm-profile", "version": 3}"#).unwrap();
    let output = cargo_raster(&run_dir, &["replay", "report", path_arg(&file)]);
    assert!(!output.status.success());
    assert!(
        printed(&output).contains("Not a replay profile"),
        "{}",
        printed(&output)
    );
    fs::remove_dir_all(run_dir).unwrap();
}

#[test]
#[ignore = "replays hello-tiles in the RISC Zero executor; needs the guest toolchain"]
fn hello_tiles_run_then_replay_profile_and_report() {
    let example = hello_tiles_dir();
    let run = cargo_raster(
        &example,
        &[
            "run",
            "--input",
            "input.json",
            "--input-manifest",
            "input_manifest.json",
        ],
    );
    assert_ok(&run, "run");
    let run_dir = stdout(&run)
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("Run artifacts dir: "))
        .map(|dir| PathBuf::from(dir.trim()))
        .expect("run prints its artifacts dir");

    let profile_output = cargo_raster(&example, &["replay", "profile", path_arg(&run_dir)]);
    assert_ok(&profile_output, "replay profile");
    let profile =
        ReplayProfile::from_json(&fs::read(run_dir.join(REPLAY_PROFILE_FILE)).unwrap()).unwrap();
    assert!(profile.complete);
    assert!(profile.failure.is_none());

    // One row per tile the CFS declares, called or not.
    let cfs_path = scratch("cfs").join("cfs.json");
    assert_ok(
        &cargo_raster(&example, &["cfs", "--output", path_arg(&cfs_path)]),
        "cfs",
    );
    let cfs: serde_json::Value = serde_json::from_slice(&fs::read(&cfs_path).unwrap()).unwrap();
    let mut declared: Vec<&str> = cfs["tiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tile| tile["id"].as_str().unwrap())
        .collect();
    declared.sort_unstable();
    let reported: Vec<&str> = profile.tiles.keys().map(String::as_str).collect();
    assert_eq!(reported, declared);

    for (id, tile) in &profile.tiles {
        let histogram: u64 = tile.size_histogram.iter().map(|bucket| bucket.calls).sum();
        assert_eq!(histogram, tile.calls_recorded, "{id}: every call is sized");
        assert_eq!(
            tile.replayed.len() as u64,
            tile.calls_recorded.min(10),
            "{id}: the top 10 calls, or all of them"
        );
        for call in &tile.replayed {
            assert!(call.segments >= 1, "{id}: {call:?}");
            assert!(call.padded_cycles >= call.user_cycles, "{id}: {call:?}");
        }
        // The replayed calls are the largest ones.
        let mut ranks: Vec<u64> = tile.replayed.iter().map(|call| call.byte_rank).collect();
        ranks.sort_unstable();
        assert_eq!(ranks, (1..=tile.replayed.len() as u64).collect::<Vec<_>>());
    }
    assert!(
        profile.tiles.values().any(|tile| tile.calls_recorded > 10),
        "hello-tiles has a tile called more than 10 times, so the cut is exercised"
    );

    let report = cargo_raster(&example, &["replay", "report", path_arg(&run_dir)]);
    assert_ok(&report, "replay report");
    assert_eq!(stdout(&report), format!("{}\n", profile.to_text()));
    assert!(stdout(&profile_output).ends_with(&stdout(&report)));
}
