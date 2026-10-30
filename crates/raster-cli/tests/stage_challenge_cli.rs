//! End-to-end coverage for disputed stage challenges and their defences
//! (`docs/proposals/stage-challenge.md`), through the real CLI:
//! `chain challenge` → `chain challenge-verify` → `chain defend` →
//! `chain defend-verify`, on the committed three-stage `examples/chain-example`
//! chain.
//!
//! - **a true challenge** — the claimer's commitment misstates `report`'s
//!   output. `chain challenge` finds it unprompted, the challenge is admitted,
//!   and `chain defend` reports that no defence exists, writing nothing;
//! - **the four answerable `--forge` cases** — each a false challenge against
//!   the honest chain, each defended (a mislabel proof for `claimed-output`, a
//!   refutation for `item`, `pad` and `truncate`), and each defence verified;
//! - a defence answers only the challenge it was proven against;
//! - the fifth forge, `drop-tail-root`, whose malformed body is refused before
//!   anything is written.
//!
//! The proofs are dev-mode (`RISC0_DEV_MODE=1`): the guests run, nothing is
//! proven, and the receipts are fakes that only a dev-mode verifier accepts. So
//! these check the files each command reads and writes, the window the defender
//! picks and the journals the guests commit — not the cryptography. They are
//! `#[ignore]` because each still executes the zkVM guests, minutes rather than
//! seconds. Run them with
//! `cargo test -p raster-cli --test stage_challenge_cli -- --ignored`.
//!
//! Each test runs in its own scratch working directory, since the chains root is
//! derived from the current directory.

use raster_core::chain::ChainCommitment;
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

fn chain_manifest() -> String {
    workspace_root()
        .join("examples/chain-example/Raster.toml")
        .to_str()
        .expect("utf-8 path")
        .to_string()
}

/// A scratch directory holding one honest authenticated run of the chain.
struct Scratch {
    dir: PathBuf,
    run_dir: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("raster-challenge-{tag}-{nanos}"));
        fs::create_dir_all(&dir).expect("scratch dir should be creatable");
        let mut scratch = Self {
            dir,
            run_dir: PathBuf::new(),
        };
        let out = scratch.cargo_raster(&["chain", "run", &chain_manifest()]);
        assert_ok(&out, "chain run");
        scratch.run_dir = fs::canonicalize(scratch.dir.join("target/raster/chains/latest"))
            .expect("chain run should leave a `latest` run");
        scratch
    }

    fn cargo_raster(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cargo-raster"))
            .current_dir(&self.dir)
            .env("RISC0_DEV_MODE", "1")
            .args(["raster"])
            .args(args)
            .output()
            .expect("cargo-raster should execute")
    }

    fn commitment(&self) -> String {
        path_str(&self.run_dir.join("chain-commitment"))
    }

    fn challenge_file(&self, name: &str) -> PathBuf {
        self.run_dir.join(".challenge").join(name)
    }

    /// `chain challenge`, against `stage` if named, forged as `forge` if named.
    fn challenge(&self, stage: Option<&str>, forge: Option<&str>) -> Output {
        let manifest = chain_manifest();
        let commitment = self.commitment();
        let mut args = vec!["chain", "challenge", &manifest, &commitment];
        if let Some(stage) = stage {
            args.extend(["--stage", stage]);
        }
        if let Some(forge) = forge {
            args.extend(["--forge", forge]);
        }
        self.cargo_raster(&args)
    }

    fn challenge_verify(&self) -> Output {
        let challenge = path_str(&self.challenge_file("challenge.bin"));
        self.cargo_raster(&[
            "chain",
            "challenge-verify",
            &challenge,
            "--chain-commitment",
            &self.commitment(),
        ])
    }

    fn defend(&self) -> Output {
        self.cargo_raster(&["chain", "defend", &chain_manifest(), &self.commitment()])
    }

    fn defend_verify(&self, defence: &Path, challenge: Option<&Path>) -> Output {
        let defence = path_str(defence);
        let mut args = vec!["chain".to_string(), "defend-verify".to_string(), defence];
        if let Some(challenge) = challenge {
            args.extend(["--challenge".to_string(), path_str(challenge)]);
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.cargo_raster(&args)
    }

    /// Post a forged challenge against `normalize`, check it is admitted,
    /// defend it and verify the defence; the `defend-verify` output.
    fn defend_forged(&self, forge: &str) -> String {
        let stdout = assert_ok(&self.challenge(Some("normalize"), Some(forge)), "chain challenge");
        assert!(stdout.contains("disputed challenge"), "{stdout}");
        assert!(stdout.contains("stage            0 'normalize'"), "{stdout}");

        let stdout = assert_ok(&self.challenge_verify(), "chain challenge-verify");
        assert!(stdout.contains("disputed challenge admitted"), "{stdout}");

        let stdout = assert_ok(&self.defend(), "chain defend");
        assert!(stdout.contains("defence →"), "{stdout}");

        let stdout = assert_ok(
            &self.defend_verify(&self.challenge_file("defence.bin"), None),
            "chain defend-verify",
        );
        assert!(stdout.contains("the challenge is dismissed"), "{stdout}");
        assert!(stdout.contains("stage            0"), "{stdout}");
        stdout
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn path_str(path: &Path) -> String {
    path.to_str().expect("utf-8 path").to_string()
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

/// The command's stdout and stderr, asserting it failed.
fn assert_fails(output: &Output, what: &str) -> String {
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success(), "{what} should have failed\n{combined}");
    combined
}

#[test]
#[ignore = "runs the zkVM guests (dev mode)"]
fn a_true_challenge_has_no_defence() {
    let scratch = Scratch::new("true");

    // The claimer misstates the terminal stage's output. `report` has no
    // consumer, so no link exposes it: only re-execution does.
    let commitment_path = scratch.run_dir.join("chain-commitment");
    let mut chain: ChainCommitment =
        postcard::from_bytes(&fs::read(&commitment_path).expect("commitment should be readable"))
            .expect("commitment should decode");
    let report = chain.stages.last_mut().expect("the chain has stages");
    assert_eq!(report.name, "report");
    report.output_payload_commitment[0] ^= 0xff;
    fs::write(&commitment_path, postcard::to_allocvec(&chain).unwrap())
        .expect("commitment should be writable");

    // No `--stage`: the challenger finds the stage itself.
    let stdout = assert_ok(&scratch.challenge(None, None), "chain challenge");
    assert!(stdout.contains("disputed challenge"), "{stdout}");
    assert!(stdout.contains("stage            2 'report'"), "{stdout}");

    let stdout = assert_ok(&scratch.challenge_verify(), "chain challenge-verify");
    assert!(stdout.contains("disputed challenge admitted"), "{stdout}");

    let combined = assert_fails(&scratch.defend(), "chain defend");
    assert!(combined.contains("no defence exists"), "{combined}");
    assert!(combined.contains("the challenge is true"), "{combined}");
    assert!(
        !scratch.challenge_file("defence.bin").exists(),
        "a true challenge must leave no defence behind"
    );
}

#[test]
#[ignore = "runs the zkVM guests (dev mode)"]
fn a_mislabelled_output_is_defended_with_a_mislabel_proof() {
    let scratch = Scratch::new("claimed-output");
    let stdout = scratch.defend_forged("claimed-output");
    assert!(stdout.contains("reason           mislabelled"), "{stdout}");
    assert!(stdout.contains("attested output"), "a mislabel attests the honest output\n{stdout}");
}

#[test]
#[ignore = "runs the zkVM guests (dev mode)"]
fn a_fabricated_item_is_refuted() {
    let scratch = Scratch::new("item");
    let stdout = scratch.defend_forged("item");
    assert!(stdout.contains("reason           refuted"), "{stdout}");
}

#[test]
#[ignore = "runs the zkVM guests (dev mode)"]
fn an_item_past_the_end_is_refuted() {
    let scratch = Scratch::new("pad");
    let stdout = scratch.defend_forged("pad");
    assert!(stdout.contains("reason           refuted"), "{stdout}");
}

#[test]
#[ignore = "runs the zkVM guests (dev mode)"]
fn a_trace_stopping_short_is_refuted() {
    let scratch = Scratch::new("truncate");
    let stdout = scratch.defend_forged("truncate");
    assert!(stdout.contains("reason           refuted"), "{stdout}");
}

#[test]
#[ignore = "runs the zkVM guests (dev mode)"]
fn a_defence_answers_only_its_own_challenge() {
    let scratch = Scratch::new("matching");
    scratch.defend_forged("item");
    let defence = scratch.dir.join("item-defence.bin");
    fs::copy(scratch.challenge_file("defence.bin"), &defence).expect("defence should be copyable");

    // A different challenge against the same stage: posting it clears the old
    // defence from beside it, and the old defence does not answer it.
    assert_ok(&scratch.challenge(Some("normalize"), Some("pad")), "chain challenge");
    assert!(
        !scratch.challenge_file("defence.bin").exists(),
        "a new challenge must not sit beside an earlier challenge's defence"
    );
    let challenge = scratch.challenge_file("challenge.bin");
    let combined = assert_fails(
        &scratch.defend_verify(&defence, Some(&challenge)),
        "chain defend-verify against another challenge",
    );
    assert!(combined.contains("the defence answers challenge"), "{combined}");
}

#[test]
#[ignore = "runs the zkVM guests (dev mode)"]
fn a_malformed_body_is_never_posted() {
    let scratch = Scratch::new("drop-tail-root");
    let combined = assert_fails(
        &scratch.challenge(Some("normalize"), Some("drop-tail-root")),
        "chain challenge --forge drop-tail-root",
    );
    assert!(combined.contains("would not be available"), "{combined}");
    assert!(
        !scratch.challenge_file("challenge.bin").exists(),
        "a refused challenge must not be written"
    );
}
