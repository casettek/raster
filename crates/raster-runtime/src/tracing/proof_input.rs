use crate::TraceRecorder;
use raster_core::cfs::CfsCoordinates;
use raster_core::input::{SelectionWitness, StorageRef};
use raster_core::merkle::{to_node_hash, EMPTY_LEAF};
use raster_core::trace::{FnInput, StepKind, StepRecord};
use raster_core::transition::{
    StorageEntry, StorageIndexValue, StorageLogWitness, StorageReadWitness, StorageWitness,
    StorageWriteWitness,
};
use raster_prover::trace::{Bytes, MemoryWitnessTree, SerializableFrontier, TraceTree};
use raster_prover::transition::StepIo;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
pub struct ProofStorageState {
    frontier: SerializableFrontier,
    append_tree: ProofLog,
    coordinate_index:
        raster_core::coordinate_index::IncrementalCoordinateIndex<crate::storage::IndexStorage>,
}

fn empty_storage_frontier() -> SerializableFrontier {
    SerializableFrontier {
        position: 0,
        leaf: EMPTY_LEAF.to_vec(),
        ommers: Vec::new(),
    }
}

fn storage_leaf_hash(entry: &StorageEntry) -> Vec<u8> {
    Sha256::digest(entry.to_bytes()).to_vec()
}

enum ProofLog {
    Memory(MemoryWitnessTree),
    Recorded(raster_prover::archive::DiskVec<StorageEntry>),
}
impl ProofLog {
    fn append(&mut self, entry: &StorageEntry) {
        match self {
            Self::Memory(tree) => {
                assert!(tree.append(Bytes(storage_leaf_hash(entry))));
            }
            Self::Recorded(entries) => entries.push(entry.clone()),
        }
    }
}
fn build_storage_log_witness(log: &ProofLog, log_position: u64) -> StorageLogWitness {
    let path = match log {
        ProofLog::Memory(tree) => tree.witness(log_position).expect("storage log witness"),
        ProofLog::Recorded(entries) => {
            // Preserve the disk-backed compatibility path. Live audit never
            // constructs this variant or reconstructs an execution prefix.
            let mut tree = TraceTree::new(1);
            assert!(tree.append(Bytes(EMPTY_LEAF.to_vec())));
            for (index, entry) in entries.iter().enumerate() {
                assert!(tree.append(Bytes(storage_leaf_hash(&entry))));
                if index as u64 + 1 == log_position {
                    tree.mark();
                }
            }
            tree.witness(log_position.into(), 0)
                .expect("recorded storage witness")
        }
    };
    StorageLogWitness {
        position: log_position,
        path_elems: path.iter().map(|elem| to_node_hash(&elem.0)).collect(),
    }
}

fn apply_storage_write_to_state(
    state: &mut ProofStorageState,
    storage_write: &crate::StorageWriteRecord,
) {
    state.frontier = storage_write.frontier_after.clone();
    state.append_tree.append(&storage_write.entry);
    state.coordinate_index.insert(
        storage_write.entry.coordinates.clone(),
        StorageIndexValue {
            log_position: storage_write.log_position,
            object_commitment: storage_write.entry.object_commitment.clone(),
        },
    );
}

impl ProofStorageState {
    pub fn new() -> Self {
        let mut append_tree = MemoryWitnessTree::new();
        assert!(append_tree.append(Bytes(EMPTY_LEAF.to_vec())));
        Self {
            frontier: empty_storage_frontier(),
            append_tree: ProofLog::Memory(append_tree),
            coordinate_index:
                raster_core::coordinate_index::IncrementalCoordinateIndex::with_storage(
                    crate::storage::IndexStorage::Memory(Default::default()),
                ),
        }
    }
    pub fn for_recorded() -> Self {
        Self {
            frontier: empty_storage_frontier(),
            append_tree: ProofLog::Recorded(raster_prover::archive::DiskVec::new()),
            coordinate_index:
                raster_core::coordinate_index::IncrementalCoordinateIndex::with_storage(
                    crate::storage::IndexStorage::Disk(Default::default()),
                ),
        }
    }
    pub fn advance(&mut self, step: &StepRecord, recorder: &TraceRecorder) {
        if step.appends_to_storage() {
            if let Some(write) = recorder
                .step_witness_for(step)
                .and_then(|w| w.storage_write())
            {
                apply_storage_write_to_state(self, &write);
            }
        }
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
fn build_entrypoint_membership_witness(state: &ProofStorageState) -> Option<StorageReadWitness> {
    let coordinates = CfsCoordinates(vec![]);
    let index_witness = state.coordinate_index.membership_proof(&coordinates)?;
    let log_witness =
        build_storage_log_witness(&state.append_tree, index_witness.value.log_position);
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
    current_storage_state: &mut ProofStorageState,
) -> StepIo {
    let step_witness = trace_recorder
        .step_witness_for(&step_record)
        .unwrap_or_else(|| {
            panic!(
                "Missing recorded I/O for fraud window step at coordinates {:?}",
                step_record.coordinates()
            )
        });
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
    let sequence_scope_witness = trace_recorder.sequence_scope_for(step_record);
    let storage_selection_witnesses = build_storage_selection_witnesses(
        step_record,
        input_source_witness.as_ref(),
        trace_recorder,
    );
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
                &current_storage_state.append_tree,
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
    let (program_output_read_witness, program_output_selection_witness) = match &step_record.kind {
        StepKind::ProgramEnd(program_end) => match &program_end.output {
            Some(output) => {
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
                    &current_storage_state.append_tree,
                    index_witness.value.log_position,
                );
                let read = StorageReadWitness {
                    entry,
                    log_witness,
                    index_witness,
                };
                let selection = if output.selection.selected_len > 0 {
                    let reference =
                        StorageRef::new(output.coordinates.clone(), output.commitment.clone());
                    Some(
                        trace_recorder
                            .storage_selection_witness(
                                &reference,
                                &output.selector,
                                output.selection.payload_kind,
                            )
                            .unwrap_or_else(|error| {
                                panic!(
                                    "Failed to build program output selection witness: {}",
                                    error
                                )
                            }),
                    )
                } else {
                    None
                };
                (Some(read), selection)
            }
            None => (None, None),
        },
        _ => (None, None),
    };

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
                    &current_storage_state.append_tree,
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
            apply_storage_write_to_state(current_storage_state, &storage_write);
            let index_membership_witness = current_storage_state
                .coordinate_index
                .membership_proof(&entry.coordinates)
                .expect("Missing coordinate-index membership proof after write");
            storage_write_witness = Some(StorageWriteWitness {
                entry,
                index_non_membership_witness,
                index_membership_witness,
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
