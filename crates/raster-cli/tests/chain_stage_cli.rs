//! End-to-end coverage for per-stage chain execution
//! (`docs/proposals/chain-stage-execution.md`).
//!
//! Drives the committed three-stage `examples/chain-example` chain
//! (`normalize → aggregate → report`) through the real CLI. Three stages is the
//! minimum that exercises what this feature is for: a **middle** stage re-run,
//! and invalidation of **more than one** downstream stage.
//!
//! Each test runs in its own scratch working directory — the chains root is
//! derived from the current directory (`raster_target_dir`), so this keeps
//! artifacts out of the repo while still using the committed manifest and
//! fixtures.

use std::fs;
use std::path::{Path, PathBuf};
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

/// The committed chain manifest. Passed explicitly so the base directory
/// resolves to `examples/chain-example` (where the stage projects and fixtures live)
/// while the run directory follows the scratch cwd.
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
        let dir = std::env::temp_dir().join(format!("raster-pipeline-{tag}-{nanos}"));
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

    fn run_dir(&self) -> PathBuf {
        self.dir.join("target/raster/chains-no-auth/latest")
    }

    fn stage_dir(&self, stage: &str) -> PathBuf {
        self.run_dir().join(stage)
    }

    fn output_digest(&self, stage: &str) -> String {
        let bytes = fs::read(self.stage_dir(stage).join("output.bin"))
            .unwrap_or_else(|e| panic!("{stage}/output.bin should be readable: {e}"));
        hex_sha256(&bytes)
    }

    /// A full run of all three stages, asserted to succeed.
    fn run_whole_chain(&self) {
        assert_ok(&self.chain(&[]), "chain run");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
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

/// Pull a `"commitment": "…"` out of a manifest, for the entry named `key`.
fn commitment_of(path: &Path, key: &str) -> String {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{} should be readable: {e}", path.display()));
    let at = text
        .find(&format!("\"{key}\""))
        .unwrap_or_else(|| panic!("{} should declare {key}", path.display()));
    let marker = "\"commitment\": \"";
    let start = at
        + text[at..]
            .find(marker)
            .expect("entry should have a commitment")
        + marker.len();
    let end = start + text[start..].find('"').expect("commitment terminated");
    text[start..end].to_string()
}

fn produced(fixture: &Fixture, stage: &str) -> String {
    commitment_of(
        &fixture.stage_dir(stage).join("output_manifest.json"),
        "output",
    )
}

fn consumed(fixture: &Fixture, stage: &str, param: &str) -> String {
    commitment_of(&fixture.stage_dir(stage).join("input_manifest.json"), param)
}

/// Every link in the chain: what a stage published is what the next stage was
/// authorized against.
#[test]
fn whole_chain_run_links_every_stage() {
    let fixture = Fixture::new("links");
    fixture.run_whole_chain();

    assert_eq!(
        produced(&fixture, "normalize"),
        consumed(&fixture, "aggregate", "filtered"),
        "aggregate must be fed exactly what normalize committed"
    );
    assert_eq!(
        produced(&fixture, "aggregate"),
        consumed(&fixture, "report", "stats"),
        "report must be fed exactly what aggregate committed"
    );
}

/// The core claim, on the terminal stage: re-running one stage against
/// artifacts already on disk reproduces its output byte-for-byte. If
/// rehydration fed it anything but the producer's committed bytes, it shows
/// here.
#[test]
fn re_running_the_terminal_stage_reproduces_its_output() {
    let fixture = Fixture::new("terminal");
    fixture.run_whole_chain();
    let before = fixture.output_digest("report");
    let run_dir = fs::canonicalize(fixture.run_dir()).expect("latest should resolve");

    let output = fixture.chain(&["--stage", "report"]);
    assert_ok(&output, "chain run --stage report");

    assert_eq!(before, fixture.output_digest("report"));
    assert!(
        !stdout_of(&output).contains("invalidating"),
        "the terminal stage invalidates nothing\n{}",
        stdout_of(&output)
    );
    assert!(
        !stdout_of(&output).contains("▸ stage 1/3"),
        "only the named stage should run\n{}",
        stdout_of(&output)
    );
    assert_eq!(
        run_dir,
        fs::canonicalize(fixture.run_dir()).expect("latest should still resolve"),
        "--stage must not repoint latest"
    );
}

/// A middle stage: rehydrates from the stage before it, and invalidates only
/// what comes after. This is the case a two-stage chain cannot express.
#[test]
fn re_running_a_middle_stage_invalidates_only_what_follows() {
    let fixture = Fixture::new("middle");
    fixture.run_whole_chain();
    let before = fixture.output_digest("aggregate");

    let output = fixture.chain(&["--stage", "aggregate"]);
    assert_ok(&output, "chain run --stage aggregate");

    assert!(
        stdout_of(&output).contains("invalidating 1 downstream stage: report"),
        "should invalidate exactly the report stage\n{}",
        stdout_of(&output)
    );
    assert!(
        fixture.stage_dir("normalize").is_dir(),
        "the upstream stage must survive"
    );
    assert!(
        !fixture.stage_dir("report").exists(),
        "the downstream stage must be gone"
    );
    assert_eq!(
        before,
        fixture.output_digest("aggregate"),
        "a middle stage re-run alone must reproduce what it produced in sequence"
    );
}

/// The first stage invalidates every later stage, not just the next one.
#[test]
fn re_running_the_first_stage_invalidates_every_later_stage() {
    let fixture = Fixture::new("first");
    fixture.run_whole_chain();

    let output = fixture.chain(&["--stage", "normalize"]);
    assert_ok(&output, "chain run --stage normalize");

    assert!(
        stdout_of(&output).contains("invalidating 2 downstream stages: aggregate, report"),
        "both later stages should be invalidated, and named\n{}",
        stdout_of(&output)
    );
    assert!(!fixture.stage_dir("aggregate").exists());
    assert!(!fixture.stage_dir("report").exists());
}

/// Rebuilding a chain one stage at a time lands on the same result the
/// whole-chain run produced — the property that makes per-stage execution a
/// shortcut rather than a second execution semantics.
#[test]
fn rebuilding_stage_by_stage_converges_on_the_whole_chain_result() {
    let fixture = Fixture::new("converge");
    fixture.run_whole_chain();
    let whole_chain_report = fixture.output_digest("report");

    // Blow away everything downstream of stage 1, then walk it back up.
    assert_ok(
        &fixture.chain(&["--stage", "normalize"]),
        "--stage normalize",
    );
    assert!(!fixture.stage_dir("report").exists());
    assert_ok(
        &fixture.chain(&["--stage", "aggregate"]),
        "--stage aggregate",
    );
    assert_ok(&fixture.chain(&["--stage", "report"]), "--stage report");

    assert_eq!(
        whole_chain_report,
        fixture.output_digest("report"),
        "stage-by-stage rebuild must equal the whole-chain result"
    );
}

/// A stage whose producer has no artifact fails before running, and the error
/// names the stage that would fix it.
#[test]
fn a_stage_without_its_producer_says_what_to_run() {
    let fixture = Fixture::new("missing");
    fixture.run_whole_chain();
    fs::remove_dir_all(fixture.stage_dir("aggregate")).expect("stage dir should be removable");

    let output = fixture.chain(&["--stage", "report"]);
    assert!(!output.status.success(), "the stage should refuse to run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("has no output.bin"),
        "should report the missing artifact\n{stderr}"
    );
    assert!(
        stderr.contains("--stage aggregate"),
        "should name the stage to run first\n{stderr}"
    );
}

/// `--stage` is meaningless without a run to work inside.
#[test]
fn a_stage_run_needs_a_previous_whole_chain_run() {
    let fixture = Fixture::new("no-previous");
    let output = fixture.chain(&["--stage", "aggregate"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("run the whole chain once"),
        "should point at the whole-chain run\n{stderr}"
    );
}
