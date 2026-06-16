use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

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

fn cargo_raster_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cargo-raster")
}

fn run_hello_tiles(extra_args: &[&str]) -> Output {
    let mut command = Command::new(cargo_raster_bin());
    command.current_dir(hello_tiles_dir()).args([
        "raster",
        "run",
        "--input",
        "input.json",
        "--input-manifest",
        "input_manifest.json",
    ]);
    command.args(extra_args);
    command
        .output()
        .expect("hello-tiles command should execute")
}

fn unique_commit_path() -> String {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    format!("target/recur-audit-{suffix}.bin")
}

#[test]
fn hello_tiles_run_reports_recur_iteration_coordinates() {
    let output = run_hello_tiles(&[]);
    assert!(
        output.status.success(),
        "cargo-raster run should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("tile_coordinates: CfsCoordinates([9, 0])"));
    assert!(stdout.contains("tile_coordinates: CfsCoordinates([9, 1])"));
    assert!(stdout.contains("recur_coordinates: CfsCoordinates([9])"));
    assert!(stdout.contains("tile_coordinates: CfsCoordinates([11, 0])"));
    assert!(stdout.contains("recur_coordinates: CfsCoordinates([11])"));
}

#[test]
fn hello_tiles_audit_accepts_recur_trace_commitment() {
    let commit_path = unique_commit_path();
    let commit_output = run_hello_tiles(&["--commit", &commit_path]);
    assert!(
        commit_output.status.success(),
        "commit run should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&commit_output.stdout),
        String::from_utf8_lossy(&commit_output.stderr),
    );

    let audit_output = run_hello_tiles(&["--audit", &commit_path]);
    let _ = fs::remove_file(hello_tiles_dir().join(&commit_path));
    assert!(
        audit_output.status.success(),
        "audit run should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&audit_output.stdout),
        String::from_utf8_lossy(&audit_output.stderr),
    );

    let stdout = String::from_utf8_lossy(&audit_output.stdout);
    assert!(stdout.contains("Verification Success"));
}
