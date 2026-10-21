//! Tests of `checks::control_flow`: the transition guest's side of the
//! control-flow receipt. The control-flow check itself and its tests live in
//! the control-flow guest (`control-flow-prover` step 6). What is tested here
//! is that a step's entry must be the one its segment committed, at its own
//! position, and that each entry field is held to this guest's own value:
//! change any one by a byte and the step is refused.

use super::*;

use crate::checks::control_flow::{assert_evidence_matches, ControlFlowFacts};
use raster_core::control_flow::{
    entries_root, entry_path, ControlFlowEntry, ControlFlowEvidence, ControlFlowJournal,
};

/// One step's facts as the transition guest holds them, the segment's entries
/// (the step's own at `own`), and the evidence an honest host hands the step.
struct Scenario {
    program_commitment: Vec<u8>,
    record: StepRecord,
    witness: FnInput,
    trace_root_before: Vec<u8>,
    expected_before: Vec<CfsCoordinates>,
    entries: Vec<ControlFlowEntry>,
    own: usize,
    evidence: ControlFlowEvidence,
}

/// The step under test sits at trace position 7.
const POSITION: u64 = 7;

impl Scenario {
    /// A segment of `len` steps starting at `first`, with this step among them.
    fn in_segment(first: u64, len: u64) -> Self {
        assert!((first..first + len).contains(&POSITION));
        let (witness, ..) = selecting_input_witness("arg", CfsCoordinates(vec![1]));
        let record = sequence_start_step(CfsCoordinates(vec![2]), input_source_commitment(&witness));
        let program_commitment = sha(b"program.bin");
        let trace_root_before = sha(b"trace prefix");
        let expected_before = vec![CfsCoordinates(vec![2])];
        let entries = (first..first + len)
            .map(|position| {
                if position == POSITION {
                    ControlFlowEntry {
                        step_record_hash: crate::merkle_tree::hash_trace_item(&record),
                        input_source_commitment: Some(input_source_commitment(&witness)),
                        trace_position: POSITION,
                        trace_root_before: trace_root_before.clone(),
                        expected_before: Some(expected_before.clone()),
                        expected_after: vec![CfsCoordinates(vec![2, 1])],
                    }
                } else {
                    ControlFlowEntry {
                        step_record_hash: sha(&position.to_le_bytes()),
                        input_source_commitment: None,
                        trace_position: position,
                        trace_root_before: sha(&[position as u8, 0xFF]),
                        expected_before: None,
                        expected_after: Vec::new(),
                    }
                }
            })
            .collect::<Vec<_>>();
        let own = (POSITION - first) as usize;
        let mut scenario = Self {
            program_commitment,
            record,
            witness,
            trace_root_before,
            expected_before,
            entries,
            own,
            evidence: ControlFlowEvidence {
                journal: ControlFlowJournal {
                    program_commitment: Vec::new(),
                    first_position: first,
                    entry_count: len,
                    entries_root: [0; 32],
                },
                entry: ControlFlowEntry {
                    step_record_hash: Vec::new(),
                    input_source_commitment: None,
                    trace_position: 0,
                    trace_root_before: Vec::new(),
                    expected_before: None,
                    expected_after: Vec::new(),
                },
                path: Vec::new(),
            },
        };
        scenario.commit();
        scenario
    }

    /// A one-step segment: step 6's per-step mode.
    fn honest() -> Self {
        Self::in_segment(POSITION, 1)
    }

    /// What the control-flow guest would commit for `entries`, and the evidence
    /// for this step. Call after changing an entry to model a segment that
    /// really committed the change.
    fn commit(&mut self) {
        let leaves: Vec<_> = self.entries.iter().map(ControlFlowEntry::leaf_hash).collect();
        self.evidence = ControlFlowEvidence {
            journal: ControlFlowJournal {
                program_commitment: self.program_commitment.clone(),
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
    fn commit_own(&mut self, change: impl FnOnce(&mut ControlFlowEntry)) {
        change(&mut self.entries[self.own]);
        self.commit();
    }

    fn facts(&self) -> ControlFlowFacts<'_> {
        ControlFlowFacts {
            program_commitment: &self.program_commitment,
            step_record: &self.record,
            input_source_witness: Some(&self.witness),
            trace_position: POSITION,
            trace_root_before: &self.trace_root_before,
            expected_before: Some(&self.expected_before),
        }
    }

    fn check(&self) {
        assert_evidence_matches(&self.evidence, &self.facts());
    }
}

fn flip_first_byte(bytes: &mut [u8]) {
    bytes[0] ^= 1;
}

#[test]
fn an_honest_one_step_segment_matches() {
    Scenario::honest().check();
}

/// A whole-window segment: the step's entry and path, at any position in it,
/// fold to the committed root.
#[test]
fn an_honest_entry_in_a_longer_segment_matches() {
    for (first, len) in [(0, 32), (7, 8), (4, 5), (6, 3)] {
        Scenario::in_segment(first, len).check();
    }
}

#[test]
#[should_panic(expected = "ran on different programs")]
fn a_journal_for_another_program_is_refused() {
    let mut scenario = Scenario::honest();
    flip_first_byte(&mut scenario.evidence.journal.program_commitment);
    scenario.check();
}

#[test]
#[should_panic(expected = "does not cover trace position 7")]
fn a_segment_not_covering_the_step_is_refused() {
    let mut scenario = Scenario::in_segment(0, 32);
    scenario.evidence.journal.first_position = 8;
    scenario.check();
}

/// The host hands the step an entry the segment never committed — here one
/// with a different trace root, which the field check would also catch, but
/// the path check refuses first.
#[test]
#[should_panic(expected = "is not the one the segment committed")]
fn an_entry_the_segment_did_not_commit_is_refused() {
    let mut scenario = Scenario::in_segment(0, 32);
    flip_first_byte(&mut scenario.evidence.entry.trace_root_before);
    scenario.check();
}

/// Another step's real entry and path, from the same segment, presented for
/// this step: its path is for another index, so it doesn't fold here.
#[test]
#[should_panic(expected = "is not the one the segment committed")]
fn another_steps_entry_from_the_same_segment_is_refused() {
    let mut scenario = Scenario::in_segment(0, 32);
    let leaves: Vec<_> = scenario.entries.iter().map(ControlFlowEntry::leaf_hash).collect();
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

/// A segment that ends before this step does not cover it. (Changing a header
/// field after the fact is caught by the receipt itself — `env::verify` hashes
/// the encoded journal — which these native tests do not run; what is tested
/// here is the step's own reading of an honest header.)
#[test]
#[should_panic(expected = "does not cover trace position 7")]
fn a_segment_ending_before_the_step_is_refused() {
    let mut scenario = Scenario::in_segment(0, 32);
    scenario.entries.truncate(7);
    scenario.own = 6;
    scenario.commit();
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

/// The proposal's case: the entry is the honest one, but the transition guest
/// is handed a different input source witness — one argument name changed,
/// everything else the same.
#[test]
#[should_panic(expected = "ran on different step arguments")]
fn a_transition_witness_with_a_renamed_argument_is_refused() {
    let mut scenario = Scenario::honest();
    scenario.witness = renaming_args(scenario.witness.clone(), &["other"]);
    scenario.check();
}

/// An entry claiming no witness while the transition guest holds one, the
/// `Option` half of the same equality.
#[test]
#[should_panic(expected = "ran on different step arguments")]
fn a_committed_entry_claiming_no_input_source_witness_is_refused() {
    let mut scenario = Scenario::honest();
    scenario.commit_own(|entry| entry.input_source_commitment = None);
    scenario.check();
}

/// An entry the segment committed at this index, but whose own position field
/// disagrees — only a forged segment could hold one.
#[test]
#[should_panic(expected = "ran at a different trace position")]
fn a_committed_entry_naming_another_position_is_refused() {
    let mut scenario = Scenario::in_segment(6, 3);
    scenario.commit_own(|entry| entry.trace_position += 1);
    scenario.check();
}

#[test]
#[should_panic(expected = "ran against a different trace prefix")]
fn a_committed_entry_against_another_trace_root_is_refused() {
    let mut scenario = Scenario::in_segment(0, 32);
    scenario.commit_own(|entry| flip_first_byte(&mut entry.trace_root_before));
    scenario.check();
}

#[test]
#[should_panic(expected = "started from different expected coordinates")]
fn a_committed_entry_from_other_expected_coordinates_is_refused() {
    let mut scenario = Scenario::honest();
    scenario.commit_own(|entry| entry.expected_before = Some(vec![CfsCoordinates(vec![3])]));
    scenario.check();
}

/// A window's first step expects nothing (`None`); an entry claiming it was
/// checked against some expected set is evidence for another position.
#[test]
#[should_panic(expected = "started from different expected coordinates")]
fn an_entry_claiming_expectations_at_a_window_start_is_refused() {
    let scenario = Scenario::honest();
    let facts = ControlFlowFacts {
        expected_before: None,
        ..scenario.facts()
    };
    assert_evidence_matches(&scenario.evidence, &facts);
}
