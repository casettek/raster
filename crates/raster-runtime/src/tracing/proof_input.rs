use crate::tracing::recorder::StepWitnessData;
use crate::TraceRecorder;
use raster_core::cfs::CfsCoordinates;
use raster_core::input::{SelectionWitness, StorageRef};
use raster_core::merkle::{to_node_hash, EMPTY_LEAF};
use raster_core::trace::{FnInput, StepKind, StepRecord};
use raster_core::transition::{
    StorageEntry, StorageIndexValue, StorageLogWitness, StorageReadWitness, StorageWitness,
    StorageWriteWitness,
};
use raster_prover::trace::{Bytes, MemoryWitnessTree, SerializableFrontier};
use raster_prover::transition::StepIo;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
/// Storage as it stood at one moment of a run: the storage log tree, the
/// coordinate index and the log frontier after a given number of writes,
/// moving forward one write at a time ([`Self::apply`]). The proof state a
/// fault window opens with comes from [`crate::StorageLedger::as_of`].
pub struct LedgerAt {
    frontier: SerializableFrontier,
    log: MemoryWitnessTree,
    coordinate_index: raster_core::coordinate_index::IncrementalCoordinateIndex,
}

pub fn empty_storage_frontier() -> SerializableFrontier {
    SerializableFrontier {
        position: 0,
        leaf: EMPTY_LEAF.to_vec(),
        ommers: Vec::new(),
    }
}

fn storage_leaf_hash(entry: &StorageEntry) -> Vec<u8> {
    Sha256::digest(entry.to_bytes()).to_vec()
}

fn build_storage_log_witness(log: &MemoryWitnessTree, log_position: u64) -> StorageLogWitness {
    let path = log.witness(log_position).expect("storage log witness");
    StorageLogWitness {
        position: log_position,
        path_elems: path.iter().map(|elem| to_node_hash(&elem.0)).collect(),
    }
}

impl Default for LedgerAt {
    fn default() -> Self {
        Self::new()
    }
}

impl LedgerAt {
    /// Storage before any write: the log holds only its seed.
    pub fn new() -> Self {
        let mut log = MemoryWitnessTree::new();
        assert!(log.append(Bytes(EMPTY_LEAF.to_vec())));
        Self {
            frontier: empty_storage_frontier(),
            log,
            coordinate_index: raster_core::coordinate_index::IncrementalCoordinateIndex::new(),
        }
    }

    /// Storage from its parts: `log` over the seed and the first `n` writes,
    /// `coordinate_index` over the same `n` entries.
    pub(crate) fn from_parts(
        log: MemoryWitnessTree,
        coordinate_index: raster_core::coordinate_index::IncrementalCoordinateIndex,
    ) -> Self {
        let frontier = log.frontier_at(log.len()).expect("storage log seed");
        Self {
            frontier: raster_prover::trace::serializable_frontier_from_trace_frontier(frontier),
            log,
            coordinate_index,
        }
    }

    /// Move past one write.
    pub fn apply(&mut self, storage_write: &crate::StorageWriteRecord) {
        self.frontier = storage_write.frontier_after.clone();
        assert!(self.log.append(Bytes(storage_leaf_hash(&storage_write.entry))));
        self.coordinate_index.insert(
            storage_write.entry.coordinates.clone(),
            StorageIndexValue {
                log_position: storage_write.log_position,
                object_commitment: storage_write.entry.object_commitment.clone(),
            },
        );
    }

    pub fn initial(
        &self,
        with_entrypoint: bool,
    ) -> (SerializableFrontier, Vec<u8>, Option<StorageReadWitness>) {
        (
            self.frontier.clone(),
            self.coordinate_index.root(),
            with_entrypoint
                .then(|| build_entrypoint_membership_witness(self))
                .flatten(),
        )
    }
}

fn build_entrypoint_membership_witness(state: &LedgerAt) -> Option<StorageReadWitness> {
    let coordinates = CfsCoordinates(vec![]);
    let index_witness = state.coordinate_index.membership_proof(&coordinates)?;
    let log_witness = build_storage_log_witness(&state.log, index_witness.value.log_position);
    Some(StorageReadWitness {
        entry: StorageEntry {
            coordinates,
            object_commitment: index_witness.value.object_commitment.clone(),
        },
        log_witness,
        index_witness,
    })
}

fn build_storage_selection_witnesses(
    step_record: &StepRecord,
    input_source_witness: Option<&FnInput>,
    trace_recorder: &TraceRecorder,
) -> BTreeMap<String, SelectionWitness> {
    let Some(input_source_witness) = input_source_witness else {
        return BTreeMap::new();
    };

    input_source_witness
        .storage()
        .iter()
        .filter_map(|(binding_name, storage)| {
            if storage.selection.selected_len == 0 {
                return None;
            }
            let reference =
                StorageRef::new(storage.coordinates.clone(), storage.commitment.clone());
            let witness = trace_recorder
                // The recorded commitment says which view of the node it
                // committed to. This process did not produce the trace, so
                // it cannot infer that from the payload — it has none yet.
                .storage_selection_witness(
                    &reference,
                    &storage.selector,
                    storage.selection.payload_kind,
                )
                .unwrap_or_else(|error| {
                    panic!(
                        "Failed to build storage selection witness for '{}' at step {} ({:?}), storage {:?}, selector {:?}: {}",
                        binding_name, step_record.exec_index, step_record.coordinates(), reference.coordinates, storage.selector, error
                    )
                });

            // A binding the step only forwards as a reference ships its root
            // instead of its value. The payload is read once here, on the
            // host, and never reaches the guest — which is the whole point:
            // `prompt-prepare` forwards a 106 MB `List<MergeBucket>` into a
            // sequence whose recorded input is 145 bytes, and rebuilding that
            // list's Merkle tree exhausted the guest heap.
            //
            // The same `binding_requires_payload` the guest enforces, so the
            // host cannot ship a shape the guest will reject — and cannot
            // choose a weaker one either.
            let witness = if raster_core::trace::binding_requires_payload(
                &step_record.kind,
                binding_name.as_str(),
                input_source_witness.storage(),
            ) {
                witness
            } else {
                witness.clone().into_reference().unwrap_or_else(|| {
                    panic!(
                        "Failed to reduce forwarded binding '{}' to a selection reference",
                        binding_name
                    )
                })
            };

            Some((binding_name.clone(), witness))
        })
        .collect()
}

/// Assemble the canonical `program.bin` frame for the fraud-proof guest by
/// reassembling the `ProgramDefinition` from source (CFS + compiled tile
/// registry + `Raster.toml`/synthesized manifest) and verifying it against
/// `Raster.lock` if present (the stale-lock drift check). Every window step
/// uses this same frame, so `program_commitment` continuity holds by
/// construction. See `docs/proposals/program-identity.md`.
pub fn prepare_step_io(
    step_record: &StepRecord,
    trace_recorder: &TraceRecorder,
    current_storage_state: &mut LedgerAt,
) -> StepIo {
    build_step_io(
        &capture_step_io(step_record, trace_recorder),
        trace_recorder,
        current_storage_state,
    )
}

/// What a step's evidence needs that the recorder does not keep: the step's
/// recorded I/O and its parent's scope input, which a live recorder releases
/// once the step is recorded. Selections and storage proofs are rebuilt later
/// from stores that only grow.
#[derive(Debug, Clone)]
pub struct CapturedStepIo {
    step_record: StepRecord,
    step_witness: StepWitnessData,
    sequence_scope_witness: Option<FnInput>,
}

impl CapturedStepIo {
    pub fn step_record(&self) -> &StepRecord {
        &self.step_record
    }

    /// The write this step applies to the proof state, under the same
    /// condition as [`build_step_storage`].
    pub fn storage_write(&self) -> Option<crate::StorageWriteRecord> {
        if self.step_record.appends_to_storage() {
            self.step_witness.storage_write()
        } else {
            None
        }
    }
}

pub fn capture_step_io(step_record: &StepRecord, trace_recorder: &TraceRecorder) -> CapturedStepIo {
    let step_witness = trace_recorder
        .step_witness_for(step_record)
        .unwrap_or_else(|| {
            panic!(
                "Missing recorded I/O for fraud window step at coordinates {:?}",
                step_record.coordinates()
            )
        });
    CapturedStepIo {
        step_record: step_record.clone(),
        step_witness,
        sequence_scope_witness: trace_recorder.sequence_scope_for(step_record),
    }
}

/// A step's evidence: [`build_step_selections`], then [`build_step_storage`].
pub fn build_step_io(
    captured: &CapturedStepIo,
    trace_recorder: &TraceRecorder,
    current_storage_state: &mut LedgerAt,
) -> StepIo {
    build_step_storage(
        captured,
        build_step_selections(captured, trace_recorder),
        current_storage_state,
    )
}

/// A step's selection witnesses: reads of stored objects through the
/// recorder. Objects never change once written, so these are the same
/// whenever they are built. They do not depend on the proof state, so a
/// window's can all be built before that state is rewound
/// ([`crate::StorageLedger::rewind_to`]), while the recorder still holds every
/// object the window wrote.
pub struct StepSelections {
    storage: BTreeMap<String, SelectionWitness>,
    program_output: Option<SelectionWitness>,
}

pub fn build_step_selections(
    captured: &CapturedStepIo,
    trace_recorder: &TraceRecorder,
) -> StepSelections {
    let step_record = &captured.step_record;
    let input_source_witness = step_record
        .input_source_commitment()
        .and_then(|_| captured.step_witness.input_source_witness());
    let storage = build_storage_selection_witnesses(
        step_record,
        input_source_witness.as_ref(),
        trace_recorder,
    );
    // A `ProgramEnd` step proves the selection that narrows its output
    // object to the returned value; the read proof is the storage half.
    let program_output = match &step_record.kind {
        StepKind::ProgramEnd(program_end) => program_end
            .output
            .as_ref()
            .filter(|output| output.selection.selected_len > 0)
            .map(|output| {
                let reference =
                    StorageRef::new(output.coordinates.clone(), output.commitment.clone());
                trace_recorder
                    .storage_selection_witness(
                        &reference,
                        &output.selector,
                        output.selection.payload_kind,
                    )
                    .unwrap_or_else(|error| {
                        panic!("Failed to build program output selection witness: {}", error)
                    })
            }),
        _ => None,
    };
    StepSelections {
        storage,
        program_output,
    }
}

/// A step's evidence against `current_storage_state`, the proof state as of
/// that step, which it then advances past the step's write.
pub fn build_step_storage(
    captured: &CapturedStepIo,
    selections: StepSelections,
    current_storage_state: &mut LedgerAt,
) -> StepIo {
    let step_record = &captured.step_record;
    let step_witness = &captured.step_witness;
    // The witness store is keyed by coordinates alone, and a sequence's
    // `SequenceStart` and `SequenceEnd` share coordinates — the End does
    // `get_mut` on the Start's entry and fills in `output_data` only
    // (`recorder.rs`, which already guards the same sharing for
    // `storage_write`). So both input fields still hold the *Start's*
    // values when this step is the End. Take each only when this step's
    // record actually declares the matching commitment: the guest refuses
    // an input source witness on a step that commits to none, and never
    // reads an input witness there.
    let input_witness = step_record
        .input_commitment()
        .and_then(|_| step_witness.input_data());
    let output_witness = step_witness.output_data();
    let input_source_witness = step_record
        .input_source_commitment()
        .and_then(|_| step_witness.input_source_witness());
    let sequence_scope_witness = captured.sequence_scope_witness.clone();
    let storage_selection_witnesses = selections.storage;
    let draft_transition_witness = step_witness.draft_transition_witness();

    let mut storage_read_witnesses = Vec::new();
    // Only a step that declares storage roots can carry a storage witness:
    // the guest has nothing to verify membership *against* otherwise, and
    // refuses one outright ("Only execution steps may carry storage
    // witnesses"). A `SequenceStart` legitimately has storage bindings —
    // that is how a sequence receives its arguments — so without this gate
    // the reads get built for it and the proof is rejected.
    //
    // The write side already guards the same way one block below; this is
    // the read half of it. A sequence step is transparent to the storage
    // chain: the guest forwards its carried frontier unchanged, so there is
    // no root here for a membership proof to be relative to. The bindings
    // are proved where they are consumed, against that step's own root.
    // The exception is a recur site `Start`: it claims read-only roots
    // (`StepKind::SequenceStart::storage`), so the gate below admits it.
    //
    // Gated here rather than on the `StorageWitness` that wraps the loop so
    // the proofs are never built: each binding costs a coordinate-index
    // membership proof and a log witness whose only consumer is that
    // discarded witness.
    let step_reads_storage = step_record.storage_roots().is_some();
    if let Some(input_source_witness_ref) =
        input_source_witness.as_ref().filter(|_| step_reads_storage)
    {
        for storage_meta in input_source_witness_ref.storage().values() {
            let index_witness = current_storage_state
                .coordinate_index
                .membership_proof(&storage_meta.coordinates)
                .unwrap_or_else(|| {
                    panic!(
                        "Missing coordinate-index witness for storage input at {:?}",
                        storage_meta.coordinates
                    )
                });
            let entry = StorageEntry {
                coordinates: storage_meta.coordinates.clone(),
                object_commitment: storage_meta.commitment.clone(),
            };
            let log_witness = build_storage_log_witness(
                &current_storage_state.log,
                index_witness.value.log_position,
            );
            storage_read_witnesses.push(StorageReadWitness {
                entry,
                log_witness,
                index_witness,
            });
        }
    }
    // A `ProgramEnd` step reads its output object from the current storage
    // state (which already contains it) and proves the selection that
    // narrows to the returned value — the same read + selection machinery
    // tile inputs use. The witnesses come straight from the recorded
    // output binding, not the shared `[]` witness-store entry.
    let program_output_read_witness = match &step_record.kind {
        StepKind::ProgramEnd(program_end) => program_end.output.as_ref().map(|output| {
            let index_witness = current_storage_state
                .coordinate_index
                .membership_proof(&output.coordinates)
                .unwrap_or_else(|| {
                    panic!(
                        "Missing coordinate-index witness for program output at {:?}",
                        output.coordinates
                    )
                });
            let entry = StorageEntry {
                coordinates: output.coordinates.clone(),
                object_commitment: output.commitment.clone(),
            };
            let log_witness = build_storage_log_witness(
                &current_storage_state.log,
                index_witness.value.log_position,
            );
            StorageReadWitness {
                entry,
                log_witness,
                index_witness,
            }
        }),
        _ => None,
    };
    let program_output_selection_witness = selections.program_output;

    // A recur-sequence iteration's `End` that carries state reads the
    // object it returned as that state, against the current storage state
    // (D5b). Its output witness is that binding.
    let returned_state_read_witness = match (&step_record.kind, &step_record.recur_state) {
        (StepKind::SequenceEnd { .. }, Some(_)) => output_witness
            .as_ref()
            .and_then(|bytes| {
                raster_core::postcard::from_bytes::<Option<raster_core::trace::StorageData>>(bytes)
                    .ok()
                    .flatten()
            })
            .map(|returned| {
                let index_witness = current_storage_state
                    .coordinate_index
                    .membership_proof(&returned.coordinates)
                    .unwrap_or_else(|| {
                        panic!(
                            "Missing coordinate-index witness for returned state at {:?}",
                            returned.coordinates
                        )
                    });
                let log_witness = build_storage_log_witness(
                    &current_storage_state.log,
                    index_witness.value.log_position,
                );
                StorageReadWitness {
                    entry: StorageEntry {
                        coordinates: returned.coordinates,
                        object_commitment: returned.commitment,
                    },
                    log_witness,
                    index_witness,
                }
            }),
        _ => None,
    };

    let mut storage_write_witness = None;
    // Only a storage-appending step owns the write recorded at its
    // coordinate. `ProgramEnd` shares coordinates `[]` (and thus a
    // witness-store entry) with `ProgramStart`, so without this gate it
    // would re-apply `ProgramStart`'s entry-object write.
    if step_record.appends_to_storage() {
        if let Some(storage_write) = step_witness.storage_write() {
            let entry = storage_write.entry.clone();
            let index_non_membership_witness = current_storage_state
                .coordinate_index
                .non_membership_proof(&entry.coordinates);
            // The guest derives the new index root from this insertion proof;
            // main no longer sends a second membership proof after the write.
            current_storage_state.apply(&storage_write);
            storage_write_witness = Some(StorageWriteWitness {
                entry,
                index_non_membership_witness,
            });
        }
    }
    let storage_witness = if storage_read_witnesses.is_empty() && storage_write_witness.is_none() {
        None
    } else {
        Some(StorageWitness {
            reads: storage_read_witnesses,
            write: storage_write_witness,
        })
    };

    StepIo {
        input_witness: input_witness.clone(),
        output_witness,
        input_source_witness,
        sequence_scope_witness,
        storage_selection_witnesses,
        storage_witness,
        draft_transition_witness,
        program_output_read_witness,
        program_output_selection_witness,
        returned_state_read_witness,
    }
}
