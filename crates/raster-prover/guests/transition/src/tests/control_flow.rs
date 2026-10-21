//! Tests of `checks::control_flow`: the transition guest's side of the
//! control-flow receipt. The control-flow check itself and its tests live in
//! the control-flow guest (`control-flow-prover` step 6). What is tested here
//! is that each journal field is held to this guest's own value: change any one
//! by a byte and the step is refused.

use super::*;

use crate::checks::control_flow::{assert_journal_matches, ControlFlowFacts};
use raster_core::control_flow::ControlFlowJournal;

/// One step's facts as the transition guest holds them, and the journal an
/// honest control-flow receipt commits for them.
struct Scenario {
    program_commitment: Vec<u8>,
    record: StepRecord,
    witness: FnInput,
    trace_root_before: Vec<u8>,
    expected_before: Vec<CfsCoordinates>,
    journal: ControlFlowJournal,
}

impl Scenario {
    fn honest() -> Self {
        let (witness, ..) = selecting_input_witness("arg", CfsCoordinates(vec![1]));
        let record = sequence_start_step(CfsCoordinates(vec![2]), input_source_commitment(&witness));
        let program_commitment = sha(b"program.bin");
        let trace_root_before = sha(b"trace prefix");
        let expected_before = vec![CfsCoordinates(vec![2])];
        let journal = ControlFlowJournal {
            program_commitment: program_commitment.clone(),
            step_record_hash: crate::merkle_tree::hash_trace_item(&record),
            input_source_commitment: Some(input_source_commitment(&witness)),
            trace_position: 7,
            trace_root_before: trace_root_before.clone(),
            expected_before: Some(expected_before.clone()),
            expected_after: vec![CfsCoordinates(vec![2, 1])],
        };
        Self {
            program_commitment,
            record,
            witness,
            trace_root_before,
            expected_before,
            journal,
        }
    }

    fn facts(&self) -> ControlFlowFacts<'_> {
        ControlFlowFacts {
            program_commitment: &self.program_commitment,
            step_record: &self.record,
            input_source_witness: Some(&self.witness),
            trace_position: 7,
            trace_root_before: &self.trace_root_before,
            expected_before: Some(&self.expected_before),
        }
    }

    fn check(&self) {
        assert_journal_matches(&self.journal, &self.facts());
    }
}

fn flip_first_byte(bytes: &mut [u8]) {
    bytes[0] ^= 1;
}

#[test]
fn an_honest_control_flow_journal_matches() {
    Scenario::honest().check();
}

#[test]
#[should_panic(expected = "ran on different programs")]
fn a_journal_for_another_program_is_refused() {
    let mut scenario = Scenario::honest();
    flip_first_byte(&mut scenario.journal.program_commitment);
    scenario.check();
}

#[test]
#[should_panic(expected = "ran on different step records")]
fn a_journal_for_another_step_record_is_refused() {
    let mut scenario = Scenario::honest();
    flip_first_byte(&mut scenario.journal.step_record_hash);
    scenario.check();
}

#[test]
#[should_panic(expected = "ran on different step arguments")]
fn a_journal_with_another_input_source_commitment_is_refused() {
    let mut scenario = Scenario::honest();
    flip_first_byte(scenario.journal.input_source_commitment.as_mut().unwrap());
    scenario.check();
}

/// The proposal's case: the journal is the honest one, but the transition
/// guest is handed a different input source witness — one argument name
/// changed, everything else the same.
#[test]
#[should_panic(expected = "ran on different step arguments")]
fn a_transition_witness_with_a_renamed_argument_is_refused() {
    let mut scenario = Scenario::honest();
    scenario.witness = renaming_args(scenario.witness.clone(), &["other"]);
    scenario.check();
}

/// A journal claiming no witness while the transition guest holds one, the
/// `Option` half of the same equality.
#[test]
#[should_panic(expected = "ran on different step arguments")]
fn a_journal_claiming_no_input_source_witness_is_refused() {
    let mut scenario = Scenario::honest();
    scenario.journal.input_source_commitment = None;
    scenario.check();
}

#[test]
#[should_panic(expected = "ran at a different trace position")]
fn a_journal_at_another_trace_position_is_refused() {
    let mut scenario = Scenario::honest();
    scenario.journal.trace_position += 1;
    scenario.check();
}

#[test]
#[should_panic(expected = "ran against a different trace prefix")]
fn a_journal_against_another_trace_root_is_refused() {
    let mut scenario = Scenario::honest();
    flip_first_byte(&mut scenario.journal.trace_root_before);
    scenario.check();
}

#[test]
#[should_panic(expected = "started from different expected coordinates")]
fn a_journal_from_other_expected_coordinates_is_refused() {
    let mut scenario = Scenario::honest();
    scenario.journal.expected_before = Some(vec![CfsCoordinates(vec![3])]);
    scenario.check();
}

/// A window's first step expects nothing (`None`); a journal claiming it was
/// checked against some expected set is a receipt for another position.
#[test]
#[should_panic(expected = "started from different expected coordinates")]
fn a_journal_claiming_expectations_at_a_window_start_is_refused() {
    let scenario = Scenario::honest();
    let facts = ControlFlowFacts {
        expected_before: None,
        ..scenario.facts()
    };
    assert_journal_matches(&scenario.journal, &facts);
}
