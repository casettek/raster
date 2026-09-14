//! Replay the first 128 invocations per selected tile ID, retaining only aggregates.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

use raster_analysis::replay_profile::{
    InvocationLocation, ReplayProfile, ReplayProfileFailure, REPLAY_INVOCATION_LIMIT,
};
use raster_backend_risc0::Risc0Backend;
use raster_compiler::{tile::TileDiscovery, Project};
use raster_core::trace::{ExecStep, ExecTarget, StepKind, StepRecord, Trace};
use raster_core::{Error, Result};
use raster_prover::replay::Replayer;
use raster_runtime::TraceRecorder;

pub(super) fn validate_tiles(project: &Project, requested: &[String]) -> Result<BTreeSet<String>> {
    let selected: BTreeSet<_> = requested.iter().cloned().collect();
    if selected.is_empty() {
        return Ok(selected);
    }
    let discovery = TileDiscovery::new(project);
    for tile in &selected {
        if !discovery.contains(tile) {
            return Err(Error::InvalidTileId(format!(
                "Tile '{tile}' not found in project"
            )));
        }
    }
    Ok(selected)
}

pub(super) fn run(
    project: &Project,
    trace: &Trace,
    recorder: &TraceRecorder,
    selected: BTreeSet<String>,
    run_id: &str,
    report_path: &Path,
) -> Result<()> {
    let backend = Risc0Backend::new(project.output_dir.clone())
        .with_user_crate(project.root_dir.clone())
        .with_guest_stdout(false);
    let replayer = Replayer::new(&backend, project);
    let mut report = ReplayProfile::new(run_id.into(), selected);
    println!("\nProfiling selected tiles in RISC Zero (up to {REPLAY_INVOCATION_LIMIT} invocations per tile, no proving)...");
    let mut last_progress = Instant::now();
    let outcome = replay_selected(
        trace.iter(),
        &mut report,
        |tile| {
            println!("  Preparing tile: {tile}");
            let prepared = replayer.prepare_profile(tile)?;
            let image_id = prepared.image_id();
            Ok((prepared, image_id))
        },
        |prepared, step| {
            let witness = recorder
                .step_witness_at(step.coordinates())
                .ok_or_else(|| Error::Other("Missing recorded input/output witness".into()))?;
            let input = witness
                .input_data()
                .ok_or_else(|| Error::Other("Missing recorded input bytes".into()))?;
            let output = witness
                .output_data()
                .ok_or_else(|| Error::Other("Missing recorded output bytes".into()))?;
            prepared.profile(&input, &output)
        },
        |done, total| {
            if last_progress.elapsed() >= Duration::from_secs(1) {
                println!("  Replayed {done}/{total} selected invocations");
                last_progress = Instant::now();
            }
        },
    );
    // Save even when replay stops on an error; `complete` stays false and the
    // failure has the tile and exact execution location. No per-call log grows.
    std::fs::write(report_path, serde_json::to_vec_pretty(&report)?)?;
    println!("\nProfiling");
    super::print_profile_artifact(report_path);
    println!("\n{}", report.to_text());
    outcome
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

/// Two linear passes over the existing trace. Additional memory is O(selected
/// tiles), plus one invocation's witnesses and the retained compiled artifacts.
/// The injected operations keep coverage/failure tests independent of a zkVM.
fn replay_selected<'a, P>(
    steps: impl Iterator<Item = &'a StepRecord> + Clone,
    report: &mut ReplayProfile,
    mut prepare: impl FnMut(&str) -> Result<(P, String)>,
    mut execute: impl FnMut(&P, &StepRecord) -> Result<u64>,
    mut progress: impl FnMut(u64, u64),
) -> Result<()> {
    let mut total = 0;
    for step in steps.clone() {
        if let Some(stats) = tile_id(step).and_then(|id| report.tiles.get_mut(id)) {
            stats.invocations += 1;
            if stats.invocations <= REPLAY_INVOCATION_LIMIT {
                total += 1;
            }
        }
    }
    let mut prepared = BTreeMap::new();
    let mut done = 0;
    for step in steps {
        let Some(tile) = tile_id(step).filter(|id| report.tiles.contains_key(*id)) else {
            continue;
        };
        // The budget belongs to the tile ID, including recursive iterations
        // and calls at other sites. Skip before loading any witness or guest.
        if report.tiles[tile].profiled_invocations >= REPLAY_INVOCATION_LIMIT {
            continue;
        }
        let location = InvocationLocation {
            exec_index: step.exec_index,
            coordinates: step.coordinates.clone(),
        };
        let outcome = (|| {
            if !prepared.contains_key(tile) {
                let (artifact, image_id) = prepare(tile)?;
                report.tiles.get_mut(tile).unwrap().image_id = Some(image_id);
                prepared.insert(tile.to_string(), artifact);
            }
            let cycles = execute(&prepared[tile], step)?;
            report.record(tile, location.clone(), cycles)
        })();
        if let Err(error) = outcome {
            report.failure = Some(ReplayProfileFailure {
                tile: tile.into(),
                invocation: location,
                message: error.to_string(),
            });
            return Err(Error::Other(format!(
                "Replay profiling failed for '{tile}' at {:?}: {error}",
                step.coordinates.0,
            )));
        }
        done += 1;
        progress(done, total);
        if done == total {
            break;
        }
    }
    report.complete = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::{cfs::CfsCoordinates, trace::StorageRoots};

    fn step(index: u32, target: ExecTarget, coordinates: Vec<u32>) -> StepRecord {
        StepRecord {
            exec_index: index as u64,
            sequence_id: "main".into(),
            coordinates: CfsCoordinates(coordinates),
            kind: StepKind::Exec(ExecStep {
                target,
                intra_sequence_index: index,
                input_commitment: vec![],
                input_source_commitment: vec![],
                output_commitment: vec![],
                storage: StorageRoots {
                    root_before: vec![],
                    root_after: vec![],
                    index_root_before: vec![],
                    index_root_after: vec![],
                },
            }),
            recur_progress_commitment: [0; 32],
        }
    }

    #[test]
    fn profiles_all_selected_invocations_once_including_recur_iterations() {
        let mut steps = vec![
            step(0, ExecTarget::Tile("other".into()), vec![0]),
            step(1, ExecTarget::Tile("chosen".into()), vec![1]),
            step(2, ExecTarget::Tile("chosen".into()), vec![2, 0]),
            step(3, ExecTarget::Tile("chosen".into()), vec![2, 1]),
            step(4, ExecTarget::RecurTile("chosen".into()), vec![2]),
            step(5, ExecTarget::Tile("second".into()), vec![3]),
        ];
        let mut boundary = steps[0].clone();
        boundary.kind = StepKind::SequenceEnd {
            output_commitment: vec![],
        };
        steps.push(boundary);
        let mut report = ReplayProfile::new(
            "run".into(),
            ["chosen", "second", "unused"].map(String::from),
        );
        let mut preparations = Vec::new();
        let mut executions = Vec::new();
        replay_selected(
            steps.iter(),
            &mut report,
            |id| {
                preparations.push(id.to_string());
                Ok((id.to_string(), format!("image-{id}")))
            },
            |id, step| {
                executions.push((id.clone(), step.exec_index));
                Ok(step.exec_index * 100)
            },
            |_, _| {},
        )
        .unwrap();
        assert_eq!(preparations, ["chosen", "second"]);
        assert_eq!(
            executions
                .iter()
                .map(|(_, index)| *index)
                .collect::<Vec<_>>(),
            [1, 2, 3, 5]
        );
        assert_eq!(report.tiles["chosen"].invocations, 3);
        assert_eq!(report.tiles["chosen"].profiled_invocations, 3);
        assert_eq!(report.tiles["chosen"].max_guest_cycles, Some(300));
        assert_eq!(
            report.tiles["chosen"]
                .heaviest_invocation
                .as_ref()
                .unwrap()
                .coordinates
                .0,
            [2, 1]
        );
        assert_eq!(report.tiles["unused"].invocations, 0);
        assert_eq!(report.total_guest_cycles, 1100);
        assert!(report.complete);
    }

    #[test]
    fn stops_on_failure_and_preserves_partial_totals_and_location() {
        let steps: Vec<_> = (0..3)
            .map(|i| step(i, ExecTarget::Tile("tile".into()), vec![i]))
            .collect();
        let mut report = ReplayProfile::new("run".into(), ["tile".into()]);
        let mut calls = 0;
        let error = replay_selected(
            steps.iter(),
            &mut report,
            |_| Ok(((), "image".into())),
            |_, step| {
                calls += 1;
                if step.exec_index == 1 {
                    Err(Error::Other("Missing recorded input bytes".into()))
                } else {
                    Ok(42)
                }
            },
            |_, _| {},
        )
        .unwrap_err();
        assert_eq!(calls, 2);
        assert!(!report.complete);
        assert_eq!(report.tiles["tile"].invocations, 3);
        assert_eq!(report.tiles["tile"].profiled_invocations, 1);
        assert_eq!(report.total_guest_cycles, 42);
        assert_eq!(
            report.failure.as_ref().unwrap().invocation.coordinates.0,
            [1]
        );
        assert!(error.to_string().contains("Missing recorded input bytes"));
        assert!(report.to_text().contains("PARTIAL"));
    }

    #[test]
    fn compilation_failure_is_a_partial_profile_without_an_execution() {
        let step = step(0, ExecTarget::Tile("tile".into()), vec![0]);
        let mut report = ReplayProfile::new("run".into(), ["tile".into()]);
        let outcome = replay_selected(
            std::iter::once(&step),
            &mut report,
            |_| -> Result<((), String)> { Err(Error::Other("toolchain missing".into())) },
            |_, _| panic!("must not execute after a preparation failure"),
            |_, _| {},
        );
        assert!(outcome.is_err());
        assert_eq!(report.total_guest_cycles, 0);
        assert_eq!(report.failure.unwrap().tile, "tile");
    }

    #[test]
    fn million_invocations_keep_one_aggregate_and_one_prepared_artifact() {
        let step = step(0, ExecTarget::Tile("tile".into()), vec![0]);
        let mut report = ReplayProfile::new("large-run".into(), ["tile".into()]);
        let mut prepares = 0;
        let mut calls = 0;
        replay_selected(
            std::iter::repeat(&step).take(1_000_000),
            &mut report,
            |_| {
                prepares += 1;
                Ok(((), "image".into()))
            },
            |_, _| {
                calls += 1;
                Ok(10)
            },
            |_, _| {},
        )
        .unwrap();
        assert_eq!(prepares, 1);
        assert_eq!(calls, 128);
        assert_eq!(report.tiles.len(), 1);
        assert_eq!(report.tiles["tile"].invocations, 1_000_000);
        assert_eq!(report.tiles["tile"].profiled_invocations, 128);
        assert_eq!(report.total_guest_cycles, 1280);
        assert!(serde_json::to_vec(&report).unwrap().len() < 1000);
    }

    #[test]
    fn replays_up_to_the_limit_and_progress_counts_only_scheduled_replays() {
        for count in [0, 1, 127, 128, 129, 1000] {
            let step = step(0, ExecTarget::Tile("tile".into()), vec![0]);
            let mut report = ReplayProfile::new("run".into(), ["tile".into()]);
            let mut calls = 0;
            let mut preparations = 0;
            let mut last_progress = None;
            replay_selected(
                std::iter::repeat(&step).take(count),
                &mut report,
                |_| {
                    preparations += 1;
                    Ok(((), "image".into()))
                },
                |_, _| {
                    calls += 1;
                    Ok(10)
                },
                |done, total| {
                    last_progress = Some((done, total));
                },
            )
            .unwrap();
            let expected = (count as u64).min(128);
            assert_eq!(calls, expected);
            assert_eq!(preparations, usize::from(count > 0));
            assert_eq!(report.tiles["tile"].invocations, count as u64);
            assert_eq!(report.tiles["tile"].profiled_invocations, expected);
            assert_eq!(report.total_guest_cycles, expected * 10);
            assert_eq!(last_progress, (count > 0).then_some((expected, expected)));
            assert!(report.complete);
            assert!(report.failure.is_none());
        }
    }

    #[test]
    fn independent_tile_limits_span_recursive_iterations_and_multiple_call_sites() {
        let mut steps = Vec::new();
        for i in 0..1000 {
            // Two sites invoking the same recursive tile; neither site's
            // enclosing record consumes an invocation from the tile budget.
            let site = if i < 64 { 2 } else { 4 };
            if i == 0 || i == 64 {
                steps.push(step(
                    steps.len() as u32,
                    ExecTarget::RecurTile("recursive".into()),
                    vec![site],
                ));
            }
            steps.push(step(
                steps.len() as u32,
                ExecTarget::Tile("recursive".into()),
                vec![site, if i < 64 { i } else { i - 64 }],
            ));
            if i < 129 {
                steps.push(step(
                    steps.len() as u32,
                    ExecTarget::Tile("ordinary".into()),
                    vec![10 + i % 2, i],
                ));
            }
            steps.push(step(
                steps.len() as u32,
                ExecTarget::Tile("unselected".into()),
                vec![20, i],
            ));
        }
        // A selected tile after both heavy tiles have exhausted their budgets
        // must still execute. Do not stop after a global total of 128 replays.
        steps.push(step(
            steps.len() as u32,
            ExecTarget::Tile("late".into()),
            vec![30],
        ));
        let expected: BTreeMap<_, Vec<_>> = ["recursive", "ordinary", "late"]
            .into_iter()
            .map(|id| {
                (
                    id.to_string(),
                    steps
                        .iter()
                        .filter(|step| tile_id(step) == Some(id))
                        .take(128)
                        .map(|step| step.exec_index)
                        .collect(),
                )
            })
            .collect();
        let mut report = ReplayProfile::new(
            "run".into(),
            ["recursive", "ordinary", "late", "unused"].map(String::from),
        );
        let mut preparations = Vec::new();
        let mut executions: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        let mut last_progress = None;
        replay_selected(
            steps.iter(),
            &mut report,
            |id| {
                preparations.push(id.to_string());
                Ok((id.to_string(), "image".into()))
            },
            |id, step| {
                let calls = executions.entry(id.clone()).or_default();
                assert!(
                    calls.len() < 128,
                    "must not load witnesses or execute beyond the limit"
                );
                calls.push(step.exec_index);
                Ok(step.exec_index + 1)
            },
            |done, total| {
                last_progress = Some((done, total));
            },
        )
        .unwrap();
        assert_eq!(executions, expected);
        assert_eq!(preparations, ["recursive", "ordinary", "late"]);
        assert_eq!(report.tiles["recursive"].invocations, 1000);
        assert_eq!(report.tiles["ordinary"].invocations, 129);
        assert_eq!(report.tiles["unused"].profiled_invocations, 0);
        assert_eq!(last_progress, Some((257, 257)));
        for (id, indices) in &expected {
            let last = *indices.last().unwrap();
            let stats = &report.tiles[id];
            assert_eq!(
                stats.total_guest_cycles,
                indices.iter().map(|i| i + 1).sum::<u64>()
            );
            assert_eq!(stats.max_guest_cycles, Some(last + 1));
            assert_eq!(
                stats.heaviest_invocation.as_ref().unwrap().coordinates,
                steps[last as usize].coordinates
            );
        }
        assert_eq!(
            report.total_guest_cycles,
            report
                .tiles
                .values()
                .map(|tile| tile.total_guest_cycles)
                .sum::<u64>()
        );
        assert!(report.complete);
    }

    #[test]
    fn failure_on_last_allowed_invocation_stays_partial() {
        let steps: Vec<_> = (0..130)
            .map(|i| step(i, ExecTarget::Tile("tile".into()), vec![i]))
            .collect();
        let mut report = ReplayProfile::new("run".into(), ["tile".into()]);
        let mut calls = 0;
        let result = replay_selected(
            steps.iter(),
            &mut report,
            |_| Ok(((), "image".into())),
            |_, step| {
                calls += 1;
                if step.exec_index == 127 {
                    Err(Error::Other("guest failed".into()))
                } else {
                    Ok(10)
                }
            },
            |_, _| {},
        );
        assert!(result.is_err());
        assert_eq!(calls, 128);
        assert!(!report.complete);
        assert_eq!(report.tiles["tile"].invocations, 130);
        assert_eq!(report.tiles["tile"].profiled_invocations, 127);
        assert_eq!(report.total_guest_cycles, 1270);
        assert_eq!(report.failure.as_ref().unwrap().invocation.exec_index, 127);
        assert!(report.to_text().contains("PARTIAL"));
    }
}
