//! End-to-end coverage for `[[chain.repeat]]` expansion
//! (`docs/proposals/chain-repeat.md`).
//!
//! One property, checked through the real CLI: **a repeat block that expands to
//! a given stage list commits to exactly those stages.** `examples/chain-example`
//! ships two manifests describing the same three-stage chain — `Raster.toml`
//! writes every stage out, `Raster-repeat.toml` expresses the first as a
//! one-iteration repeat block. Their `StageCheckpoint`s must be byte-identical,
//! and both must audit.
//!
//! Their whole *chain digests* differ, deliberately: the digest covers
//! `ChainShape::spec_digest`, taken over the unexpanded manifest, so two
//! different manifests are two different chains even when they compute the same
//! thing. Comparing the stages rather than the digest is what isolates the claim
//! this test is making from the one it is not.
//!
//! The unit tests in `chain::expand` already compare expanded stage lists
//! directly; what this adds is that the **run loop** consumes an expanded list
//! the same way it consumes an authored one — that nothing between `expand` and
//! the written commitment can tell the two apart.
//!
//! Each test runs in its own scratch working directory, since the chains root is
//! derived from the current directory.

use raster_core::chain::ChainCommitment;
use raster_core::input::parse_scalar_leaf;
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

fn manifest(name: &str) -> PathBuf {
    workspace_root().join("examples/chain-example").join(name)
}

struct Scratch {
    dir: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("raster-repeat-{tag}-{nanos}"));
        fs::create_dir_all(&dir).expect("scratch dir should be creatable");
        Self { dir }
    }

    fn cargo_raster(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cargo-raster"))
            .current_dir(&self.dir)
            .args(["raster"])
            .args(args)
            .output()
            .expect("cargo-raster should execute")
    }

    /// Run a chain to completion, returning its reported digest.
    fn run(&self, manifest_name: &str) -> String {
        let out = self.cargo_raster(&[
            "chain",
            "run",
            manifest(manifest_name).to_str().expect("utf-8 path"),
            "--no-auth",
        ]);
        let stdout = assert_ok(&out, &format!("chain run {manifest_name}"));
        digest_line(&stdout, manifest_name)
    }

    /// The stage checkpoints of the run just performed, as canonical bytes.
    fn stage_checkpoints(&self) -> Vec<u8> {
        postcard::to_allocvec(&self.chain(false).stages).expect("checkpoints are serializable")
    }

    /// Run a chain authenticated — traces, `commit.bin` per stage, the lot.
    fn run_authenticated(&self, manifest_name: &str) -> String {
        let out = self.cargo_raster(&[
            "chain",
            "run",
            manifest(manifest_name).to_str().expect("utf-8 path"),
        ]);
        let stdout = assert_ok(&out, &format!("chain run {manifest_name} (authenticated)"));
        digest_line(&stdout, manifest_name)
    }

    fn run_root(&self, authenticated: bool) -> PathBuf {
        let root = if authenticated {
            "target/raster/chains"
        } else {
            "target/raster/chains-no-auth"
        };
        self.dir.join(root).join("latest")
    }

    fn latest_commitment(&self) -> PathBuf {
        self.run_root(false).join("chain-commitment")
    }

    fn chain(&self, authenticated: bool) -> ChainCommitment {
        let path = self.run_root(authenticated).join("chain-commitment");
        let bytes = fs::read(&path)
            .unwrap_or_else(|e| panic!("{} should be readable: {e}", path.display()));
        postcard::from_bytes(&bytes).expect("chain-commitment should decode")
    }

    /// The count the planner actually committed, read out of its artifact the
    /// same way the run loop reads it.
    fn planner_count(&self, authenticated: bool) -> u64 {
        let bytes = fs::read(self.run_root(authenticated).join("planner/output.bin"))
            .expect("planner/output.bin should exist");
        let (_, count) =
            parse_scalar_leaf(&bytes).expect("the planner's output is one unsigned scalar");
        count
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn assert_ok(output: &Output, what: &str) -> String {
    assert!(
        output.status.success(),
        "{what} failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn digest_line(stdout: &str, what: &str) -> String {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("chain digest: "))
        .unwrap_or_else(|| panic!("{what} printed no chain digest\n{stdout}"))
        .trim()
        .to_string()
}

#[test]
fn a_repeat_expressed_chain_commits_to_the_same_stages_as_its_unrolled_twin() {
    let unrolled = Scratch::new("unrolled");
    let expanded = Scratch::new("expanded");

    let unrolled_digest = unrolled.run("Raster.toml");
    let expanded_digest = expanded.run("Raster-repeat.toml");

    assert_eq!(
        unrolled.stage_checkpoints(),
        expanded.stage_checkpoints(),
        "a repeat block that expands to the same stages must commit to the same checkpoints — \
         expansion is a refactor of the manifest, not of what the chain computes"
    );

    // The converse, stated so it cannot rot into an accident: the chain digest
    // *does* move, because it covers the manifest the chain was expanded from.
    assert_ne!(
        unrolled_digest, expanded_digest,
        "the chain digest covers spec_digest, so two different manifests are two different chains"
    );
}

#[test]
fn a_repeat_expressed_chain_audits() {
    // Expansion has to be reproducible from the manifest alone, since `audit`
    // re-derives the stage list rather than reading it out of the commitment.
    let scratch = Scratch::new("audit");
    scratch.run("Raster-repeat.toml");

    let commitment = scratch.latest_commitment();
    let out = scratch.cargo_raster(&[
        "chain",
        "audit",
        manifest("Raster-repeat.toml").to_str().expect("utf-8 path"),
        commitment.to_str().expect("utf-8 path"),
    ]);
    let stdout = assert_ok(&out, "chain audit");
    assert!(stdout.contains("chain verified"), "{stdout}");
    assert!(stdout.contains("link     ✓  filtered ⇐ normalize"), "{stdout}");
}

// ---------------------------------------------------------------------------
// A chain whose length is decided at run time
// ---------------------------------------------------------------------------

const DYNAMIC: &str = "Raster-dynamic.toml";

#[test]
fn a_chain_expands_to_the_number_of_stages_its_planner_asked_for() {
    // The feature's reason to exist: `Raster-dynamic.toml` names no stage count
    // anywhere. The chain has as many `step` stages as the planner's committed
    // output says, and nothing in the manifest could have predicted it.
    let scratch = Scratch::new("dynamic");
    scratch.run(DYNAMIC);

    let count = scratch.planner_count(false) as usize;
    assert!(count > 0, "the fixture planner should ask for at least one step");

    let chain = scratch.chain(false);
    let names: Vec<&str> = chain.stages.iter().map(|s| s.name.as_str()).collect();

    let mut expected = vec!["planner".to_string()];
    expected.extend((0..count).map(|t| format!("step{t}")));
    expected.push("sink".to_string());
    assert_eq!(names, expected);

    // And the shape record says where that number came from, so a verifier can
    // re-derive it rather than take it on trust.
    assert_eq!(chain.shape.repeats.len(), 1);
    let steps = &chain.shape.repeats[0];
    assert_eq!(steps.name, "steps");
    assert_eq!(steps.resolved_count as usize, count);
    assert_eq!(
        steps.source_stage,
        Some(0),
        "the count came from stage 0, the planner"
    );
}

#[test]
fn a_dynamic_chain_audits_against_a_re_derived_shape() {
    // `chain audit` does not read the stage list out of the commitment. It
    // re-derives the count from the planner's own committed output, re-expands
    // the manifest, and compares — so a truncated chain fails here with no
    // prover involved.
    let scratch = Scratch::new("dynamic-audit");
    scratch.run(DYNAMIC);

    let out = scratch.cargo_raster(&[
        "chain",
        "audit",
        manifest(DYNAMIC).to_str().expect("utf-8 path"),
        scratch
            .latest_commitment()
            .to_str()
            .expect("utf-8 path"),
    ]);
    let stdout = assert_ok(&out, "chain audit (dynamic)");
    assert!(stdout.contains("chain verified"), "{stdout}");
    // The block's export resolved to its final iteration, and the sink is fed
    // from there rather than from a position anyone wrote down.
    let last = scratch.planner_count(false) - 1;
    assert!(
        stdout.contains(&format!("link     ✓  prev ⇐ step{last}")),
        "{stdout}"
    );
}

#[test]
fn a_dynamic_chain_commits_identically_in_both_postures() {
    // The claim `chain-io-commitment.md` rests on, extended to a chain whose
    // length is *derived*: a cheap run and an authenticated one must produce
    // byte-identical commitments. If expansion depended on anything the two
    // postures do differently, this is where it would show.
    let scratch = Scratch::new("dynamic-postures");

    let cheap = scratch.run(DYNAMIC);
    let authenticated = scratch.run_authenticated(DYNAMIC);

    assert_eq!(cheap, authenticated, "chain digests must match across postures");
    assert_eq!(
        postcard::to_allocvec(&scratch.chain(false)).unwrap(),
        postcard::to_allocvec(&scratch.chain(true)).unwrap(),
        "the whole commitment, not just its digest, must be identical"
    );
}

#[test]
fn a_stage_of_a_dynamic_chain_can_be_re_run_in_place() {
    // `chain-io-commitment.md` §3 makes `chain run --stage` the way a contested
    // stage's trace is produced on demand. That has to keep working when the
    // stage list is derived — which means the shape must be reconstructible
    // from the run directory, not only from a chain-commitment, because a run
    // that could not resolve program identity writes no commitment at all.
    let scratch = Scratch::new("dynamic-stage");
    scratch.run(DYNAMIC);

    let out = scratch.cargo_raster(&[
        "chain",
        "run",
        manifest(DYNAMIC).to_str().expect("utf-8 path"),
        "--no-auth",
        "--stage",
        "step1",
    ]);
    let stdout = assert_ok(&out, "chain run --stage step1");

    // Found at its expanded position, not at some index in the manifest.
    assert!(stdout.contains("(re-run in place)"), "{stdout}");
    assert!(
        stdout.contains("chain-commitment left untouched"),
        "a single-stage run must not overwrite a whole-chain commitment\n{stdout}"
    );

    // Everything after it is stale and goes; everything before it stays.
    let run_dir = scratch.run_root(false);
    assert!(run_dir.join("step1").join("output.bin").is_file());
    assert!(run_dir.join("planner").join("output.bin").is_file());
    assert!(
        !run_dir.join("sink").join("output.bin").is_file(),
        "the sink runs after step1 and should have been invalidated"
    );
}

#[test]
fn a_stage_re_run_needs_a_shape_to_place_it_in() {
    // Without a previous run there is no `chain-shape`, so the count that
    // decides where `step1` even sits is unknown. The refusal has to name the
    // stage that would supply it rather than reporting an unknown stage name,
    // which is what an unresolved shape looks like from the inside.
    let scratch = Scratch::new("dynamic-stage-cold");
    // An existing but empty run directory, so the refusal under test is the
    // unresolved shape rather than the missing directory.
    fs::create_dir_all(scratch.run_root(false)).expect("run dir should be creatable");

    let out = scratch.cargo_raster(&[
        "chain",
        "run",
        manifest(DYNAMIC).to_str().expect("utf-8 path"),
        "--no-auth",
        "--run",
        scratch.run_root(false).to_str().expect("utf-8 path"),
        "--stage",
        "step1",
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stdout}{stderr}");
    assert!(!out.status.success(), "expected a refusal\n{combined}");
    assert!(combined.contains("shape is not fully known"), "{combined}");
    assert!(
        combined.contains("stage 'planner'"),
        "the refusal must name the stage that would supply the count\n{combined}"
    );
}

#[test]
fn a_tampered_trip_count_is_rejected_with_no_prover() {
    // The fault `ChainShape` exists to make detectable, and the reason a
    // stage-produced count is sound: a chain that claims a different number of
    // iterations than its planner committed is refutable from the commitment
    // alone. No trace, no receipt, no clock — `chain audit` re-encodes the
    // count the chain claims and compares one hash against what the planner
    // committed. See `docs/proposals/chain-repeat.md` §6.
    let scratch = Scratch::new("shape-fraud");
    scratch.run(DYNAMIC);

    let honest = scratch.planner_count(false);
    let commitment_path = scratch.latest_commitment();
    let mut chain = scratch.chain(false);
    assert_eq!(chain.shape.repeats[0].resolved_count as u64, honest);

    // Claim one more iteration than the planner asked for.
    chain.shape.repeats[0].resolved_count += 1;
    fs::write(&commitment_path, postcard::to_allocvec(&chain).unwrap())
        .expect("commitment should be writable");

    let out = scratch.cargo_raster(&[
        "chain",
        "audit",
        manifest(DYNAMIC).to_str().expect("utf-8 path"),
        commitment_path.to_str().expect("utf-8 path"),
    ]);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "a tampered count must not audit\n{combined}");
    assert!(combined.contains("shape fraud"), "{combined}");
    assert!(
        combined.contains("stage 0 ('planner') committed a different value"),
        "the failure must name the stage whose commitment contradicts it\n{combined}"
    );
}

#[test]
fn a_tampered_manifest_is_rejected_with_no_prover() {
    // The other half of §6 step 1: the commitment names the manifest it was
    // expanded from, so a verifier holding a different manifest learns that
    // rather than silently checking the wrong chain. This is what closes S1.
    let scratch = Scratch::new("spec-fraud");
    scratch.run(DYNAMIC);

    let commitment_path = scratch.latest_commitment();
    let mut chain = scratch.chain(false);
    chain.shape.spec_digest = vec![0xde, 0xad, 0xbe, 0xef];
    fs::write(&commitment_path, postcard::to_allocvec(&chain).unwrap())
        .expect("commitment should be writable");

    let out = scratch.cargo_raster(&[
        "chain",
        "audit",
        manifest(DYNAMIC).to_str().expect("utf-8 path"),
        commitment_path.to_str().expect("utf-8 path"),
    ]);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{combined}");
    assert!(combined.contains("different chain manifest"), "{combined}");
}

/// The `Shape` fault as a receipt, end to end.
///
/// `#[ignore]` because it runs the zkVM prover — minutes, not seconds. Run it
/// with `cargo test -p raster-cli --test chain_repeat_cli -- --ignored`.
/// Everything it asserts about *detection* is covered cheaply by
/// `a_tampered_trip_count_is_rejected_with_no_prover`; what this adds is that
/// the fault is also **exhibitable** — a succinct receipt a relying party can
/// check without holding the manifest.
#[test]
#[ignore = "runs the zkVM prover"]
fn a_shape_fault_produces_a_verifiable_receipt() {
    let scratch = Scratch::new("shape-receipt");
    scratch.run(DYNAMIC);

    let commitment_path = scratch.latest_commitment();
    let mut chain = scratch.chain(false);
    chain.shape.repeats[0].resolved_count += 1;
    fs::write(&commitment_path, postcard::to_allocvec(&chain).unwrap())
        .expect("commitment should be writable");

    let out = scratch.cargo_raster(&[
        "chain",
        "fraud-prove",
        manifest(DYNAMIC).to_str().expect("utf-8 path"),
        commitment_path.to_str().expect("utf-8 path"),
    ]);
    let stdout = assert_ok(&out, "chain fraud-prove");
    assert!(stdout.contains("shape fraud"), "{stdout}");
    assert!(
        stdout.contains("stage 0 ('planner') did not commit"),
        "{stdout}"
    );

    // And the receipt checks out against the pinned image ids.
    let out = scratch.cargo_raster(&[
        "chain",
        "fraud-verify",
        "--chain",
        manifest(DYNAMIC).to_str().expect("utf-8 path"),
        "--chain-commitment",
        commitment_path.to_str().expect("utf-8 path"),
    ]);
    let stdout = assert_ok(&out, "chain fraud-verify");
    assert!(stdout.contains("Shape"), "the verdict must name the fault\n{stdout}");
}
