//! Real local witnesses for one sample, independent of fault-window building.

use std::collections::BTreeMap;

use raster_compiler::Project;
use raster_core::authorization::AuthorizationJournal;
use raster_core::cfs::{CfsCursor, ControlFlowSchema, SequenceChildItem};
use raster_core::draft::TileReplayJournal;
use raster_core::input::StorageRef;
use raster_core::program::ProgramDefinition;
use raster_core::trace::{FnInputValue, StepRecord, Trace};
use raster_core::transition::{
    StorageEntry, StorageLogWitness, StorageReadWitness, StorageWitness, TransitionInput,
};
use raster_core::{Error, Result};
use raster_prover::authorization::authorization_guest_image_id;
use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::trace::{Bytes, TraceTree};
use raster_runtime::tracing::replay_profile::ReplayProfileSample;
use raster_runtime::TraceRecorder;
use sha2::{Digest, Sha256};

pub(super) fn program_frame(
    project: &Project,
    cfs: &ControlFlowSchema,
    tile: &str,
    image_id: &str,
) -> Result<ProgramDefinition> {
    let mut manifest = crate::program::load_or_synthesize_manifest(project, cfs)?;
    raster_compiler::schema_walk::fill_schema_hashes(&project.ast, &mut manifest)?;
    let image_id: [u8; 32] = hex::decode(image_id)
        .map_err(|e| Error::Other(e.to_string()))?
        .try_into()
        .map_err(|_| Error::Other("Sample image ID must be 32 bytes".into()))?;
    let registry = cfs
        .tiles
        .iter()
        .map(|definition| {
            let image = if definition.id == tile {
                image_id
            } else {
                // Keep the real registry's size and names without compiling
                // unselected/unused tiles. This frame never leaves profiling.
                Sha256::digest(format!("raster/profile/uncompiled/v1/{}", definition.id)).into()
            };
            (definition.id.clone(), image)
        })
        .collect();
    let program = ProgramDefinition::assemble(manifest, cfs.clone(), registry)?;
    // Fail before entering the executor if a future schema edit breaks its
    // positional binary frame (in particular, optional recursive fields).
    ProgramDefinition::decode(&program.canonical_bytes())?;
    Ok(program)
}

pub(super) fn transition_input(
    trace: &Trace,
    recorder: &TraceRecorder,
    cfs: &ControlFlowSchema,
    step: &StepRecord,
    sample: &ReplayProfileSample,
    replay_journal: &TileReplayJournal,
    authorization: &AuthorizationJournal,
) -> Result<TransitionInput> {
    if sample.exec_index != step.exec_index {
        return Err(Error::Other(
            "Overhead sample is not the first recorded invocation".into(),
        ));
    }
    let witness = recorder
        .step_witness_at(step.coordinates())
        .ok_or_else(|| Error::Other("Missing transition I/O witnesses".into()))?;
    let source = witness.input_source_witness();
    let mut selections = BTreeMap::new();
    for (name, storage) in source.iter().flat_map(|input| input.storage().iter()) {
        if storage.selection.selected_len > 0 {
            selections.insert(
                name.clone(),
                recorder.storage_selection_witness(
                    &StorageRef::new(storage.coordinates.clone(), storage.commitment.clone()),
                    &storage.selector,
                    storage.selection.payload_kind,
                )?,
            );
        }
    }
    // Rebuild only the Merkle branches required by this sample. The iterator
    // borrows existing append records; no copy of the trace or store is made.
    let mut tree = TraceTree::new(1);
    tree.append(Bytes(EMPTY_TRIE_NODES[0].to_vec()));
    let mut positions: BTreeMap<_, _> = sample
        .reads
        .iter()
        .map(|proof| (proof.value.log_position, None))
        .collect();
    for prior in trace
        .iter()
        .take_while(|prior| prior.exec_index < step.exec_index)
    {
        if !prior.appends_to_storage() {
            continue;
        }
        if let Some(write) = recorder.storage_write_at(prior.coordinates()) {
            tree.append(Bytes(Sha256::digest(write.entry.to_bytes()).to_vec()));
            if let Some(position) = positions.get_mut(&write.log_position) {
                *position = tree.mark();
            }
        }
    }
    if tree.root(0).map(|root| root.0) != Some(sample.context.storage_root.clone()) {
        return Err(Error::Other(
            "Sample storage prefix does not match its recorded root".into(),
        ));
    }
    let reads = sample
        .reads
        .iter()
        .map(|proof| {
            let position = positions
                .get(&proof.value.log_position)
                .copied()
                .flatten()
                .ok_or_else(|| Error::Other("Missing sample storage append position".into()))?;
            let path = tree.witness(position, 0).map_err(|error| {
                Error::Other(format!("Missing sample storage append witness: {error:?}"))
            })?;
            Ok(StorageReadWitness {
                entry: StorageEntry {
                    coordinates: proof.coordinates.clone(),
                    object_commitment: proof.value.object_commitment.clone(),
                },
                log_witness: StorageLogWitness {
                    position: proof.value.log_position,
                    path_elems: path.into_iter().map(|hash| hash.0).collect(),
                },
                index_witness: proof.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let sequence_scope_witness = step
        .coordinates()
        .try_parent()
        .map(|(parent, _)| {
            let mut scope = recorder
                .step_witness_at(&parent)
                .and_then(|witness| witness.input_source_witness());
            let cursor = CfsCursor::new(cfs.clone());
            let recur_sequence_scope = cursor
                .try_get_recur_iteration_coordinates(&parent)
                .and_then(|(site, _)| cursor.try_get_item(&site))
                .is_some_and(|item| matches!(item, SequenceChildItem::RecurSequence(_)));
            if recur_sequence_scope {
                if let Some(scope) = scope.as_mut() {
                    // RecurSequenceInput serializes iteration-control metadata
                    // inline; the consumed value is its recorded storage binding.
                    // The child tile receives that value, not the control wrapper.
                    // Resolve the modeled predecessor's scope without changing the
                    // sampled step, its input bytes, or any selection metadata.
                    let index = scope
                        .args
                        .iter()
                        .position(|arg| arg.name == "input")
                        .ok_or_else(|| {
                            Error::Other("Missing recur-sequence input argument".into())
                        })?;
                    if !scope.storage.contains_key("input") {
                        return Err(Error::Other(
                            "Missing recur-sequence storage binding".into(),
                        ));
                    }
                    *scope.values.get_mut(index).ok_or_else(|| {
                        Error::Other("Missing recur-sequence input value".into())
                    })? = FnInputValue::StorageBinding;
                }
            }
            Ok(scope)
        })
        .transpose()?
        .flatten();
    Ok(TransitionInput {
        step_record: step.clone(),
        replay_journal: Some(replay_journal.clone()),
        input_witness: witness.input_data(),
        output_witness: witness.output_data(),
        input_source_witness: source,
        sequence_scope_witness,
        storage_selection_witnesses: selections,
        storage_witness: Some(StorageWitness {
            reads,
            write: sample.write.clone(),
        }),
        draft_transition_witness: witness.draft_transition_witness(),
        input_sources_witnesses: Default::default(),
        authorization_image_id: authorization_guest_image_id(),
        authorization_journal: authorization.clone(),
        window_start_recur_progress: None,
        entrypoint_membership_witness: None,
        program_output_read_witness: None,
        program_output_selection_witness: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_backend_risc0::Risc0Backend;
    use raster_compiler::CfsBuilder;
    use raster_prover::replay::Replayer;
    use raster_prover::trace::BytesHashable;
    use raster_prover::transition_profile::{profile_authorization, profile_transition};
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    fn command(command: &mut Command) {
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Real production guests, unresolved executor assumptions, and recorded
    /// storage/recursive/draft witnesses. Cold build costs are intentionally
    /// opt-in and kept outside the executor timing intervals.
    #[test]
    #[ignore = "builds native and guest fixtures; requires the RISC Zero toolchain"]
    fn real_transition_overhead_validates_recorded_context_and_measures_latency() {
        let example = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/hello-tiles")
            .canonicalize()
            .unwrap();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let scratch = example.join(format!("target/transition-profile-test-{nonce}"));
        std::fs::create_dir_all(&scratch).unwrap();
        command(
            Command::new("cargo")
                .current_dir(&example)
                .args([
                    "run",
                    "--release",
                    "--features",
                    "gen-input",
                    "--bin",
                    "gen_input",
                    "--",
                ])
                .arg(&scratch),
        );
        command(
            Command::new("cargo")
                .current_dir(&example)
                .args(["build", "--release"]),
        );
        let mut project = Project::new(example).unwrap();
        let cfs = CfsBuilder::new(&project).build().unwrap();
        let input = scratch.join("input.json");
        let manifest = scratch.join("input_manifest.json");
        let trace_path = scratch.join("trace.json");
        let native_start = Instant::now();
        let mut native = Command::new(project.target_dir.join("release/hello-tiles"));
        native.current_dir(&project.root_dir).args([
            "--input",
            input.to_str().unwrap(),
            "--input-manifest",
            manifest.to_str().unwrap(),
        ]);
        crate::runtime_env::RuntimeEnv::new(&scratch)
            .authenticated(&trace_path, crate::TraceFormat::Json)
            .apply(&mut native);
        command(&mut native);
        let native_elapsed = native_start.elapsed();
        let selected: BTreeSet<String> = [
            "greet",
            "build_recur_draft_greeting",
            "decorate_address_line",
            "count_to",
        ]
        .map(String::from)
        .into();
        let start = Instant::now();
        let (trace, recorder) = super::super::run::load_trace_for_profile(
            &trace_path,
            crate::TraceFormat::Json,
            &cfs,
            input.to_str(),
            manifest.to_str(),
            &selected,
        )
        .unwrap();
        println!(
            "TIMING native execution {:.3}s; trace loading + sample capture {:.3}s",
            native_elapsed.as_secs_f64(),
            start.elapsed().as_secs_f64()
        );
        let manifested =
            crate::utils::authorization::build_manifested_inputs(manifest.to_str()).unwrap();
        let authorization = profile_authorization(&manifested).unwrap();
        project.output_dir = scratch.join("cold-guests");
        let backend = Risc0Backend::new(project.output_dir.clone())
            .with_user_crate(project.root_dir.clone())
            .with_guest_stdout(false);
        let replayer = Replayer::new(&backend, &project);
        for tile in [
            "greet",
            "build_recur_draft_greeting",
            "decorate_address_line",
        ] {
            let start = Instant::now();
            let prepared = replayer.prepare_profile(tile).unwrap();
            let cold = start.elapsed();
            let sample = recorder
                .replay_profile_sample(tile)
                .unwrap()
                .as_ref()
                .unwrap();
            let step = trace
                .iter()
                .find(|step| step.exec_index == sample.exec_index)
                .unwrap();
            let witness = recorder.step_witness_at(step.coordinates()).unwrap();
            let start = Instant::now();
            let replay = prepared
                .profile_with_journal(
                    &witness.input_data().unwrap(),
                    &witness.output_data().unwrap(),
                )
                .unwrap();
            let replay_time = start.elapsed();
            let start = Instant::now();
            let program = program_frame(&project, &cfs, tile, &prepared.image_id()).unwrap();
            let transition = transition_input(
                &trace,
                &recorder,
                &cfs,
                step,
                sample,
                &replay.journal,
                &authorization.journal,
            )
            .unwrap();
            let preparation = start.elapsed();
            let start = Instant::now();
            let cycles = profile_transition(
                &program,
                &sample.context,
                transition.clone(),
                &authorization,
            )
            .unwrap();
            println!("TIMING {tile}: cold compilation {:.3}s; warm replay {:.3}s ({} cycles); host overhead preparation {:.3}s; warm transition {:.3}s ({} cycles)",
                cold.as_secs_f64(), replay_time.as_secs_f64(), replay.cycles, preparation.as_secs_f64(), start.elapsed().as_secs_f64(), cycles);
            assert!(cycles > 0);
            assert_eq!(program.tiles.len(), cfs.tiles.len());
            assert_eq!(hex::encode(program.tiles[tile]), prepared.image_id());
            let mut broken = transition.clone();
            broken.input_witness.as_mut().unwrap().push(0);
            assert!(
                profile_transition(&program, &sample.context, broken, &authorization).is_err(),
                "input mismatch accepted"
            );
            let mut broken = transition.clone();
            broken.authorization_image_id[0] ^= 1;
            assert!(
                profile_transition(&program, &sample.context, broken, &authorization).is_err(),
                "missing authorization assumption accepted"
            );
            let mut broken = transition.clone();
            broken
                .storage_witness
                .as_mut()
                .unwrap()
                .write
                .as_mut()
                .unwrap()
                .index_non_membership_witness
                .siblings[0][0] ^= 1;
            assert!(
                profile_transition(&program, &sample.context, broken, &authorization).is_err(),
                "storage mismatch accepted"
            );
            if !sample.context.recur_progress.is_empty() {
                let mut broken = sample.context.clone();
                broken.recur_progress = Default::default();
                assert!(
                    profile_transition(&program, &broken, transition, &authorization).is_err(),
                    "missing recursive state accepted"
                );
            }
        }
        assert!(recorder.replay_profile_sample("count_to").is_none());

        // Sample the consumer of an ordinary tile output inside a recursive
        // sequence. Earlier calls to this same tile occur outside the loop,
        // so this test recorder starts capture when the first iteration opens.
        let mut consumer_recorder = TraceRecorder::new(cfs.clone());
        consumer_recorder
            .set_external_input(input.to_str(), manifest.to_str())
            .unwrap();
        let mut capturing_consumer = false;
        for line in std::io::BufRead::lines(std::io::BufReader::new(
            std::fs::File::open(&trace_path).unwrap(),
        )) {
            let event: raster_core::trace::TraceEvent =
                serde_json::from_str(&line.unwrap()).unwrap();
            if !capturing_consumer
                && matches!(
                    event,
                    raster_core::trace::TraceEvent::RecurSequenceIterationStart(_)
                )
            {
                consumer_recorder.capture_replay_profile(["push_draft_greeting_line".into()]);
                capturing_consumer = true;
            }
            consumer_recorder.record(event);
        }
        let mut sample = consumer_recorder
            .replay_profile_sample("push_draft_greeting_line")
            .unwrap()
            .as_ref()
            .unwrap()
            .clone();
        let step = trace
            .iter()
            .find(|step| step.exec_index == sample.exec_index)
            .unwrap();
        // Capture began mid-trace for this targeted fixture. Restore the full
        // recorded prefix rather than measuring an artificial empty frontier.
        let mut prefix = TraceTree::new(1);
        prefix.append(Bytes(EMPTY_TRIE_NODES[0].to_vec()));
        for prior in trace
            .iter()
            .take_while(|prior| prior.exec_index < sample.exec_index)
        {
            prefix.append(Bytes(prior.try_hash().unwrap()));
        }
        sample.context.frontier = raster_prover::trace::serializable_frontier_from_trace_frontier(
            prefix.frontier().unwrap().clone(),
        );
        let witness = consumer_recorder
            .step_witness_at(step.coordinates())
            .unwrap();
        let prepared = replayer
            .prepare_profile("push_draft_greeting_line")
            .unwrap();
        let replay = prepared
            .profile_with_journal(
                &witness.input_data().unwrap(),
                &witness.output_data().unwrap(),
            )
            .unwrap();
        let program = program_frame(
            &project,
            &cfs,
            "push_draft_greeting_line",
            &prepared.image_id(),
        )
        .unwrap();
        let transition = transition_input(
            &trace,
            &consumer_recorder,
            &cfs,
            step,
            &sample,
            &replay.journal,
            &authorization.journal,
        )
        .unwrap();
        assert!(
            profile_transition(&program, &sample.context, transition, &authorization).unwrap() > 0
        );

        // Exercise the actual aggregate/report path using warm prepared guests.
        let report_path = scratch.join("replay-profile.json");
        super::super::replay_profile::run(
            &project,
            &trace,
            &recorder,
            &cfs,
            manifest.to_str(),
            selected,
            "fixture",
            &report_path,
        )
        .unwrap();
        let report: raster_analysis::replay_profile::ReplayProfile =
            serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
        assert!(report.complete);
        assert_eq!(report.version, 3);
        assert!(report.tiles["count_to"].transition_overhead.is_none());
        for tile in [
            "greet",
            "build_recur_draft_greeting",
            "decorate_address_line",
        ] {
            let overhead = report.tiles[tile].transition_overhead.as_ref().unwrap();
            assert_eq!(
                overhead.invocation.exec_index,
                recorder
                    .replay_profile_sample(tile)
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .exec_index
            );
        }
        let (_, uncaptured) = super::super::run::load_trace_from_file(
            &trace_path,
            crate::TraceFormat::Json,
            &cfs,
            input.to_str(),
            manifest.to_str(),
        )
        .unwrap();
        assert!(super::super::replay_profile::run(
            &project,
            &trace,
            &uncaptured,
            &cfs,
            manifest.to_str(),
            ["greet".into()].into(),
            "partial",
            &report_path
        )
        .is_err());
        let partial: raster_analysis::replay_profile::ReplayProfile =
            serde_json::from_slice(&std::fs::read(&report_path).unwrap()).unwrap();
        assert!(!partial.complete);
        assert_eq!(partial.tiles["greet"].profiled_invocations, 1);
        assert!(partial.total_guest_cycles > 0);
        assert_eq!(
            partial.failure.unwrap().phase,
            raster_analysis::replay_profile::ProfileFailurePhase::TransitionOverhead
        );
        std::fs::remove_dir_all(scratch).unwrap();
    }
}
