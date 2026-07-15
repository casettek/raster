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

fn run_hello_tiles_directly() -> Output {
    Command::new("cargo")
        .current_dir(hello_tiles_dir())
        .env_remove(raster_runtime::TRACE_PATH_ENV)
        .env_remove(raster_runtime::TRACE_FORMAT_ENV)
        .args([
            "run",
            "--release",
            "--",
            "--input",
            "input.json",
            "--input-manifest",
            "input_manifest.json",
        ])
        .output()
        .expect("plain hello-tiles command should execute")
}

fn unique_commit_path() -> String {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    format!("target/recur-audit-{suffix}.bin")
}

fn unique_artifact_dir() -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be after epoch")
        .as_nanos();
    let dir = hello_tiles_dir().join(format!("target/cli-test-artifacts-{suffix}"));
    fs::create_dir_all(&dir).expect("artifact dir should be created");
    dir
}

fn extract_stdout_path(stdout: &str, prefix: &str) -> String {
    stdout
        .lines()
        .find_map(|line| line.trim_start().strip_prefix(prefix).map(str::trim))
        .unwrap_or_else(|| panic!("missing '{prefix}' in stdout:\n{stdout}"))
        .to_string()
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
    assert!(stdout.contains("tile_coordinates: CfsCoordinates([10, 0])"));
    assert!(stdout.contains("tile_coordinates: CfsCoordinates([10, 1])"));
    assert!(stdout.contains("recur_tile_coordinates: CfsCoordinates([10])"));
    assert!(stdout.contains("tile_coordinates: CfsCoordinates([12, 0])"));
    assert!(stdout.contains("recur_tile_coordinates: CfsCoordinates([12])"));
}

#[test]
fn direct_hello_tiles_run_does_not_emit_trace_events_to_stdout() {
    let output = run_hello_tiles_directly();
    assert!(
        output.status.success(),
        "plain cargo run should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("SequenceStart") && !stdout.contains("TileExec"),
        "plain cargo run should not emit trace events to stdout:\n{stdout}"
    );
}

#[test]
fn hello_tiles_audit_accepts_recur_trace_commitment() {
    let commit_path = unique_commit_path();
    let commit_output = run_hello_tiles(&[
        "--commit",
        &commit_path,
        "--fraud-proof-window-size",
        "8",
    ]);
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

#[test]
fn hello_tiles_run_uses_distinct_run_scoped_artifact_dirs() {
    let first = run_hello_tiles(&[]);
    let second = run_hello_tiles(&[]);
    assert!(
        first.status.success(),
        "first run should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr),
    );
    assert!(
        second.status.success(),
        "second run should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr),
    );

    let first_stdout = String::from_utf8_lossy(&first.stdout);
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    let first_run_dir = extract_stdout_path(&first_stdout, "Run artifacts dir: ");
    let second_run_dir = extract_stdout_path(&second_stdout, "Run artifacts dir: ");
    let first_trace_path = extract_stdout_path(&first_stdout, "Trace path: ");
    let second_trace_path = extract_stdout_path(&second_stdout, "Trace path: ");

    assert_ne!(first_run_dir, second_run_dir);
    assert_ne!(first_trace_path, second_trace_path);
    assert!(PathBuf::from(&first_run_dir).exists());
    assert!(PathBuf::from(&second_run_dir).exists());
    assert!(PathBuf::from(&first_trace_path).exists());
    assert!(PathBuf::from(&second_trace_path).exists());
    assert!(first_trace_path.ends_with("trace.bin"));
    assert!(second_trace_path.ends_with("trace.bin"));
}

#[test]
fn hello_tiles_run_can_use_json_trace_format() {
    let output = run_hello_tiles(&["--trace-format", "json"]);
    assert!(
        output.status.success(),
        "json trace run should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trace_path = extract_stdout_path(&stdout, "Trace path: ");
    assert!(trace_path.ends_with("trace.ndjson"));

    let trace_contents =
        fs::read_to_string(&trace_path).expect("json trace file should be readable text");
    let first_line = trace_contents
        .lines()
        .next()
        .expect("json trace should contain at least one event");
    let parsed: serde_json::Value =
        serde_json::from_str(first_line).expect("json trace line should parse as JSON");
    assert!(
        parsed.get("SequenceStart").is_some()
            || parsed.get("TileExec").is_some()
            || parsed.get("RecurTileExec").is_some()
            || parsed.get("SequenceEnd").is_some(),
        "json trace line should be a trace event: {first_line}"
    );
}

#[test]
fn analyze_requires_explicit_run_scoped_path() {
    let output = Command::new(cargo_raster_bin())
        .current_dir(hello_tiles_dir())
        .args(["raster", "analyze"])
        .output()
        .expect("analyze command should execute");

    assert!(
        !output.status.success(),
        "analyze without a path should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Provide a profile path"));
}

#[test]
fn analyze_accepts_explicit_profile_path() {
    let artifact_dir = unique_artifact_dir();
    let profile_path = artifact_dir.join("profile.json");
    fs::write(
        &profile_path,
        r#"{
  "version": 3,
  "run_id": "test-run",
  "program_total_duration_ns": 0,
  "records": []
}"#,
    )
    .expect("profile should be written");

    let output = Command::new(cargo_raster_bin())
        .current_dir(hello_tiles_dir())
        .args([
            "raster",
            "analyze",
            profile_path
                .to_str()
                .expect("profile path should be valid utf-8"),
        ])
        .output()
        .expect("analyze command should execute");

    assert!(
        output.status.success(),
        "analyze with explicit path should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Analyzing profile:"));
    assert!(stdout.contains(profile_path.to_str().unwrap()));
}

#[test]
fn analyze_follow_accepts_explicit_stream_path() {
    let artifact_dir = unique_artifact_dir();
    let stream_path = artifact_dir.join("profile.ndjson");
    fs::write(
        &stream_path,
        concat!(
            "{\"RunStarted\":{\"run_id\":\"test-run\"}}\n",
            "{\"RunFinished\":{\"run_id\":\"test-run\",\"program_total_duration_ns\":0}}\n"
        ),
    )
    .expect("stream should be written");

    let output = Command::new(cargo_raster_bin())
        .current_dir(hello_tiles_dir())
        .args([
            "raster",
            "analyze",
            "--follow",
            stream_path
                .to_str()
                .expect("stream path should be valid utf-8"),
        ])
        .output()
        .expect("follow command should execute");

    assert!(
        output.status.success(),
        "follow with explicit path should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Following profile stream:"));
    assert!(stdout.contains(stream_path.to_str().unwrap()));
}

#[test]
fn hello_tiles_run_forwards_requested_build_features() {
    let output = run_hello_tiles(&["--features", "profiling"]);
    assert!(
        output.status.success(),
        "run with forwarded features should succeed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let profile_path = extract_stdout_path(&stdout, "Execution profile saved to: ");
    let profile_stream_path = extract_stdout_path(&stdout, "Live profile stream saved to: ");

    assert!(stdout.contains("Follow with: cargo raster analyze --follow"));
    assert!(PathBuf::from(profile_path).exists());
    assert!(PathBuf::from(profile_stream_path).exists());
}
