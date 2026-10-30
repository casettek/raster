//! `cargo raster replay`: zkVM work on a finished run's recorded calls.
//!
//! `replay profile` measures what a run's tiles cost in the RISC Zero executor,
//! without proving: for each tile, its largest recorded calls by input + output
//! bytes are replayed from the trace. `replay report` prints a saved result.
//! See `docs/proposals/tile-cost-estimator.md` §5.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use raster_analysis::replay_profile::{
    ReplayFailure, ReplayProfile, ReplayedCall, REPLAY_PROFILE_FILE,
};
use raster_backend_risc0::Risc0Backend;
use raster_compiler::{CfsBuilder, Project};
use raster_core::cfs::CfsCoordinates;
use raster_core::trace::{ExecStep, ExecTarget, StepKind, StepRecord};
use raster_core::{Error, Result};
use raster_prover::replay::Replayer;
use raster_runtime::TraceRecorder;

use crate::commands::run::load_trace_from_file;
use crate::run_manifest::{cfs_digest, RunManifest};

/// How many calls per tile `replay profile` replays: the largest by input +
/// output bytes, which on measured programs found every tile's most expensive
/// call (tile-cost-estimator §3).
pub const TOP_CALLS_PER_TILE: usize = 10;

/// A recorded tile call, before replay.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    exec_index: u64,
    coordinates: CfsCoordinates,
    input_bytes: u64,
    output_bytes: u64,
}

/// Replay each tile's largest recorded calls from the run in `run_dir`, write
/// `replay-profile.json` there and print it. Saved even when a replay fails,
/// marked partial; the command then fails too.
pub fn profile(run_dir: &str) -> Result<()> {
    let run_dir = std::path::absolute(run_dir)?;
    let manifest = RunManifest::read(&run_dir)?;
    // Paths inside the run's inputs resolve against the project root, as they
    // did for `run`, which is started there.
    std::env::set_current_dir(&manifest.project_root)?;
    let project = Project::new(manifest.project_root.clone())?;

    let cfs = CfsBuilder::new(&project)
        .build()
        .map_err(|e| Error::Other(format!("Failed to build CFS: {e}")))?;
    if cfs_digest(&cfs)? != manifest.cfs_digest {
        println!(
            "Warning: the project's CFS changed since this run. Every replayed call is still \
             checked against the trace."
        );
    }

    let (trace, recorder) = load_trace_from_file(
        &manifest.trace_path(&run_dir),
        manifest.trace_format,
        &cfs,
        manifest.input.as_deref(),
        manifest.input_manifest.as_deref(),
    )?;

    let mut report = ReplayProfile::new(
        manifest.run_id.clone(),
        cfs.tiles.iter().map(|tile| tile.id.clone()),
        TOP_CALLS_PER_TILE,
    );
    // The executor runs in this process, so this build's profile sets its speed.
    report.release_build = !cfg!(debug_assertions);
    let mut calls: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
    for step in &trace.0 {
        let Some(tile) = tile_id(step) else {
            continue;
        };
        let (input, output) = recorded_io(&recorder, step)?;
        let candidate = Candidate {
            exec_index: step.exec_index,
            coordinates: step.coordinates.clone(),
            input_bytes: input.len() as u64,
            output_bytes: output.len() as u64,
        };
        report
            .tiles
            .entry(tile.to_string())
            .or_default()
            .record_size(candidate.input_bytes + candidate.output_bytes);
        calls.entry(tile.to_string()).or_default().push(candidate);
    }
    let selected: BTreeMap<String, Vec<(u64, Candidate)>> = calls
        .into_iter()
        .map(|(tile, calls)| (tile, select_top(calls, TOP_CALLS_PER_TILE)))
        .collect();

    let planned: usize = selected.values().map(Vec::len).sum();
    let recorded: u64 = report.tiles.values().map(|tile| tile.calls_recorded).sum();
    println!(
        "\nReplaying {planned} of {recorded} recorded tile calls in the RISC Zero executor \
         (no proving): the {TOP_CALLS_PER_TILE} largest per tile by input + output bytes."
    );

    let lock = lock_image_ids(&project);
    let backend = Risc0Backend::new(project.output_dir.clone())
        .with_user_crate(project.root_dir.clone())
        .with_guest_stdout(false);
    let replayer = Replayer::new(&backend, &project);
    let outcome = replay_selected(&replayer, &recorder, &selected, &lock, &mut report, planned);
    report.complete = outcome.is_ok();

    let report_path = run_dir.join(REPLAY_PROFILE_FILE);
    std::fs::write(&report_path, report.to_json()?)?;
    println!("\nReplay profile saved to: {}\n", report_path.display());
    println!("{}", report.to_text());
    outcome
}

/// Print a saved replay profile: the file itself, or the one in a run directory.
pub fn report(path: &str) -> Result<()> {
    let path = PathBuf::from(path);
    let file = if path.is_dir() {
        path.join(REPLAY_PROFILE_FILE)
    } else {
        path
    };
    let bytes = std::fs::read(&file)
        .map_err(|e| Error::Other(format!("Cannot read {}: {e}", file.display())))?;
    println!("{}", ReplayProfile::from_json(&bytes)?.to_text());
    Ok(())
}

fn replay_selected(
    replayer: &Replayer<'_>,
    recorder: &TraceRecorder,
    selected: &BTreeMap<String, Vec<(u64, Candidate)>>,
    lock: &BTreeMap<String, String>,
    report: &mut ReplayProfile,
    planned: usize,
) -> Result<()> {
    let mut done = 0;
    let mut last_progress = Instant::now();
    for (tile, calls) in selected {
        let Some((_, first)) = calls.first() else {
            continue;
        };
        println!("  Preparing {tile} (its guest is compiled on first use)");
        let prepared = replayer
            .prepare_profile(tile)
            .map_err(|error| fail(report, tile, first, error))?;
        let entry = report.tiles.entry(tile.clone()).or_default();
        entry.image_id = Some(prepared.image_id());
        entry.lock_image_id = lock.get(tile).cloned();

        for (byte_rank, call) in calls {
            let measure = recorder
                .step_witness_at(&call.coordinates)
                .ok_or_else(|| Error::Other("Missing recorded witness".into()))
                .and_then(|witness| {
                    let input = witness.input_data().unwrap_or_default();
                    let output = witness.output_data().unwrap_or_default();
                    prepared.measure(&input, &output)
                })
                .map_err(|error| fail(report, tile, call, error))?;
            report
                .tiles
                .get_mut(tile)
                .expect("entry created above")
                .replayed
                .push(ReplayedCall {
                    exec_index: call.exec_index,
                    coordinates: call.coordinates.clone(),
                    input_bytes: call.input_bytes,
                    output_bytes: call.output_bytes,
                    byte_rank: *byte_rank,
                    user_cycles: measure.user_cycles,
                    segments: measure.segments.len() as u64,
                    padded_cycles: measure.padded_cycles,
                    exec_ns: duration_ns(measure.exec_time),
                });
            done += 1;
            if last_progress.elapsed() >= Duration::from_secs(1) {
                println!("  Replayed {done}/{planned}");
                last_progress = Instant::now();
            }
        }
    }
    Ok(())
}

/// Record where replay stopped, and hand the error back to stop it.
fn fail(report: &mut ReplayProfile, tile: &str, call: &Candidate, error: Error) -> Error {
    report.failure = Some(ReplayFailure {
        tile: tile.to_string(),
        exec_index: call.exec_index,
        coordinates: call.coordinates.clone(),
        message: error.to_string(),
    });
    error
}

/// The `n` largest calls by input + output bytes, largest first, ties in trace
/// order, each with its rank among all of the tile's calls (1 = largest).
/// `calls` must be in trace order.
fn select_top(mut calls: Vec<Candidate>, n: usize) -> Vec<(u64, Candidate)> {
    // A stable sort keeps trace order among equal sizes.
    calls.sort_by_key(|call| std::cmp::Reverse(call.input_bytes + call.output_bytes));
    calls
        .into_iter()
        .take(n)
        .enumerate()
        .map(|(index, call)| (index as u64 + 1, call))
        .collect()
}

fn tile_id(step: &StepRecord) -> Option<&str> {
    match &step.kind {
        StepKind::Exec(ExecStep {
            target: ExecTarget::Tile(id),
            ..
        }) => Some(id),
        _ => None,
    }
}

fn recorded_io(recorder: &TraceRecorder, step: &StepRecord) -> Result<(Vec<u8>, Vec<u8>)> {
    let missing = || {
        Error::Other(format!(
            "No recorded witness for the tile call at {:?} (exec {})",
            step.coordinates.0, step.exec_index
        ))
    };
    let input = recorder
        .input_data_at(&step.coordinates)
        .ok_or_else(missing)?;
    let output = recorder
        .output_data_at(&step.coordinates)
        .ok_or_else(missing)?;
    Ok((input.unwrap_or_default(), output.unwrap_or_default()))
}

/// Tile image ids from the project's `Raster.lock`; empty when it has none or
/// it cannot be read, since the comparison is informational.
fn lock_image_ids(project: &Project) -> BTreeMap<String, String> {
    let path = crate::program::raster_lock_path(project);
    if !path.exists() {
        return BTreeMap::new();
    }
    match crate::program::read_lock(&path) {
        Ok(lock) => lock
            .tiles
            .into_iter()
            .map(|(tile, entry)| (tile, entry.image_id))
            .collect(),
        Err(error) => {
            println!("Warning: {error}; image ids are not compared with Raster.lock");
            BTreeMap::new()
        }
    }
}

fn duration_ns(duration: Duration) -> u64 {
    duration.as_nanos().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(exec_index: u64, input_bytes: u64, output_bytes: u64) -> Candidate {
        Candidate {
            exec_index,
            coordinates: CfsCoordinates(vec![exec_index as i32]),
            input_bytes,
            output_bytes,
        }
    }

    fn picked(calls: Vec<Candidate>, n: usize) -> Vec<(u64, u64)> {
        select_top(calls, n)
            .into_iter()
            .map(|(rank, call)| (rank, call.exec_index))
            .collect()
    }

    #[test]
    fn selects_the_largest_calls_by_input_plus_output_bytes() {
        let calls = vec![
            call(1, 10, 0),
            call(2, 5, 50),
            call(3, 30, 1),
            call(4, 0, 0),
        ];
        assert_eq!(picked(calls, 2), [(1, 2), (2, 3)]);
    }

    #[test]
    fn equal_sizes_keep_trace_order() {
        let calls = vec![call(1, 7, 0), call(2, 3, 4), call(3, 9, 0), call(4, 0, 7)];
        assert_eq!(picked(calls, 4), [(1, 3), (2, 1), (3, 2), (4, 4)]);
    }

    #[test]
    fn a_tile_with_few_calls_is_replayed_in_full() {
        assert_eq!(picked(vec![call(1, 1, 1), call(2, 2, 2)], 10).len(), 2);
        assert!(picked(vec![], 10).is_empty());
    }
}
