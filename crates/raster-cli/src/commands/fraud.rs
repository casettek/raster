//! Fraud injection: corrupt a chosen step of an honest trace and commit it.

use rand::seq::IteratorRandom;

use std::io::Write;

use raster_core::cfs::CfsCoordinates;
use raster_core::trace::{ExecStep, ExecTarget, StepKind, StepRecord, Trace};
use raster_core::{Error, Result};

use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::trace::{FraudProofConfig, TraceCommitment, TraceCommitmentExt};

/// Which executed step the injector should corrupt.
///
/// The injector used to pick uniformly at random with a fresh `rand::rng()`,
/// which made every fraud-path finding unrepeatable: two probes of the same
/// program corrupt different steps, exercise different guest checks, and stop
/// at different walls. That is how `authenticated-chain-draft-output` came to
/// be recorded as "the last blocker" on the strength of one draw, and
/// `sequence-scope-forbids-narrowing` was found by another. A probe costs
/// hours of proving, so guessing is expensive as well as unsound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FraudTarget {
    /// Position in the eligible list, as `--fraud-step list` prints it.
    Index(usize),
    /// The eligible step whose `exec_index` is this.
    ExecIndex(u64),
    /// The first eligible step running this tile or recur sequence.
    Named(String),
    /// Uniform choice from this seed — reproducible, unlike an unseeded one.
    Seed(u64),
    /// Print the eligible steps and stop, writing no commitment.
    List,
}

impl std::str::FromStr for FraudTarget {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        if value == "list" {
            return Ok(FraudTarget::List);
        }
        if let Some(rest) = value.strip_prefix("exec:") {
            return rest
                .parse()
                .map(FraudTarget::ExecIndex)
                .map_err(|_| format!("--fraud-step exec:<n> expects an integer, got '{rest}'"));
        }
        if let Some(rest) = value.strip_prefix("seed:") {
            return rest
                .parse()
                .map(FraudTarget::Seed)
                .map_err(|_| format!("--fraud-step seed:<n> expects an integer, got '{rest}'"));
        }
        if let Some(rest) = value.strip_prefix("tile:") {
            if rest.is_empty() {
                return Err("--fraud-step tile:<name> expects a name".into());
            }
            return Ok(FraudTarget::Named(rest.to_string()));
        }
        value.parse().map(FraudTarget::Index).map_err(|_| {
            format!(
                "--fraud-step expects <n>, exec:<n>, tile:<name>, seed:<n> or list, got '{value}'"
            )
        })
    }
}

/// What ran at a step, for the listing and for `tile:` matching.
fn exec_target_name(target: &ExecTarget) -> &str {
    match target {
        ExecTarget::Tile(id) | ExecTarget::RecurTile(id) => id,
        ExecTarget::RecurSequence(id) => id,
    }
}

fn exec_target_kind(target: &ExecTarget) -> &'static str {
    match target {
        ExecTarget::Tile(_) => "tile",
        ExecTarget::RecurTile(_) => "recur-tile",
        ExecTarget::RecurSequence(_) => "recur-seq",
    }
}

/// Steps worth corrupting: a sequence boundary or a program start has no
/// replayed output to contradict.
fn eligible_fraud_steps(trace: &Trace) -> Vec<usize> {
    trace
        .iter()
        .enumerate()
        .filter(|(_, step_record)| {
            matches!(&step_record.kind, StepKind::Exec(exec) if !exec.input_commitment.is_empty())
        })
        .map(|(position, _)| position)
        .collect()
}

fn print_eligible_fraud_steps(trace: &Trace, eligible: &[usize]) {
    println!();
    println!("Eligible fraud steps ({}):", eligible.len());
    println!(
        "  {:>5}  {:>10}  {:>10}  {:<24}  {}",
        "index", "exec_index", "kind", "target", "coordinates"
    );
    for (index, position) in eligible.iter().enumerate() {
        let step_record = &trace[*position];
        let StepKind::Exec(exec) = &step_record.kind else {
            continue;
        };
        println!(
            "  {:>5}  {:>10}  {:>10}  {:<24}  {:?}",
            index,
            step_record.exec_index,
            exec_target_kind(&exec.target),
            exec_target_name(&exec.target),
            step_record.coordinates,
        );
    }
    println!();
    println!("Corrupt one with --fraud-step <index>, exec:<exec_index> or tile:<target>.");
}

/// Resolve a target to a position in `trace`, or explain why it did not match.
fn resolve_fraud_step(trace: &Trace, eligible: &[usize], target: &FraudTarget) -> Result<usize> {
    match target {
        FraudTarget::List => unreachable!("list is handled before resolution"),
        FraudTarget::Index(index) => eligible.get(*index).copied().ok_or_else(|| {
            Error::Other(format!(
                "--fraud-step {index} is out of range: the trace has {} eligible steps (0..{})",
                eligible.len(),
                eligible.len().saturating_sub(1),
            ))
        }),
        FraudTarget::ExecIndex(exec_index) => eligible
            .iter()
            .copied()
            .find(|position| trace[*position].exec_index == *exec_index)
            .ok_or_else(|| {
                Error::Other(format!(
                    "--fraud-step exec:{exec_index} matched no eligible step;                      run with --fraud-step list to see them"
                ))
            }),
        FraudTarget::Named(name) => eligible
            .iter()
            .copied()
            .find(|position| match &trace[*position].kind {
                StepKind::Exec(exec) => exec_target_name(&exec.target) == name,
                _ => false,
            })
            .ok_or_else(|| {
                Error::Other(format!(
                    "--fraud-step tile:{name} matched no eligible step;                      run with --fraud-step list to see them"
                ))
            }),
        FraudTarget::Seed(seed) => {
            use rand::SeedableRng;
            let mut rng = rand::rngs::StdRng::seed_from_u64(*seed);
            eligible.iter().copied().choose(&mut rng).ok_or_else(|| {
                Error::Other("The trace has no step worth corrupting".into())
            })
        }
    }
}

pub fn fraud(
    trace: &mut Trace,
    commit_path: &str,
    fraud_proof_config: FraudProofConfig,
    target: Option<&FraudTarget>,
) -> Result<()> {
    let eligible = eligible_fraud_steps(trace);

    if matches!(target, Some(FraudTarget::List)) {
        print_eligible_fraud_steps(trace, &eligible);
        return Ok(());
    }

    if eligible.is_empty() {
        return Err(Error::Other(
            "The trace has no step worth corrupting: every step is a boundary \
             with no replayed output to contradict"
                .into(),
        ));
    }

    // An absent flag still picks at random, but from a seed that is printed —
    // so an interesting result found by luck can be replayed on purpose.
    let effective = match target {
        Some(target) => target.clone(),
        None => FraudTarget::Seed(rand::random::<u64>()),
    };
    let position = resolve_fraud_step(trace, &eligible, &effective)?;
    let index = eligible
        .iter()
        .position(|candidate| *candidate == position)
        .expect("resolved position comes from the eligible list");

    let exec_index = trace[position].exec_index;
    let coordinates = trace[position].coordinates.clone();
    let (kind, name) = match &trace[position].kind {
        StepKind::Exec(exec) => (
            exec_target_kind(&exec.target),
            exec_target_name(&exec.target).to_string(),
        ),
        _ => unreachable!("eligible steps are Exec steps"),
    };

    match &mut trace[position].kind {
        StepKind::Exec(exec) => exec.output_commitment = vec![0u8, 1u8],
        _ => unreachable!("eligible steps are Exec steps"),
    }

    println!();
    println!(
        "Fraud injected into 1 of {} eligible steps:",
        eligible.len()
    );
    println!("  index       {index}");
    println!("  exec_index  {exec_index}");
    println!("  target      {kind} {name}");
    println!("  coordinates {coordinates:?}");
    if let FraudTarget::Seed(seed) = effective {
        println!("  seed        {seed}");
    }
    println!("  replay with --fraud-step {index}   (or exec:{exec_index})");

    let trace_commitment =
        TraceCommitment::try_build(trace, &EMPTY_TRIE_NODES[0], fraud_proof_config)
            .map_err(|e| Error::Other(e.to_string()))?;

    let bytes = postcard::to_allocvec(&trace_commitment).unwrap();

    let mut commitment_file =
        std::fs::File::create(commit_path).expect("Failed to create commitemt file");
    commitment_file
        .write_all(&bytes)
        .expect("Failed to save commitment");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The selector grammar is the whole point of the flag — a form that
    /// silently parses as something else would send a multi-hour probe at the
    /// wrong step, which is the failure this flag exists to end.
    #[test]
    fn fraud_target_parses_every_form() {
        use std::str::FromStr;

        assert_eq!(FraudTarget::from_str("list").unwrap(), FraudTarget::List);
        assert_eq!(FraudTarget::from_str("0").unwrap(), FraudTarget::Index(0));
        assert_eq!(FraudTarget::from_str("12").unwrap(), FraudTarget::Index(12));
        assert_eq!(
            FraudTarget::from_str("exec:7").unwrap(),
            FraudTarget::ExecIndex(7)
        );
        assert_eq!(
            FraudTarget::from_str("seed:42").unwrap(),
            FraudTarget::Seed(42)
        );
        assert_eq!(
            FraudTarget::from_str("tile:concat_messages").unwrap(),
            FraudTarget::Named("concat_messages".into())
        );

        // A tile whose name is digits must not become an index, and a
        // malformed prefix must not fall through to the bare-integer arm.
        assert_eq!(
            FraudTarget::from_str("tile:7").unwrap(),
            FraudTarget::Named("7".into())
        );
        assert!(FraudTarget::from_str("exec:").is_err());
        assert!(FraudTarget::from_str("tile:").is_err());
        assert!(FraudTarget::from_str("seed:x").is_err());
        assert!(FraudTarget::from_str("-1").is_err());
        assert!(FraudTarget::from_str("").is_err());
    }

    /// A seed must select the same step every time, or the flag has not
    /// actually removed the nondeterminism it was added to remove.
    #[test]
    fn seeded_selection_is_stable() {
        use raster_core::trace::StorageRoots;

        let trace = Trace(
            (0..12)
                .map(|index| StepRecord {
                    exec_index: index,
                    sequence_id: "s".into(),
                    coordinates: CfsCoordinates::default(),
                    kind: StepKind::Exec(ExecStep {
                        target: ExecTarget::Tile(format!("tile_{index}")),
                        intra_sequence_index: 1,
                        input_commitment: vec![1],
                        input_source_commitment: Vec::new(),
                        output_commitment: vec![2],
                        storage: StorageRoots {
                            root_before: Vec::new(),
                            root_after: Vec::new(),
                            index_root_before: Vec::new(),
                            index_root_after: Vec::new(),
                        },
                    }),
                    recur_progress_commitment: Default::default(),
                    recur_state: None,
                })
                .collect::<Vec<_>>(),
        );

        let eligible = eligible_fraud_steps(&trace);
        assert_eq!(eligible.len(), 12);

        let first = resolve_fraud_step(&trace, &eligible, &FraudTarget::Seed(99)).unwrap();
        let again = resolve_fraud_step(&trace, &eligible, &FraudTarget::Seed(99)).unwrap();
        assert_eq!(first, again, "the same seed must choose the same step");

        assert_eq!(
            resolve_fraud_step(&trace, &eligible, &FraudTarget::Index(3)).unwrap(),
            3
        );
        assert_eq!(
            resolve_fraud_step(&trace, &eligible, &FraudTarget::ExecIndex(5)).unwrap(),
            5
        );
        assert_eq!(
            resolve_fraud_step(&trace, &eligible, &FraudTarget::Named("tile_8".into())).unwrap(),
            8
        );
        assert!(resolve_fraud_step(&trace, &eligible, &FraudTarget::Index(99)).is_err());
        assert!(resolve_fraud_step(&trace, &eligible, &FraudTarget::Named("nope".into())).is_err());
    }
}
