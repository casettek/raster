//! Tests of `checks::store`: the transition guest's side of the storage
//! receipt. The storage check itself and its tests live in the storage guest
//! (`storage-prover` step 4). What is tested here is that a step's entry must be
//! the one its segment committed, at its own position, and that each entry
//! field is held to this guest's own value: change any one by a byte and the
//! step is refused.

use super::*;

use crate::checks::store::{assert_evidence_matches, StorageFacts};
use raster_core::storage_check::{
    entries_root, entry_path, selection_witnesses_commitment, StorageEvidence, StorageJournal,
    StorageState, StorageStepEntry,
};

/// One step's facts as the transition guest holds them, the segment's entries
/// (the step's own at `own`), and the evidence an honest host hands the step.
struct Scenario {
    record: StepRecord,
    witness: FnInput,
    selections: BTreeMap<String, SelectionWitness>,
    before: StorageState,
    entries: Vec<StorageStepEntry>,
    own: usize,
    evidence: StorageEvidence,
}

/// The step under test sits at trace position 7.
const POSITION: u64 = 7;

fn state(seed: u8) -> StorageState {
    StorageState {
        position: u64::from(seed),
        root: sha(&[seed, 0x01]),
        index_root: sha(&[seed, 0x02]),
    }
}

impl Scenario {
    /// A segment of `len` steps starting at `first`, with this step among them.
    fn in_segment(first: u64, len: u64) -> Self {
        assert!((first..first + len).contains(&POSITION));
        let (witness, _root, payload, _reference) =
            selecting_input_witness("arg", CfsCoordinates(vec![1]));
        let record = sequence_start_step(CfsCoordinates(vec![2]), input_source_commitment(&witness));
        let selections: BTreeMap<String, SelectionWitness> =
            [("arg".to_string(), payload)].into_iter().collect();
        let before = state(3);
        let entries = (first..first + len)
            .map(|position| {
                if position == POSITION {
                    StorageStepEntry {
                        step_record_hash: crate::merkle_tree::hash_trace_item(&record),
                        input_source_commitment: Some(input_source_commitment(&witness)),
                        selection_witnesses_commitment: selection_witnesses_commitment(&selections),
                        trace_position: POSITION,
                        before: before.clone(),
                        after: state(4),
                    }
                } else {
                    StorageStepEntry {
                        step_record_hash: sha(&position.to_le_bytes()),
                        input_source_commitment: None,
                        selection_witnesses_commitment: [position as u8; 32],
                        trace_position: position,
                        before: state(position as u8 + 0x40),
                        after: state(position as u8 + 0x41),
                    }
                }
            })
            .collect::<Vec<_>>();
        let own = (POSITION - first) as usize;
        let leaves: Vec<_> = entries.iter().map(StorageStepEntry::leaf_hash).collect();
        let evidence = StorageEvidence {
            journal: StorageJournal {
                first_position: first,
                entry_count: len,
                entries_root: entries_root(&leaves),
            },
            entry: entries[own].clone(),
            path: entry_path(&leaves, own),
        };
        Self {
            record,
            witness,
            selections,
            before,
            entries,
            own,
            evidence,
        }
    }

    /// A one-step segment: the per-step mode.
    fn honest() -> Self {
        Self::in_segment(POSITION, 1)
    }

    /// What the storage guest would commit for `entries`, and the evidence for
    /// this step. Call after changing an entry to model a segment that really
    /// committed the change.
    fn commit(&mut self) {
        let leaves: Vec<_> = self.entries.iter().map(StorageStepEntry::leaf_hash).collect();
        self.evidence = StorageEvidence {
            journal: StorageJournal {
                first_position: self.entries[0].trace_position,
                entry_count: self.entries.len() as u64,
                entries_root: entries_root(&leaves),
            },
            entry: self.entries[self.own].clone(),
            path: entry_path(&leaves, self.own),
        };
    }

    /// Change this step's entry and have the segment commit the change, so the
    /// path still folds and only the field equality can catch it.
    fn commit_own(&mut self, change: impl FnOnce(&mut StorageStepEntry)) {
        change(&mut self.entries[self.own]);
        self.commit();
    }

    fn facts(&self) -> StorageFacts<'_> {
        StorageFacts {
            step_record: &self.record,
            input_source_witness: Some(&self.witness),
            storage_selection_witnesses: &self.selections,
            trace_position: POSITION,
            before: &self.before,
        }
    }

    fn check(&self) -> StorageState {
        assert_evidence_matches(&self.evidence, &self.facts()).after.clone()
    }
}

fn flip_first_byte(bytes: &mut [u8]) {
    bytes[0] ^= 1;
}

#[test]
fn an_honest_one_step_segment_matches_and_hands_back_the_store_after() {
    assert_eq!(Scenario::honest().check(), state(4));
}

/// A whole-window segment: the step's entry and path, at any position in it,
/// fold to the committed root.
#[test]
fn an_honest_entry_in_a_longer_segment_matches() {
    for (first, len) in [(0, 32), (7, 8), (4, 5), (6, 3)] {
        assert_eq!(Scenario::in_segment(first, len).check(), state(4));
    }
}

#[test]
#[should_panic(expected = "does not cover trace position 7")]
fn a_segment_not_covering_the_step_is_refused() {
    let mut scenario = Scenario::in_segment(0, 32);
    scenario.evidence.journal.first_position = 8;
    scenario.check();
}

#[test]
#[should_panic(expected = "does not cover trace position 7")]
fn a_segment_ending_before_the_step_is_refused() {
    let mut scenario = Scenario::in_segment(0, 32);
    scenario.entries.truncate(7);
    scenario.own = 6;
    scenario.commit();
    scenario.check();
}

/// The host hands the step an entry the segment never committed — here one
/// claiming another store after, which nothing else would catch: `after` is
/// adopted, not equated.
#[test]
#[should_panic(expected = "is not the one the segment committed")]
fn an_entry_the_segment_did_not_commit_is_refused() {
    let mut scenario = Scenario::in_segment(0, 32);
    flip_first_byte(&mut scenario.evidence.entry.after.root);
    scenario.check();
}

/// Another step's real entry and path, from the same segment, presented for
/// this step: its path is for another index, so it doesn't fold here.
#[test]
#[should_panic(expected = "is not the one the segment committed")]
fn another_steps_entry_from_the_same_segment_is_refused() {
    let mut scenario = Scenario::in_segment(0, 32);
    let leaves: Vec<_> = scenario.entries.iter().map(StorageStepEntry::leaf_hash).collect();
    scenario.evidence.entry = scenario.entries[8].clone();
    scenario.evidence.path = entry_path(&leaves, 8);
    scenario.check();
}

#[test]
#[should_panic(expected = "is not the one the segment committed")]
fn a_tampered_path_is_refused() {
    let mut scenario = Scenario::in_segment(0, 32);
    flip_first_byte(&mut scenario.evidence.path[2]);
    scenario.check();
}

/// A control-flow entry's tree is not the storage tree: the same entries
/// committed under the control-flow domain do not fold here.
#[test]
#[should_panic(expected = "is not the one the segment committed")]
fn a_root_from_the_control_flow_tree_is_refused() {
    let mut scenario = Scenario::in_segment(4, 5);
    let leaves: Vec<_> = scenario.entries.iter().map(StorageStepEntry::leaf_hash).collect();
    scenario.evidence.journal.entries_root = raster_core::control_flow::entries_root(&leaves);
    scenario.evidence.path = raster_core::control_flow::entry_path(&leaves, scenario.own);
    scenario.check();
}

#[test]
#[should_panic(expected = "ran on different step records")]
fn a_committed_entry_for_another_step_record_is_refused() {
    let mut scenario = Scenario::in_segment(4, 5);
    scenario.commit_own(|entry| flip_first_byte(&mut entry.step_record_hash));
    scenario.check();
}

#[test]
#[should_panic(expected = "ran on different step arguments")]
fn a_committed_entry_with_another_input_source_commitment_is_refused() {
    let mut scenario = Scenario::honest();
    scenario.commit_own(|entry| flip_first_byte(entry.input_source_commitment.as_mut().unwrap()));
    scenario.check();
}

#[test]
#[should_panic(expected = "ran on different step arguments")]
fn a_transition_witness_with_a_renamed_argument_is_refused() {
    let mut scenario = Scenario::honest();
    scenario.witness = renaming_args(scenario.witness.clone(), &["other"]);
    scenario.check();
}

#[test]
#[should_panic(expected = "ran on different selection witnesses")]
fn a_committed_entry_with_another_selection_commitment_is_refused() {
    let mut scenario = Scenario::in_segment(0, 32);
    scenario.commit_own(|entry| flip_first_byte(&mut entry.selection_witnesses_commitment));
    scenario.check();
}

/// The proposal's case: the entry is the honest one, so the storage guest
/// folded the honest witnesses — but the transition guest, where recur and io
/// read them, is handed a different map. One payload byte changed.
#[test]
#[should_panic(expected = "ran on different selection witnesses")]
fn a_transition_selection_witness_with_a_changed_payload_is_refused() {
    let mut scenario = Scenario::honest();
    scenario.selections.get_mut("arg").unwrap().bytes[0] ^= 1;
    scenario.check();
}

/// An extra binding nobody reads still changes the map, so the transition
/// guest can't be handed witnesses the storage guest never saw.
#[test]
#[should_panic(expected = "ran on different selection witnesses")]
fn a_transition_selection_map_with_an_extra_witness_is_refused() {
    let mut scenario = Scenario::honest();
    let extra = scenario.selections["arg"].clone();
    scenario.selections.insert("unread".to_string(), extra);
    scenario.check();
}

#[test]
#[should_panic(expected = "ran at a different trace position")]
fn a_committed_entry_naming_another_position_is_refused() {
    let mut scenario = Scenario::in_segment(6, 3);
    scenario.commit_own(|entry| entry.trace_position += 1);
    scenario.check();
}

#[test]
#[should_panic(expected = "started from a different store")]
fn a_committed_entry_from_another_append_log_root_is_refused() {
    let mut scenario = Scenario::in_segment(0, 32);
    scenario.commit_own(|entry| flip_first_byte(&mut entry.before.root));
    scenario.check();
}

#[test]
#[should_panic(expected = "started from a different store")]
fn a_committed_entry_from_another_index_root_is_refused() {
    let mut scenario = Scenario::honest();
    scenario.commit_own(|entry| flip_first_byte(&mut entry.before.index_root));
    scenario.check();
}

/// Same roots, another log position: the frontier that root names is a
/// different one (`a_start_frontier_with_the_right_root_at_the_wrong_position`
/// in the storage guest), so the position is equated too.
#[test]
#[should_panic(expected = "started from a different store")]
fn a_committed_entry_from_another_log_position_is_refused() {
    let mut scenario = Scenario::honest();
    scenario.commit_own(|entry| entry.before.position += 1);
    scenario.check();
}
