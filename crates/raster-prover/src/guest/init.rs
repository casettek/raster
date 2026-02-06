//! Init guest logic for creating the initial commitment.
//!
//! The Init guest creates the proven commitment and produces the first
//! `ReplayExpectation` for the Transition pipeline.
//!
//! This module provides the core logic that can be used both in std and no_std
//! environments (the guest crates handle the alloc setup).

use super::types::{InitInput, InitOutput, ReplayExpectation};

/// Execute the Init guest logic.
///
/// Creates the initial `ReplayExpectation` from the first trace item,
/// establishing the starting point for the Transition pipeline.
///
/// # Arguments
///
/// * `input` - The Init guest input containing fingerprint, trace, frontier, and first replay image ID
///
/// # Returns
///
/// The Init output containing the initial replay expectation.
///
/// # Panics
///
/// Panics if `input.trace` is empty.
pub fn execute(input: InitInput) -> InitOutput {
    assert!(!input.trace.is_empty(), "trace must not be empty");

    // Create the initial replay expectation for Transition₁
    let initial_replay_expectation = ReplayExpectation {
        image_id: input.first_replay_image_id,
        trace_item: input.trace[0].clone(),
        frontier: input.frontier,
    };

    InitOutput {
        initial_replay_expectation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::types::Fingerprint;
    use crate::trace::SerializableFrontier;
    use raster_core::trace::TraceItem;

    fn make_test_trace_item() -> TraceItem {
        TraceItem {
            fn_name: "test_tile".into(),
            desc: None,
            inputs: vec![],
            input_data: "dGVzdA==".into(), // base64 "test"
            output_type: Some("u64".into()),
            output_data: "MTIz".into(), // base64 "123"
        }
    }

    fn make_test_frontier() -> SerializableFrontier {
        SerializableFrontier {
            position: 0,
            leaf: vec![0u8; 32],
            ommers: vec![],
        }
    }

    fn make_test_fingerprint() -> Fingerprint {
        Fingerprint {
            bytes: vec![0x1234567890abcdef],
            bits_per_item: 8,
            inclusion_proof: [0u8; 32],
        }
    }

    #[test]
    fn test_execute_produces_valid_output() {
        let first_replay_image_id = [1u8; 32];
        let trace_item = make_test_trace_item();
        let frontier = make_test_frontier();

        let input = InitInput {
            fingerprint: make_test_fingerprint(),
            trace: vec![trace_item.clone()],
            frontier: frontier.clone(),
            first_replay_image_id,
        };

        let output = execute(input);

        // Verify replay expectation matches input
        assert_eq!(
            output.initial_replay_expectation.image_id,
            first_replay_image_id
        );
        assert_eq!(
            output.initial_replay_expectation.trace_item.fn_name,
            trace_item.fn_name
        );
        assert_eq!(
            output.initial_replay_expectation.frontier.position,
            frontier.position
        );
    }

    #[test]
    fn test_execute_uses_first_trace_item() {
        let mut trace_item1 = make_test_trace_item();
        trace_item1.fn_name = "first_tile".into();
        let mut trace_item2 = make_test_trace_item();
        trace_item2.fn_name = "second_tile".into();

        let input = InitInput {
            fingerprint: make_test_fingerprint(),
            trace: vec![trace_item1.clone(), trace_item2],
            frontier: make_test_frontier(),
            first_replay_image_id: [1u8; 32],
        };

        let output = execute(input);

        // Should use the first trace item
        assert_eq!(
            output.initial_replay_expectation.trace_item.fn_name,
            "first_tile"
        );
    }

    #[test]
    #[should_panic(expected = "trace must not be empty")]
    fn test_execute_panics_on_empty_trace() {
        let input = InitInput {
            fingerprint: make_test_fingerprint(),
            trace: vec![],
            frontier: make_test_frontier(),
            first_replay_image_id: [1u8; 32],
        };

        execute(input);
    }
}
