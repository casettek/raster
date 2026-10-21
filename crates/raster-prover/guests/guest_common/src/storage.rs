//! The storage check: one step's storage transition, from the store before the
//! step to the store after it.
//!
//! Read witnesses (append-log path, coordinate-index membership, selection
//! witnesses) are checked against the store before the step, and the optional
//! write witness (index non-membership before, membership after) against the
//! roots the record claims after. Runs in the storage guest, over a segment of
//! steps ([`StorageCursor`]); the transition guest keeps
//! [`verify_storage_read_witness`] for the reads its own families make (the
//! genesis entry argument, the program output, a returned recur state). See
//! `docs/proposals/storage-prover.md`.

use std::collections::BTreeMap;

use bridgetree::NonEmptyFrontier;

use raster_core::cfs::CfsCoordinates;
use raster_core::coordinate_index::{
    verify_coordinate_index_insert, verify_coordinate_index_membership,
};
use raster_core::input::{
    verify_bytes_page_geometry, verify_selection_reference_witness, verify_selection_witness,
    SelectionWitness,
};
use raster_core::storage_check::StorageState;
use raster_core::trace::{FnInput, StepRecord, StorageRoots};
use raster_core::transition::{
    SerializableFrontier, StorageEntry, StorageIndexValue, StorageLogWitness, StorageReadWitness,
    StorageWitness, StorageWriteWitness,
};

use crate::input_source::verify_input_source_anchored;
use crate::merkle_tree::{combine_merkle_level, deserialize_frontier, frontier_root, sha256_bytes, Bytes};

/// Everything the storage check reads for one step besides the store itself.
///
/// Every field is either the record — which the transition guest equates by
/// hash — or a witness the check anchors to it: the input source witness to
/// `input_source_commitment`, the selection witnesses to the bindings the input
/// source names, the storage witness to the store before the step and the
/// roots the record claims after.
pub struct StorageInput<'a> {
    pub step_record: &'a StepRecord,
    pub input_source_witness: Option<&'a FnInput>,
    pub storage_selection_witnesses: &'a BTreeMap<String, SelectionWitness>,
    pub storage_witness: Option<&'a StorageWitness>,
}

/// The store as the storage check walks it: the append-log frontier and the
/// coordinate-index root. Only the start is host-supplied; every later state is
/// the previous step's result.
pub struct StorageCursor {
    frontier: NonEmptyFrontier<Bytes>,
    /// `frontier_root(&frontier)`, folded once per write rather than on every
    /// read of the state (`storage-prover` §Where the storage guest's cycles
    /// go): it changes only when the frontier does.
    root: Vec<u8>,
    index_root: Vec<u8>,
}

impl StorageCursor {
    pub fn new(start_frontier: &SerializableFrontier, start_index_root: Vec<u8>) -> Self {
        let frontier = deserialize_frontier(start_frontier)
            .expect("host must supply a valid storage frontier");
        Self {
            root: frontier_root(&frontier),
            frontier,
            index_root: start_index_root,
        }
    }

    /// The store's state now, as entries commit it.
    pub fn state(&self) -> StorageState {
        StorageState {
            position: self.frontier.position().into(),
            root: self.root.clone(),
            index_root: self.index_root.clone(),
        }
    }

    /// Check one step's storage transition from the current store and move
    /// the store past it.
    ///
    /// Reads nothing but its argument and the store, so a future check that
    /// needs more has to add it to [`StorageInput`], where the host has to
    /// supply it and the entry has to bind it.
    pub fn verify_step(&mut self, input: &StorageInput<'_>) {
        // Every read below is keyed by the input source witness's bindings.
        // The transition guest anchors its own copy; this anchors the one
        // checked here.
        verify_input_source_anchored(input.step_record, input.input_source_witness);
        (self.root, self.index_root) = verify_transition(
            input.step_record,
            input.input_source_witness,
            input.storage_selection_witnesses,
            input.storage_witness,
            &mut self.frontier,
            &self.root,
            &self.index_root,
        );
    }
}

pub fn storage_leaf_hash(entry: &StorageEntry) -> Vec<u8> {
    sha256_bytes(&entry.to_bytes())
}

fn append_log_root_from_witness(entry: &StorageEntry, witness: &StorageLogWitness) -> Vec<u8> {
    let mut current = storage_leaf_hash(entry);
    for (level, sibling) in witness.path_elems.iter().enumerate() {
        current = if ((witness.position >> level) & 1) == 0 {
            combine_merkle_level(level, &current, sibling.as_slice())
        } else {
            combine_merkle_level(level, sibling.as_slice(), &current)
        };
    }
    current
}

/// Verify a trace-inclusion (append-log + coordinate-index membership) proof
/// that `coordinates` commits to `commitment` in the store rooted at
/// `current_log_root`/`current_index_root`. Shared by ordinary per-step
/// storage reads and by the fraud-proof genesis check that a
/// window's initial storage state really contains an authorized
/// entry-argument binding (see `checks::entrypoint`).
pub fn verify_storage_read_witness(
    read_witness: &StorageReadWitness,
    current_log_root: &[u8],
    current_index_root: &[u8],
    coordinates: &CfsCoordinates,
    commitment: &[u8],
) {
    assert_eq!(
        read_witness.entry.coordinates, *coordinates,
        "Storage read witness coordinates do not match requested coordinates",
    );
    assert_eq!(
        read_witness.entry.object_commitment, commitment,
        "Storage read witness commitment does not match requested commitment",
    );
    assert!(
        verify_coordinate_index_membership(current_index_root, &read_witness.index_witness),
        "Storage coordinate-index membership proof is invalid",
    );
    assert_eq!(
        read_witness.index_witness.coordinates, *coordinates,
        "Coordinate-index witness coordinates do not match storage input",
    );
    assert_eq!(
        read_witness.index_witness.value.object_commitment, commitment,
        "Coordinate-index witness commitment does not match storage input commitment",
    );
    assert_eq!(
        read_witness.index_witness.value.log_position, read_witness.log_witness.position,
        "Coordinate-index witness log position does not match append-log witness position",
    );
    assert_eq!(
        append_log_root_from_witness(&read_witness.entry, &read_witness.log_witness),
        current_log_root,
        "Append-log witness does not match current storage root",
    );
}

/// A write: the entry is the one this step appends, its coordinates were
/// absent from the index before, and the index root after is the one the
/// record claims. That root is not taken from a second proof: it is computed
/// from the non-membership proof and the entry's value, which this guest
/// builds from its own append (`compact-coordinate-index` §Writes).
fn verify_storage_write_witness(
    write_witness: &StorageWriteWitness,
    current_index_root: &[u8],
    next_index_root: &[u8],
    expected_entry: &StorageEntry,
    expected_log_position: u64,
) {
    assert_eq!(
        write_witness.entry, *expected_entry,
        "Storage write witness entry does not match expected append entry",
    );
    assert_eq!(
        write_witness.index_non_membership_witness.coordinates, expected_entry.coordinates,
        "Coordinate-index non-membership proof coordinates do not match write entry",
    );
    let value = StorageIndexValue {
        log_position: expected_log_position,
        object_commitment: expected_entry.object_commitment.clone(),
    };
    let root_after = verify_coordinate_index_insert(
        current_index_root,
        &write_witness.index_non_membership_witness,
        &value,
    )
    .unwrap_or_else(|| panic!("Coordinate-index non-membership proof is invalid before write"));
    assert_eq!(
        root_after.as_slice(),
        next_index_root,
        "Coordinate-index root after write does not match the inserted entry",
    );
}

/// The storage transition of one step: advances `current_frontier` past the
/// step's write, if any, and returns the append-log and coordinate-index roots
/// after it. `current_root` is `frontier_root(current_frontier)`, which the
/// caller keeps.
fn verify_transition(
    step_record: &StepRecord,
    input_source_witness: Option<&FnInput>,
    storage_selection_witnesses: &BTreeMap<String, SelectionWitness>,
    storage_witness: Option<&StorageWitness>,
    current_frontier: &mut NonEmptyFrontier<Bytes>,
    current_root: &[u8],
    current_index_root: &[u8],
) -> (Vec<u8>, Vec<u8>) {
    // Runs for *every* step, before the storage-roots guard below. A
    // `SequenceStart` carries no roots, so it exits that guard immediately —
    // and a sequence step is exactly where a forwarded binding lives, so a
    // check placed inside the guard would never see one. Without this, a host
    // could still hand the guest a payload it has no use for, which is how a
    // 106 MB `List<MergeBucket>` reached a step whose recorded input was 145
    // bytes and exhausted the guest heap.
    if let Some(input_source_witness) = input_source_witness {
        for (binding_name, storage_meta) in input_source_witness.storage() {
            if storage_meta.selection.selected_len == 0 {
                continue;
            }
            let Some(witness) = storage_selection_witnesses.get(binding_name.as_str()) else {
                continue;
            };
            let requires_payload = raster_core::trace::binding_requires_payload(
                &step_record.kind,
                binding_name.as_str(),
                input_source_witness.storage(),
            );
            assert_eq!(
                !witness.is_reference_only(),
                requires_payload,
                "Storage input '{}' on step {:?} carries the wrong selection witness shape: \
                 a binding whose value the step consumes must carry its payload, and one the \
                 step only forwards must omit it",
                binding_name,
                step_record.coordinates,
            );
        }
    }

    if let Some(StorageRoots {
        root_before: storage_root_before,
        root_after: storage_root_after,
        index_root_before: storage_index_root_before,
        index_root_after: storage_index_root_after,
    }) = step_record.storage_roots()
    {
        assert_eq!(
            storage_root_before, current_root,
            "Execution-step storage root before does not match current storage root",
        );
        assert_eq!(
            storage_index_root_before, &current_index_root,
            "Execution-step storage index root before does not match current index root",
        );

        if let Some(input_source_witness) = input_source_witness {
            for (binding_name, storage_meta) in input_source_witness.storage() {
                let read_witness = storage_witness
                    .and_then(|witness| {
                        witness.reads.iter().find(|read| {
                            read.entry.coordinates == storage_meta.coordinates
                                && read.entry.object_commitment == storage_meta.commitment
                        })
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "Missing storage read witness for coordinates {:?}",
                            storage_meta.coordinates
                        )
                    });
                verify_storage_read_witness(
                    read_witness,
                    current_root,
                    current_index_root,
                    &storage_meta.coordinates,
                    &storage_meta.commitment,
                );
                assert_eq!(
                    storage_meta.commitment, storage_meta.selection.source_root_hash,
                    "Storage input '{}' commitment must match raster selection root",
                    binding_name,
                );
                if storage_meta.selection.selected_len > 0 {
                    let witness = storage_selection_witnesses
                        .get(binding_name.as_str())
                        .unwrap_or_else(|| {
                            panic!(
                                "Missing storage selection witness for binding '{}'",
                                binding_name
                            )
                        });
                    // Which mode is legal is decided here, from the record's
                    // own kind and this step's storage map — never from the
                    // witness's shape. A prover that could pick would always
                    // pick the cheaper one.
                    let requires_payload = raster_core::trace::binding_requires_payload(
                        &step_record.kind,
                        binding_name.as_str(),
                        input_source_witness.storage(),
                    );
                    if requires_payload {
                        assert!(
                            verify_selection_witness(&storage_meta.selection, witness),
                            "Storage input '{}' selection witness is invalid",
                            binding_name,
                        );
                        assert!(
                            verify_bytes_page_geometry(witness),
                            "Storage input '{}' bytes-page geometry is invalid",
                            binding_name,
                        );
                    } else {
                        // Forwarded as a reference: the step never saw the
                        // bytes, so the fold runs from the selected value's
                        // root and proves only that the reference resolves
                        // inside an object the store authenticates. Demanding
                        // the reference shape rather than merely accepting it
                        // keeps a large payload from being smuggled back in.
                        assert!(
                            witness.is_reference_only(),
                            "Storage input '{}' is forwarded as a reference, so its selection \
                             witness must omit the payload and carry the selected root",
                            binding_name,
                        );
                        assert!(
                            verify_selection_reference_witness(&storage_meta.selection, witness),
                            "Storage input '{}' selection reference witness is invalid",
                            binding_name,
                        );
                    }
                }
            }

            // Every binding above is now proved *individually*: its coordinates
            // commit to its commitment in the store, and its selection witness
            // folds its payload up to that commitment. What none of that
            // establishes is where a data-sourced index came from — the element
            // proof only pins the index the segment claims. Discharge that here,
            // once, over the whole map, because the obligation is relational:
            // it is about one binding citing another.
            //
            // Placed after the loop deliberately. Running it per-entry would let
            // a `BoundIndex` cite a binding that had not yet been verified.
            if let Err(violation) =
                raster_core::trace::verify_bound_index_bindings(input_source_witness.storage())
            {
                panic!("Bound-index binding is invalid: {:?}", violation);
            }
        }

        let write_witness = storage_witness.and_then(|witness| witness.write.as_ref());

        match write_witness {
            Some(write_witness) => {
                assert!(
                    step_record.appends_to_storage(),
                    "Step {:?} does not append to storage, so it must not carry a write witness",
                    step_record.coordinates,
                );
                let object_commitment = step_record
                    .output_commitment()
                    .expect("Execution step must expose output commitment")
                    .clone();

                let expected_entry = StorageEntry {
                    // A `RecurEnd` sits at `[-s]` and writes the site's
                    // object at `[s]`; every other writer writes where it sits.
                    coordinates: step_record.coordinates().opened(),
                    object_commitment,
                };
                current_frontier.append(Bytes(storage_leaf_hash(&expected_entry)));
                let next_root = frontier_root(current_frontier);
                let next_position: u64 = current_frontier.position().into();
                verify_storage_write_witness(
                    write_witness,
                    current_index_root,
                    storage_index_root_after,
                    &expected_entry,
                    next_position,
                );
                assert_eq!(
                    storage_root_after, &next_root,
                    "Execution-step storage root after does not match appended storage root",
                );

                (next_root, storage_index_root_after.clone())
            }
            None => {
                // A writing step kind that wrote nothing commits to nothing:
                // otherwise its output commitment is a free 32-byte field —
                // entropy for manufacturing a divergence — on every recur
                // iteration, which never writes (D3).
                if step_record.appends_to_storage()
                    && !matches!(step_record.kind, raster_core::trace::StepKind::ProgramStart(_))
                {
                    assert!(
                        step_record
                            .output_commitment()
                            .is_none_or(|commitment| commitment.is_empty()),
                        "Step {:?} wrote nothing, so it must record no output commitment",
                        step_record.coordinates,
                    );
                }
                assert_eq!(
                    storage_root_before, storage_root_after,
                    "Execution-step without storage write must leave append-log root unchanged",
                );
                assert_eq!(
                    storage_index_root_before, storage_index_root_after,
                    "Execution-step without storage write must leave index root unchanged",
                );
                (current_root.to_vec(), current_index_root.to_vec())
            }
        }
    } else {
        assert!(
            storage_witness.is_none(),
            "Only execution steps may carry storage witnesses",
        );
        (current_root.to_vec(), current_index_root.to_vec())
    }
}
