//! End-to-end coverage for artifact inspection
//! (`docs/proposals/artifact-inspection.md`).
//!
//! Drives the committed three-stage `examples/chain-example` chain through the
//! real CLI, then reads what it produced back — the specific gap the proposal
//! opens on, where the only way to see a stage's output was `strings(1)`.
//!
//! The load-bearing test here is [`show_and_show_output_render_identically`]:
//! `--show-output` is meant to be sugar over `cargo raster show`, and the only
//! thing keeping that true is that the two agree character for character.
//!
//! Each test runs in its own scratch working directory — the chains root is
//! derived from the current directory, so this keeps artifacts out of the repo
//! while still using the committed manifest and fixtures.

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

fn cargo_raster_bin() -> &'static str {
    env!("CARGO_BIN_EXE_cargo-raster")
}

fn chain_manifest() -> PathBuf {
    workspace_root().join("examples/chain-example/Raster.toml")
}

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("raster-show-{tag}-{nanos}"));
        fs::create_dir_all(&dir).expect("scratch dir should be creatable");
        Self { dir }
    }

    fn chain(&self, extra_args: &[&str]) -> Output {
        Command::new(cargo_raster_bin())
            .current_dir(&self.dir)
            .args(["raster", "chain", "run"])
            .arg(chain_manifest())
            .arg("--no-auth")
            .args(extra_args)
            .output()
            .expect("chain command should execute")
    }

    fn show(&self, args: &[&str]) -> Output {
        Command::new(cargo_raster_bin())
            .current_dir(&self.dir)
            .args(["raster", "show"])
            .args(args)
            .output()
            .expect("show command should execute")
    }

    fn run_dir(&self) -> PathBuf {
        self.dir.join("target/raster/chains-no-auth/latest")
    }

    fn output_bin(&self, stage: &str) -> PathBuf {
        self.run_dir().join(stage).join("output.bin")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn assert_ok(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} should succeed\n--- stdout ---\n{}\n--- stderr ---\n{}",
        stdout_of(output),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// The value block `--show-output` prints, with its two-space indent removed,
/// so it can be compared against `show`'s own rendering.
fn shown_output_block(text: &str) -> String {
    let start = text
        .find("Program output value")
        .expect("--show-output should print a value block");
    let body = text[start..]
        .lines()
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with("commitment "))
        .map(|line| line.strip_prefix("  ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    body.trim_end().to_string()
}

/// `show`'s own value rendering, without its trailing commitment line.
fn show_value_block(text: &str) -> String {
    text.lines()
        .take_while(|line| !line.starts_with("commitment "))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

/// The gap in the proposal's §Problem, closed: a stage's output read back as a
/// typed, structured value rather than through `strings(1)`.
#[test]
fn show_reads_a_stage_output_back() {
    let fixture = Fixture::new("read-back");
    assert_ok(&fixture.chain(&[]), "chain run");

    let shown = fixture.show(&[fixture.output_bin("report").to_str().unwrap()]);
    assert_ok(&shown, "show report output");
    let text = stdout_of(&shown);

    // Field names and the string leaves `strings(1)` could already see …
    assert!(
        text.contains("title: \"Pipeline report for sensor-A\""),
        "expected the report title\n{text}"
    );
    // … and the list structure it could not.
    assert!(
        text.contains("lines: [4] ["),
        "expected a 4-element list\n{text}"
    );
    assert!(
        text.contains("commitment ") && text.contains("✓ matches .rindex"),
        "expected an integrity line\n{text}"
    );
}

/// Integers are little-endian binary and invisible to `strings(1)`. This is the
/// half of the artifact that had no reader at all.
#[test]
fn show_renders_integers_with_their_width() {
    let fixture = Fixture::new("integers");
    assert_ok(&fixture.chain(&[]), "chain run");

    let shown = fixture.show(&[fixture.output_bin("aggregate").to_str().unwrap()]);
    assert_ok(&shown, "show aggregate output");
    let text = stdout_of(&shown);

    // `count: 6` as a typed leaf — the type name comes from the index, per §4.1.
    assert!(text.contains("count: 6u64"), "expected count: 6u64\n{text}");
    assert!(text.contains("sum: 353u64"), "expected sum: 353u64\n{text}");
    assert!(text.contains("max: 88u64"), "expected max: 88u64\n{text}");
}

/// What keeps `--show-output` sugar rather than a second code path.
#[test]
fn show_and_show_output_render_identically() {
    let fixture = Fixture::new("agree");
    let ran = fixture.chain(&["--show-output"]);
    assert_ok(&ran, "chain run --show-output");

    let shown = fixture.show(&[fixture.output_bin("report").to_str().unwrap()]);
    assert_ok(&shown, "show report output");

    assert_eq!(
        shown_output_block(&stdout_of(&ran)),
        show_value_block(&stdout_of(&shown)),
        "`chain run --show-output` and `show` must render the same value"
    );
}

/// §6: final stage only, so a long chain does not print one value per stage.
#[test]
fn chain_show_output_prints_only_the_final_stage() {
    let fixture = Fixture::new("final-only");
    let ran = fixture.chain(&["--show-output"]);
    assert_ok(&ran, "chain run --show-output");
    let text = stdout_of(&ran);

    assert_eq!(
        text.matches("Program output value").count(),
        1,
        "exactly one value block for a three-stage chain\n{text}"
    );
    assert!(
        text.contains("Program output value (report):"),
        "the block should name the final stage\n{text}"
    );
}

/// The dev loop the flag exists for: re-run one middle stage and see it, with
/// no separate `chain show` command. The stage you re-ran is the last one that
/// ran, so "final stage" and "the stage I am working on" coincide.
#[test]
fn stage_rerun_shows_that_stage() {
    let fixture = Fixture::new("stage-rerun");
    assert_ok(&fixture.chain(&[]), "chain run");

    let rerun = fixture.chain(&["--stage", "aggregate", "--show-output"]);
    assert_ok(&rerun, "chain run --stage aggregate --show-output");
    let text = stdout_of(&rerun);

    assert!(
        text.contains("Program output value (aggregate):"),
        "the re-run stage's own output should be shown\n{text}"
    );
    assert!(
        !text.contains("(report)"),
        "a stage re-run should not show a downstream stage\n{text}"
    );
}

/// JSON composes with `jq` and with tests, and is untruncated by default.
#[test]
fn show_emits_json() {
    let fixture = Fixture::new("json");
    assert_ok(&fixture.chain(&[]), "chain run");

    let shown = fixture.show(&[
        fixture.output_bin("report").to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_ok(&shown, "show --format json");

    let value: serde_json::Value =
        serde_json::from_str(&stdout_of(&shown)).expect("output should be valid JSON");
    assert_eq!(value["title"], "Pipeline report for sensor-A");
    assert_eq!(
        value["lines"]
            .as_array()
            .expect("lines should be an array")
            .len(),
        4
    );
}

/// Truncation is stated, never silent.
#[test]
fn show_states_its_truncation() {
    let fixture = Fixture::new("truncate");
    assert_ok(&fixture.chain(&[]), "chain run");

    let shown = fixture.show(&[
        fixture.output_bin("report").to_str().unwrap(),
        "--max-list",
        "2",
    ]);
    assert_ok(&shown, "show --max-list 2");
    let text = stdout_of(&shown);

    // The true length stays visible even though only two elements are printed.
    assert!(text.contains("lines: [4] ["), "true length kept\n{text}");
    assert!(
        text.contains("… 2 more elements"),
        "truncation should be visible\n{text}"
    );
}

/// The structural fallback is deferred (§2), so a missing index refuses — and
/// says which path it looked for, because the usual cause is a misplaced index
/// rather than an absent one.
#[test]
fn show_refuses_without_an_index_and_names_the_path() {
    let fixture = Fixture::new("no-index");
    assert_ok(&fixture.chain(&[]), "chain run");

    let orphan = fixture.dir.join("orphan.bin");
    fs::copy(fixture.output_bin("report"), &orphan).expect("payload should be copyable");

    let shown = fixture.show(&[orphan.to_str().unwrap()]);
    assert!(
        !shown.status.success(),
        "show should fail without an index\n{}",
        stdout_of(&shown)
    );
    let message = String::from_utf8_lossy(&shown.stderr).to_string();
    assert!(
        message.contains("orphan.rindex"),
        "the error should name the index path it looked for\n{message}"
    );
}

/// Corrupt a decodable artifact so its payload no longer matches its index.
///
/// A flipped bit inside a string leaf keeps the *shape* valid — the index still
/// describes a struct with the same fields — so the artifact decodes cleanly
/// and only the commitment reveals that it is not what was committed. That is
/// the case a viewer can silently get wrong.
fn corrupt_payload(fixture: &Fixture) -> PathBuf {
    let payload = fixture.output_bin("report");
    let mut bytes = fs::read(&payload).expect("payload should be readable");
    let at = bytes
        .windows(7)
        .position(|window| window == b"sensor-")
        .expect("the title should be in the payload");
    bytes[at] ^= 0x01;
    fs::write(&payload, &bytes).expect("payload should be writable");
    payload
}

/// A byte-flipped payload renders — and says it is not what was committed.
/// Refusing to show a corrupt artifact would remove the only tool for finding
/// out why it is corrupt, so it renders *and* fails.
#[test]
fn show_reports_a_commitment_mismatch() {
    let fixture = Fixture::new("mismatch");
    assert_ok(&fixture.chain(&[]), "chain run");
    let payload = corrupt_payload(&fixture);

    let shown = fixture.show(&[payload.to_str().unwrap()]);
    let text = stdout_of(&shown);

    assert!(
        text.contains("MISMATCH"),
        "a flipped byte should be reported\n{text}"
    );
    assert!(
        text.contains("title: "),
        "the value should still render\n{text}"
    );
    assert!(
        !shown.status.success(),
        "a mismatch must not exit successfully\n{text}"
    );
}

/// The JSON path must not be the quiet one. Text mode reported a mismatch while
/// `--format json` printed an apparently valid document and exited 0 — a
/// machine consumer would have accepted a corrupt artifact.
#[test]
fn json_reports_a_commitment_mismatch_and_fails() {
    let fixture = Fixture::new("mismatch-json");
    assert_ok(&fixture.chain(&[]), "chain run");
    let payload = corrupt_payload(&fixture);

    let shown = fixture.show(&[payload.to_str().unwrap(), "--format", "json"]);
    let stdout = stdout_of(&shown);
    let stderr = String::from_utf8_lossy(&shown.stderr).to_string();

    assert!(
        !shown.status.success(),
        "a mismatch must not exit successfully\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stderr.contains("MISMATCH"),
        "the mismatch belongs on stderr, where it cannot corrupt the document\n{stderr}"
    );
    // stdout stays a single parseable document, so `show … | jq` still works
    // and the diagnosis is not smuggled into the data.
    serde_json::from_str::<serde_json::Value>(&stdout)
        .expect("stdout should remain valid JSON even when the artifact is corrupt");
}

/// The healthy JSON path stays clean: nothing but the document on stdout.
#[test]
fn json_keeps_stdout_free_of_the_integrity_line() {
    let fixture = Fixture::new("json-clean");
    assert_ok(&fixture.chain(&[]), "chain run");

    let shown = fixture.show(&[
        fixture.output_bin("report").to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_ok(&shown, "show --format json");
    let stdout = stdout_of(&shown);

    assert!(
        !stdout.contains("commitment"),
        "the integrity report must not land in the JSON document\n{stdout}"
    );
    serde_json::from_str::<serde_json::Value>(&stdout).expect("stdout should be valid JSON");
}
