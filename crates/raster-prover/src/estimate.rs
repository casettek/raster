//! Executor-only fault-window measurements. These results are never proof receipts.
use crate::{
    authorization::authorization_env,
    replay::{CallMeasure, Replayer},
    trace::{commitment_header, commitment_slice_witness, CommitmentRead, SerializableFrontier},
    transition::{build_step_input, storage_root, write_transition_inputs, PreparedFault},
    AUTHORIZATION_GUEST_ELF, AUTHORIZATION_GUEST_ID, TRANSITION_GUEST_ELF, TRANSITION_GUEST_ID,
};
use raster_backend::{padded_proof_cycles, SegmentStat};
use raster_core::{
    authorization::{AuthorizationJournal, ManifestedInputs},
    trace::{ExecStep, ExecTarget, StepKind},
    transition::{InitTransition, TransitionJournal, TransitionState},
    Error, Result,
};
use risc0_zkvm::{ExitCode, ReceiptClaim};
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Instant};

fn error(e: impl std::fmt::Display) -> Error {
    Error::Other(e.to_string())
}
fn id_hex(id: [u32; 8]) -> String {
    hex::encode(
        id.into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>(),
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct Measurement {
    pub image_id: String,
    pub user_cycles: u64,
    pub segments: Vec<SegmentStat>,
    pub segment_count: usize,
    pub padded_cycles: u64,
    pub executor_duration_ms: f64,
}
impl Measurement {
    fn tile(id: String, m: CallMeasure) -> Self {
        Self {
            image_id: id,
            user_cycles: m.user_cycles,
            segment_count: m.segments.len(),
            segments: m.segments,
            padded_cycles: m.padded_cycles,
            executor_duration_ms: m.exec_time.as_secs_f64() * 1000.0,
        }
    }
}

/// Run the unchanged guest and retain only execution output, never a receipt.
fn execute(
    env: risc0_zkvm::ExecutorEnv<'_>,
    elf: &[u8],
    id: [u32; 8],
) -> Result<(Measurement, risc0_zkvm::Journal)> {
    let start = Instant::now();
    let session = risc0_zkvm::default_executor()
        .execute(env, elf)
        .map_err(error)?;
    if session.exit_code != ExitCode::Halted(0) {
        return Err(error("guest did not halt successfully"));
    }
    if session.segments.is_empty() {
        return Err(error("executor returned no segments"));
    }
    let segments: Vec<_> = session
        .segments
        .iter()
        .map(|s| SegmentStat {
            po2: s.po2,
            cycles: s.cycles,
        })
        .collect();
    Ok((
        Measurement {
            image_id: id_hex(id),
            user_cycles: session.cycles(),
            padded_cycles: padded_proof_cycles(&segments),
            segment_count: segments.len(),
            segments,
            executor_duration_ms: start.elapsed().as_secs_f64() * 1000.0,
        },
        session.journal,
    ))
}

/// An assumption about completed execution, not a claim that a proof was generated.
/// Do not pass the executor's conditional claim: env::verify expects an empty
/// assumptions list, as it would see after proof composition resolved dependencies.
fn assumption(image: &[u8], journal: &[u8]) -> Result<ReceiptClaim> {
    let digest = risc0_zkvm::sha::Digest::try_from(image).map_err(error)?;
    Ok(ReceiptClaim::ok(digest, journal.to_vec()))
}

#[derive(Debug, Serialize)]
pub struct StepMeasurement {
    pub exec_index: u64,
    pub coordinates: Vec<i32>,
    pub step_kind: String,
    pub tile_id: Option<String>,
    pub window_opening: bool,
    pub tile: Option<Measurement>,
    pub transition: Option<Measurement>,
    pub transition_to_tile_ratio: Option<f64>,
}
#[derive(Debug, Serialize)]
pub struct Failure {
    pub phase: String,
    pub exec_index: Option<u64>,
    pub message: String,
}
#[derive(Serialize)]
pub struct EstimateReport {
    pub version: u32,
    pub kind: &'static str,
    pub outcome: String,
    pub complete: bool,
    pub commitment_digest: String,
    pub configured_window_size: usize,
    pub window_start: usize,
    pub actual_step_count: usize,
    pub requested_step_limit: Option<usize>,
    pub measured_step_count: usize,
    pub authorization: Option<Measurement>,
    pub steps: Vec<StepMeasurement>,
    pub final_journal: Option<TransitionJournal>,
    pub error: Option<Failure>,
    pub summary: Value,
}

fn kind(k: &StepKind) -> &'static str {
    match k {
        StepKind::ProgramStart(_) => "ProgramStart",
        StepKind::ProgramEnd(_) => "ProgramEnd",
        StepKind::SequenceStart { .. } => "SequenceStart",
        StepKind::SequenceEnd { .. } => "SequenceEnd",
        StepKind::RecurStart(_) => "RecurStart",
        StepKind::RecurEnd(_) => "RecurEnd",
        StepKind::Exec(_) => "Exec",
    }
}

/// Measure a validated audit window. Failures retain only completed executions and
/// identify the phase/step. The caller must treat an error report as a failed command.
pub fn estimate_window(
    prepared: &PreparedFault,
    program_frame: &[u8],
    inputs: &ManifestedInputs,
    commitment: &impl CommitmentRead,
    replayer: &Replayer<'_>,
    limit: Option<usize>,
) -> EstimateReport {
    let header = commitment_header(commitment);
    let mut report = EstimateReport {
        version: 1,
        kind: "fault-window-estimate",
        outcome: "error".into(),
        complete: false,
        commitment_digest: hex::encode(header.digest()),
        configured_window_size: header.window_size as usize,
        window_start: 0,
        actual_step_count: prepared.window.items.len(),
        requested_step_limit: limit,
        measured_step_count: 0,
        authorization: None,
        steps: vec![],
        final_journal: None,
        error: None,
        summary: Value::Null,
    };
    let mut phase = "preparation";
    let mut exec_index = None;
    let result = (|| -> Result<()> {
        if limit == Some(0) {
            return Err(error("step limit must be positive"));
        }
        if risc0_zkvm::ProverOpts::default().dev_mode() {
            return Err(error("estimation requires RISC0_DEV_MODE disabled"));
        }
        if AUTHORIZATION_GUEST_ELF.is_empty() || TRANSITION_GUEST_ELF.is_empty() {
            return Err(error("compiled guests required"));
        }
        if prepared.io.len() != prepared.window.items.len() || prepared.io.is_empty() {
            return Err(error("invalid prepared window"));
        }
        let frontier = SerializableFrontier::from_bytes(&prepared.window.frontier)
            .ok_or_else(|| error("invalid window frontier"))?;
        report.window_start = usize::try_from(frontier.position).map_err(error)?;
        if report
            .window_start
            .checked_add(prepared.window.items.len())
            .is_none_or(|end| end > commitment.len())
            || prepared.window.fingerprint.len() != prepared.window.items.len()
        {
            return Err(error(
                "window exceeds commitment or fingerprint length differs",
            ));
        }
        let slice = commitment_slice_witness(
            commitment,
            report.window_start,
            prepared.window.fingerprint.len(),
        );
        let mut state = TransitionState::Init(InitTransition {
            init_frontier: frontier,
            init_storage_frontier: prepared.initial_storage_frontier.clone(),
            init_storage_root: storage_root(&prepared.initial_storage_frontier),
            init_storage_index_root: prepared.initial_index_root.clone(),
            fingerprint: prepared.window.fingerprint.clone(),
        });
        phase = "authorization";
        let (measurement, authorization_raw) = execute(
            authorization_env(inputs)?,
            AUTHORIZATION_GUEST_ELF,
            AUTHORIZATION_GUEST_ID,
        )?;
        let authorization: AuthorizationJournal = authorization_raw.decode().map_err(error)?;
        let authorization_claim = assumption(
            &hex::decode(&measurement.image_id).map_err(error)?,
            &authorization_raw.bytes,
        )?;
        report.authorization = Some(measurement);
        let mut previous_raw: Option<Vec<u8>> = None;
        let mut tiles = BTreeMap::new();
        for (index, (step, io)) in prepared
            .window
            .items
            .iter()
            .zip(&prepared.io)
            .take(limit.unwrap_or(usize::MAX))
            .enumerate()
        {
            exec_index = Some(step.exec_index);
            let tile_id = match &step.kind {
                StepKind::Exec(ExecStep {
                    target: ExecTarget::Tile(id),
                    ..
                }) => Some(id.clone()),
                _ => None,
            };
            report.steps.push(StepMeasurement {
                exec_index: step.exec_index,
                coordinates: step.coordinates.to_vec(),
                step_kind: kind(&step.kind).into(),
                tile_id: tile_id.clone(),
                window_opening: index == 0,
                tile: None,
                transition: None,
                transition_to_tile_ratio: None,
            });
            println!(
                "RASTER_ESTIMATE_PROGRESS {}",
                json!({"step":index+1,"total":report.actual_step_count.min(limit.unwrap_or(usize::MAX)),"exec_index":step.exec_index})
            );
            let mut replay_journal = None;
            let mut tile_claim = None;
            if let Some(id) = tile_id {
                phase = "tile";
                if !tiles.contains_key(&id) {
                    tiles.insert(id.clone(), replayer.prepare_profile(&id)?);
                }
                let tile = tiles[&id].measure_with_journal(
                    io.input_witness.as_deref().unwrap_or_default(),
                    io.output_witness.as_deref().unwrap_or_default(),
                )?;
                tile_claim = Some(assumption(
                    &hex::decode(&tile.image_id).map_err(error)?,
                    &tile.raw_journal,
                )?);
                replay_journal = Some(tile.journal);
                report.steps.last_mut().unwrap().tile =
                    Some(Measurement::tile(tile.image_id, tile.measure));
            }
            phase = "transition";
            let input = build_step_input(
                step,
                &prepared.input_sources_witnesses,
                io.clone(),
                replay_journal,
                &authorization,
                (index == 0)
                    .then_some(prepared.entrypoint_membership_witness.as_ref())
                    .flatten(),
                (index == 0)
                    .then(|| prepared.window_start_recur_progress.clone())
                    .flatten(),
                (index == 0).then(|| commitment.tail().to_vec()),
            );
            let mut builder = risc0_zkvm::ExecutorEnv::builder();
            builder.add_assumption(authorization_claim.clone());
            if let Some(claim) = tile_claim {
                builder.add_assumption(claim);
            }
            if let Some(raw) = &previous_raw {
                builder.add_assumption(assumption(
                    &hex::decode(id_hex(TRANSITION_GUEST_ID)).map_err(error)?,
                    raw,
                )?);
            }
            write_transition_inputs(
                &mut builder,
                program_frame,
                &input,
                &state,
                report.final_journal.as_ref(),
                &header,
                &slice,
            );
            let (measurement, raw) = execute(
                builder.build().map_err(error)?,
                TRANSITION_GUEST_ELF,
                TRANSITION_GUEST_ID,
            )?;
            let journal: TransitionJournal = raw.decode().map_err(error)?;
            if journal.refuted_trace_commitment != header.digest().to_vec() {
                return Err(error("transition commitment binding mismatch"));
            }
            state = journal.current_state.clone();
            previous_raw = Some(raw.bytes);
            report.final_journal = Some(journal);
            let row = report.steps.last_mut().unwrap();
            row.transition_to_tile_ratio = row
                .tile
                .as_ref()
                .and_then(|tile| ratio(measurement.user_cycles, tile.user_cycles));
            row.transition = Some(measurement);
            report.measured_step_count += 1;
        }
        let full = report.measured_step_count == report.actual_step_count;
        if full && !matches!(state, TransitionState::Finished) {
            return Err(error(
                "complete window did not reach the expected fault state",
            ));
        }
        report.complete = full;
        report.outcome = if full { "estimated" } else { "partial" }.into();
        Ok(())
    })();
    if let Err(e) = result {
        report.error = Some(Failure {
            phase: phase.into(),
            exec_index,
            message: e.to_string(),
        });
    }
    report.summary = summarize(&report);
    report
}

fn ratio(n: u64, d: u64) -> Option<f64> {
    (d != 0).then(|| n as f64 / d as f64)
}
fn stats(mut values: Vec<u64>) -> Value {
    values.sort_unstable();
    let n = values.len();
    if n == 0 {
        return json!({"count":0,"min":null,"max":null,"median":null,"mean":null,"total":null});
    }
    let total: u64 = values.iter().sum();
    let median = if n % 2 == 1 {
        values[n / 2] as f64
    } else {
        values[n / 2 - 1] as f64 / 2.0 + values[n / 2] as f64 / 2.0
    };
    json!({"count":n,"min":values[0],"max":values[n-1],"median":median,"mean":total as f64/n as f64,"total":total})
}
fn distribution(values: &[&Measurement]) -> Value {
    json!({"user_cycles":stats(values.iter().map(|m|m.user_cycles).collect()),"padded_cycles":stats(values.iter().map(|m|m.padded_cycles).collect())})
}
fn summarize(report: &EstimateReport) -> Value {
    let tiles: Vec<_> = report
        .steps
        .iter()
        .filter_map(|s| s.tile.as_ref())
        .collect();
    let transitions: Vec<_> = report
        .steps
        .iter()
        .filter_map(|s| s.transition.as_ref())
        .collect();
    let opening: Vec<_> = report
        .steps
        .iter()
        .filter(|s| s.window_opening)
        .filter_map(|s| s.transition.as_ref())
        .collect();
    let continuation: Vec<_> = report
        .steps
        .iter()
        .filter(|s| !s.window_opening)
        .filter_map(|s| s.transition.as_ref())
        .collect();
    let mut by_kind: BTreeMap<&str, Vec<&Measurement>> = BTreeMap::new();
    let mut by_tile: BTreeMap<&str, Vec<&Measurement>> = BTreeMap::new();
    for s in &report.steps {
        if let Some(m) = &s.transition {
            by_kind.entry(&s.step_kind).or_default().push(m);
        }
        if let (Some(id), Some(m)) = (&s.tile_id, &s.tile) {
            by_tile.entry(id).or_default().push(m);
        }
    }
    let totals = |padded: bool| {
        let cycles = |m: &Measurement| {
            if padded {
                m.padded_cycles
            } else {
                m.user_cycles
            }
        };
        let authorization = report.authorization.as_ref().map(&cycles).unwrap_or(0);
        let tile: u64 = tiles.iter().map(|m| cycles(m)).sum();
        let transition: u64 = transitions.iter().map(|m| cycles(m)).sum();
        let overhead = authorization + transition;
        json!({"authorization":authorization,"tiles":tile,"transitions":transition,"total":overhead+tile,
            "non_tile_overhead":overhead,"overhead_share":ratio(overhead,overhead+tile),
            "overhead_per_tile_invocation":ratio(overhead,tiles.len() as u64)})
    };
    json!({"tiles":distribution(&tiles),"transitions":distribution(&transitions),"opening":distribution(&opening),"continuation":distribution(&continuation),
        "transitions_by_kind":by_kind.into_iter().map(|(k,v)|(k,distribution(&v))).collect::<BTreeMap<_,_>>(),
        "tiles_by_id":by_tile.into_iter().map(|(k,v)|(k,distribution(&v))).collect::<BTreeMap<_,_>>(),
        "user_cycles":totals(false),"padded_cycles":totals(true)})
}

impl EstimateReport {
    pub fn to_text(&self) -> String {
        let total = &self.summary["user_cycles"];
        format!("Fault estimate: {} ({}/{} steps)\nUser cycles: authorization {}, transitions {}, tiles {}, total {}\nTransition cycles min/median/mean/max: {}/{}/{}/{}\nTile cycles min/median/mean/max: {}/{}/{}/{}\nNon-tile overhead per tile invocation: {} cycles\nExecutor measurements only; excludes proof composition and Groth16.\n",
            self.outcome, self.measured_step_count, self.actual_step_count,
            total["authorization"],total["transitions"],total["tiles"],total["total"],
            self.summary["transitions"]["user_cycles"]["min"],self.summary["transitions"]["user_cycles"]["median"],self.summary["transitions"]["user_cycles"]["mean"],self.summary["transitions"]["user_cycles"]["max"],
            self.summary["tiles"]["user_cycles"]["min"],self.summary["tiles"]["user_cycles"]["median"],self.summary["tiles"]["user_cycles"]["mean"],self.summary["tiles"]["user_cycles"]["max"],total["overhead_per_tile_invocation"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn statistics_and_zero_ratios() {
        assert_eq!(stats(vec![])["median"], Value::Null);
        assert_eq!(stats(vec![9, 1, 3])["median"], 3.0);
        assert_eq!(stats(vec![9, 1, 4, 2])["median"], 3.0);
        assert_eq!(stats(vec![9, 1, 4, 2])["mean"], 4.0);
        assert_eq!(stats(vec![9, 1, 4, 2])["total"], 16);
        assert_eq!(ratio(1, 0), None);
        assert_eq!(ratio(6, 3), Some(2.0));
    }
    fn measurement(cycles: u64) -> Measurement {
        Measurement {
            image_id: "test".into(),
            user_cycles: cycles,
            segments: vec![],
            segment_count: 0,
            padded_cycles: cycles * 2,
            executor_duration_ms: 1.0,
        }
    }
    #[test]
    fn mixed_steps_and_partial_failures_use_actual_populations() {
        let mut report = EstimateReport {
            version: 1,
            kind: "fault-window-estimate",
            outcome: "error".into(),
            complete: false,
            commitment_digest: "test".into(),
            configured_window_size: 128,
            window_start: 12,
            actual_step_count: 128,
            requested_step_limit: None,
            measured_step_count: 2,
            authorization: Some(measurement(30)),
            steps: vec![],
            final_journal: None,
            error: None,
            summary: Value::Null,
        };
        for (index, tile, transition) in [
            (0, None, Some(100)),
            (1, Some(200), Some(50)),
            (2, Some(400), None),
        ] {
            report.steps.push(StepMeasurement {
                exec_index: index,
                coordinates: vec![],
                step_kind: if index == 0 { "RecurEnd" } else { "Exec" }.into(),
                tile_id: tile.map(|_| "tile".into()),
                window_opening: index == 0,
                tile: tile.map(measurement),
                transition: transition.map(measurement),
                transition_to_tile_ratio: None,
            });
        }
        let s = summarize(&report);
        assert_eq!(s["transitions"]["user_cycles"]["count"], 2);
        assert_eq!(s["tiles"]["user_cycles"]["count"], 2);
        assert_eq!(s["user_cycles"]["total"], 780);
        assert_eq!(s["user_cycles"]["overhead_per_tile_invocation"], 90.0);
        assert_eq!(s["opening"]["user_cycles"]["mean"], 100.0);
        assert_eq!(s["continuation"]["user_cycles"]["mean"], 50.0);
        assert_eq!(
            s["transitions_by_kind"]["RecurEnd"]["user_cycles"]["count"],
            1
        );
        assert_eq!(s["tiles_by_id"]["tile"]["user_cycles"]["median"], 300.0);
        report.steps.clear();
        let s = summarize(&report);
        assert_eq!(s["tiles"]["user_cycles"]["min"], Value::Null);
        assert_eq!(
            s["user_cycles"]["overhead_per_tile_invocation"],
            Value::Null
        );
    }
}
