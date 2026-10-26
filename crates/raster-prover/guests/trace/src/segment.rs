//! The trace over a segment of steps: the window binding, the append of each
//! step's record hash, and the verdict at every step.

use raster_core::fingerprint::fingerprint_value;
use raster_core::fraud_window::{
    trace_state_digest, Chain, ChainKind, PartKind, PartOutput, PartSegmentJournal, Span,
    TraceOutput,
};
use raster_core::input::Hash32;
use raster_core::trace_check::{TraceSegmentHeader, TraceStepEntry};

use guest_common::merkle_tree::{deserialize_frontier, frontier_root, Bytes};

use crate::window::{assert_window_is_commitment_slice, WindowBinding};

/// Bind the window, then append and judge every step of the segment. Returns
/// the binding and one entry per step.
///
/// The verdict needs no state carried from earlier steps: at window index `i`
/// it compares this step's root, packed to the fingerprint's bits per item,
/// with the committed item `i`. Every index before the window's last must
/// match; the last must diverge, by its packed bits or, inside the revealed
/// tail, by its full root. A segment that breaks the rule at any step is
/// refused, so a receipt covering an entry also says the verdict held there.
pub fn verify_segment(
    header: TraceSegmentHeader,
    mut next_step_hash: impl FnMut() -> Vec<u8>,
) -> (WindowBinding, Vec<TraceStepEntry>) {
    assert!(header.step_count > 0, "a trace segment must cover at least one step");
    let init = &header.init;
    let binding = assert_window_is_commitment_slice(
        init,
        &header.commitment_header,
        &header.slice_witness,
        header.revealed_tail_roots.as_ref(),
    );

    let committed = &init.fingerprint;
    let window_start = init.init_frontier.position;
    let window_len = committed.len() as u64;
    let first_position = header.start_frontier.position;
    assert!(
        first_position >= window_start
            && first_position + header.step_count <= window_start + window_len,
        "trace segment [{first_position}, +{}) is outside the window [{window_start}, +{window_len})",
        header.step_count,
    );
    if first_position == window_start {
        assert!(
            header.start_frontier == init.init_frontier,
            "a segment opening the window must start from the window's initial frontier",
        );
    }

    let mut frontier = deserialize_frontier(&header.start_frontier)
        .expect("host must supply a valid trace frontier");
    let mut root = frontier_root(&frontier);
    let mut entries = Vec::with_capacity(header.step_count as usize);
    for offset in 0..header.step_count {
        let step_record_hash = next_step_hash();
        let trace_position = first_position + offset;
        let root_before = root;
        frontier.append(Bytes(step_record_hash.clone()));
        root = frontier_root(&frontier);

        let index = (trace_position - window_start) as usize;
        let committed_value = committed
            .bits_packer
            .get(index, &committed.bits)
            .expect("the window fingerprint covers every window index");
        let diverges = fingerprint_value(&root, committed.bits_per_item()) != committed_value;
        if index as u64 + 1 == window_len {
            // The fingerprint keeps only `bits_per_item` bits of each root, so
            // inside the revealed tail the full root settles a divergence the
            // packed bits can't see. Root equality implies bit equality, so
            // this only widens what is provable, never what matches.
            let diverges_by_root = binding
                .final_committed_root
                .as_ref()
                .is_some_and(|committed_root| *committed_root != root);
            assert!(
                diverges || diverges_by_root,
                "A fraud proof's final step must diverge from the commitment, by fingerprint \
                 entry or by revealed trace root",
            );
        } else {
            assert!(
                !diverges,
                "Every window item before the last must match the commitment; a divergence \
                 here is not the one this window claims to prove",
            );
        }
        entries.push(TraceStepEntry {
            step_record_hash,
            trace_position,
            trace_root_before: root_before,
            trace_root_after: root.clone(),
        });
    }
    (binding, entries)
}

/// What the segment commits: the record chain over its record hashes, the
/// trace state (the frontier, bound by position and root) before and after,
/// and the window binding as its output.
pub fn segment_journal(
    window: Hash32,
    records_before: Hash32,
    binding: WindowBinding,
    entries: &[TraceStepEntry],
) -> PartSegmentJournal {
    let mut records = Chain::resume(ChainKind::Records, records_before);
    for entry in entries {
        records.fold(&entry.step_record_hash);
    }
    let first = entries.first().expect("a segment covers at least one step");
    let last = entries.last().expect("a segment covers at least one step");
    PartSegmentJournal {
        part: PartKind::Trace,
        window_id: window,
        program_commitment: None,
        first_position: first.trace_position,
        step_count: entries.len() as u64,
        records: Span {
            before: records_before,
            after: records.value(),
        },
        state: Span {
            before: trace_state_digest(first.trace_position, &first.trace_root_before),
            after: trace_state_digest(last.trace_position + 1, &last.trace_root_after),
        },
        columns: Vec::new(),
        out: Some(PartOutput::Trace(TraceOutput {
            refuted_trace_commitment: binding.refuted_trace_commitment,
            window_is_terminal: binding.window_is_terminal,
            final_committed_root: binding.final_committed_root,
        })),
    }
}
