//! Tests of `checks::recur`: chunking and the recur-progress stack.

use super::*;

/// The case the change exists for: a window opening at iteration 1 of a live
/// sweep verifies, seeded from the prefix, at unchanged window size.
#[test]
fn a_seeded_mid_loop_window_verifies() {
    let mut seed = seeded_stack(0);
    // What the recorder stamped on the step this window opens with.
    let recorded = {
        let mut expected = seeded_stack(0);
        expected
            .advance_tile_iteration(
                &CfsCoordinates(vec![1, 2]),
                1,
                3,
                2,
                RecurControlKind::Continue,
                None,
            )
            .expect("honest advance");
        expected.commitment()
    };

    advance_seeded(&mut seed, 1, recorded);
}

/// The failure this replaces: with no seed the empty stack is advanced, which
/// is the claim "no loop in flight" — and iteration 1 contradicts it.
#[test]
#[should_panic(expected = "recur iteration has no active recur site")]
fn an_unseeded_mid_loop_window_is_rejected() {
    let mut empty = RecurProgressStack::new();
    advance_seeded(&mut empty, 1, RecurProgressStack::new().commitment());
}

/// Reconstruction does not weaken the check: a seed claiming a different
/// position advances to a different stack and fails the comparison.
#[test]
#[should_panic(expected = "Recur progress commitment does not match")]
fn a_forged_seed_is_rejected() {
    // The honest record for a window opening after iteration 0...
    let recorded = {
        let mut expected = seeded_stack(0);
        expected
            .advance_tile_iteration(
                &CfsCoordinates(vec![1, 2]),
                1,
                3,
                2,
                RecurControlKind::Continue,
                None,
            )
            .expect("honest advance");
        expected.commitment()
    };

    // ...against a seed claiming iteration 1 already happened. It advances to
    // a different stack, so the recorded commitment does not reproduce.
    let mut forged = seeded_stack(1);
    let cfs_cursor = chunked_recur_cfs(Some(2));
    let journal = recur_journal(2, 3, 2, RecurControlKind::Continue);
    let (input, witnesses) = sweep_item_witness(4, 2);
    let mut step = recur_iteration_step(2);
    step.recur_progress_commitment = recorded;
    crate::checks::recur::advance_recur_progress(
        &cfs_cursor,
        &mut forged,
        &step,
        Some(&journal),
        None,
        Some(&input),
        None,
        &witnesses,
        None,
        &[],
        &[],
    );
}

/// Rule 8 through the guest's own entry point: an iteration whose journal
/// and commitment are honest but whose item re-reads chunk 0 is rejected
/// before the commitment is even compared.
#[test]
#[should_panic(expected = "starts at element 0 but the sweep has reached 2")]
fn a_seeded_window_that_rereads_the_first_chunk_is_rejected() {
    let mut seed = seeded_stack(0);
    let cfs_cursor = chunked_recur_cfs(Some(2));
    let journal = recur_journal(1, 3, 2, RecurControlKind::Continue);
    let (input, witnesses) = sweep_item_witness(0, 2);
    crate::checks::recur::advance_recur_progress(
        &cfs_cursor,
        &mut seed,
        &recur_iteration_step(1),
        Some(&journal),
        None,
        Some(&input),
        None,
        &witnesses,
        None,
        &[],
        &[],
    );
}

/// And an iteration with no item at all — the shape every recur iteration had
/// to the guest before rule 8, since iterations skip the CFS input check.
#[test]
#[should_panic(expected = "records no input witness for its item")]
fn a_recur_iteration_without_an_item_is_rejected() {
    let mut seed = seeded_stack(0);
    let cfs_cursor = chunked_recur_cfs(Some(2));
    let journal = recur_journal(1, 3, 2, RecurControlKind::Continue);
    crate::checks::recur::advance_recur_progress(
        &cfs_cursor,
        &mut seed,
        &recur_iteration_step(1),
        Some(&journal),
        None,
        None,
        None,
        &BTreeMap::new(),
        None,
        &[],
        &[],
    );
}

#[test]
fn verify_recur_iteration_chunking_accepts_declared_chunk_sizes() {
    let cfs_cursor = chunked_recur_cfs(Some(2));
    // A full chunk mid-sweep, and a short chunk on the final iteration.
    for (iteration, declared_iterations, consumed) in [(0u64, 3u64, 2u64), (2, 3, 1)] {
        let journal = recur_journal(
            iteration,
            declared_iterations,
            consumed,
            RecurControlKind::Continue,
        );
        verify_recur_iteration_chunking(
            &cfs_cursor,
            &recur_iteration_step(iteration as CfsCoordinate),
            Some(&journal),
        );
    }
}

#[test]
fn verify_recur_iteration_chunking_ignores_chunking_when_not_declared() {
    let cfs_cursor = chunked_recur_cfs(None);
    // Without a declared chunk an iteration step carries no chunk obligation.
    verify_recur_iteration_chunking(&cfs_cursor, &recur_iteration_step(0), None);
}

#[test]
#[should_panic(expected = "exceeds declared chunk size")]
fn verify_recur_iteration_chunking_rejects_oversized_chunk() {
    let cfs_cursor = chunked_recur_cfs(Some(2));
    let journal = recur_journal(0, 3, 3, RecurControlKind::Continue);
    verify_recur_iteration_chunking(&cfs_cursor, &recur_iteration_step(0), Some(&journal));
}

#[test]
#[should_panic(expected = "empty chunk")]
fn verify_recur_iteration_chunking_rejects_empty_chunk() {
    let cfs_cursor = chunked_recur_cfs(Some(2));
    let journal = recur_journal(0, 3, 0, RecurControlKind::Continue);
    verify_recur_iteration_chunking(&cfs_cursor, &recur_iteration_step(0), Some(&journal));
}

/// Only the final chunk may be short — the `4,1,4,1` shape, rejected on the
/// iteration that actually goes short.
///
/// The native recorder used to enforce this by remembering the previous
/// iteration's length. `declared_iterations` makes it a stateless fact of the
/// iteration itself, so the guest can check it without carrying state across
/// steps.
#[test]
#[should_panic(expected = "smaller than declared chunk size")]
fn verify_recur_iteration_chunking_rejects_short_non_final_chunk() {
    let cfs_cursor = chunked_recur_cfs(Some(2));
    let journal = recur_journal(0, 3, 1, RecurControlKind::Continue);
    verify_recur_iteration_chunking(&cfs_cursor, &recur_iteration_step(0), Some(&journal));
}

#[test]
#[should_panic(expected = "missing its replay-proven recur facts")]
fn verify_recur_iteration_chunking_requires_recur_facts_for_declared_chunk() {
    let cfs_cursor = chunked_recur_cfs(Some(2));
    verify_recur_iteration_chunking(&cfs_cursor, &recur_iteration_step(0), None);
}

#[test]
fn an_honest_recur_sequence_sweep_satisfies_rule_8() {
    let mut progress = lines_sweep();
    for index in 0..2 {
        let (step, input, witnesses) = recur_sequence_iteration(index);
        advance_lines_iteration(&mut progress, index, &input, &witnesses, step);
    }
}

#[test]
#[should_panic(expected = "starts at element 0 but the sweep has reached 1")]
fn a_recur_sequence_iteration_rereading_an_element_is_rejected() {
    let mut progress = lines_sweep();
    let (step, input, witnesses) = recur_sequence_iteration(0);
    advance_lines_iteration(&mut progress, 0, &input, &witnesses, step);

    // Iteration 1 hands the body element 0 again.
    let (_, input, witnesses) = recur_sequence_iteration(0);
    let (step, _, _) = recur_sequence_iteration(1);
    advance_lines_iteration(&mut progress, 1, &input, &witnesses, step);
}

#[test]
#[should_panic(expected = "item selection witness does not fold to its commitment")]
fn a_recur_sequence_item_whose_proof_does_not_fold_is_rejected() {
    let mut progress = lines_sweep();
    let (step, input, mut witnesses) = recur_sequence_iteration(0);
    witnesses.get_mut("line").unwrap().selected_root = Some([5; 32]);
    advance_lines_iteration(&mut progress, 0, &input, &witnesses, step);
}

#[test]
#[should_panic(expected = "not selected out of its site's source list")]
fn a_recur_sequence_item_from_another_object_is_rejected() {
    let mut progress = lines_sweep();
    let (step, mut input, witnesses) = recur_sequence_iteration(0);
    input.storage.get_mut("line").unwrap().coordinates = CfsCoordinates(vec![3]);
    advance_lines_iteration(&mut progress, 0, &input, &witnesses, step);
}

#[test]
fn an_iteration_returning_its_body_tiles_state_is_accepted() {
    let state = [6u8; 32];
    check_returned_state(
        &iteration_end([1u8; 32], state),
        returned(vec![1, 1, 1], state),
        returned(vec![1, 1, 1], state),
        true,
    );
}

/// An object the body wrote somewhere else, or did not write as its return.
#[test]
#[should_panic(expected = "returned a state its body did not produce")]
fn an_iteration_returning_another_object_is_rejected() {
    let state = [6u8; 32];
    check_returned_state(
        &iteration_end([1u8; 32], state),
        returned(vec![1, 1, 2], state),
        returned(vec![1, 1, 2], state),
        true,
    );
}

/// `{6}` proven, `{7}` claimed: the chain's `state_out` must be the object
/// the body returned.
#[test]
#[should_panic(expected = "claims a state_out that is not its returned object")]
fn an_iteration_claiming_another_state_out_is_rejected() {
    let state = [6u8; 32];
    check_returned_state(
        &iteration_end([1u8; 32], [7u8; 32]),
        returned(vec![1, 1, 1], state),
        returned(vec![1, 1, 1], state),
        true,
    );
}

/// The returned object is read against the current storage state: a claim
/// no store holds has no read witness to verify.
#[test]
#[should_panic(expected = "missing the read witness of its returned state")]
fn an_iteration_without_a_read_of_its_returned_state_is_rejected() {
    let state = [6u8; 32];
    check_returned_state(
        &iteration_end([1u8; 32], state),
        returned(vec![1, 1, 1], state),
        returned(vec![1, 1, 1], state),
        false,
    );
}

#[test]
fn an_iteration_reading_the_chains_state_is_accepted() {
    let (step, input) = iteration_start_with_state(FnInputValue::StorageBinding, [5u8; 32]);
    crate::checks::recur::check_iteration_state_binding(&step, &chain_at([5u8; 32]), Some(&input));
}

#[test]
#[should_panic(expected = "reads a state that is not the one its sweep reached")]
fn an_iteration_reading_another_state_is_rejected() {
    let (step, input) = iteration_start_with_state(FnInputValue::StorageBinding, [4u8; 32]);
    crate::checks::recur::check_iteration_state_binding(&step, &chain_at([5u8; 32]), Some(&input));
}

#[test]
#[should_panic(expected = "carries its state inline after the chain started")]
fn an_iteration_carrying_state_inline_after_the_chain_started_is_rejected() {
    let (step, input) = iteration_start_with_state(FnInputValue::Inline(vec![1, 2]), [5u8; 32]);
    crate::checks::recur::check_iteration_state_binding(&step, &chain_at([5u8; 32]), Some(&input));
}
