//! Tests of the trace guest: the window binding, and the segment's append and
//! verdict.

use std::collections::BTreeMap;

use raster_core::coordinate_index::coordinate_index_root;
use raster_core::merkle::{to_node_hash, NodeHash};
use raster_core::transition::SerializableFrontier;

use guest_common::merkle_tree::{sha256_bytes, Bytes, TraceBridgeTree, EMPTY_LEAF};

use raster_core::fingerprint::{BitPacker, Fingerprint};
use raster_core::transition::InitTransition;
use raster_core::transition::{
    FingerprintBlockWitness, FingerprintSliceWitness, TraceCommitmentHeader,
};

use crate::window::assert_window_is_commitment_slice;

/// Mirror of the prover's fingerprint-block tree: leaf `i` =
/// `sha256(bits[i].to_le_bytes())`, combined by the shared trace-tree
/// convention. Rebuilt here so the guest-side check is exercised against
/// an independently constructed witness.
/// A header whose declared window size matches the window being built —
/// the ordinary case, and what every slice test other than the shape tests
/// wants.
fn build_header_and_witness(
    bits: &[u64],
    bits_per_item: usize,
    fingerprint_len: usize,
    window_start: usize,
    window_len: usize,
) -> (TraceCommitmentHeader, FingerprintSliceWitness) {
    build_header_and_witness_for_window(
        bits,
        bits_per_item,
        fingerprint_len,
        window_start,
        window_len,
        window_len,
    )
}

/// A header whose tail-roots commitment matches `tail_roots`, so the
/// revealed-tail binding can be exercised.
fn build_header_and_witness_with_tail(
    bits: &[u64],
    bits_per_item: usize,
    fingerprint_len: usize,
    window_start: usize,
    window_len: usize,
    tail_roots: &[NodeHash],
) -> (TraceCommitmentHeader, FingerprintSliceWitness) {
    let (mut header, witness) = build_header_and_witness_for_window(
        bits,
        bits_per_item,
        fingerprint_len,
        window_start,
        window_len,
        tail_roots.len(),
    );
    header.revealed_tail_roots_commitment = sha256_bytes(
        &postcard::to_allocvec(&tail_roots.to_vec()).expect("tail roots serialize"),
    );
    (header, witness)
}

/// As above, but the commitment declares `window_size` independently of the
/// window actually presented — which is what the shape check is about.
fn build_header_and_witness_for_window(
    bits: &[u64],
    bits_per_item: usize,
    fingerprint_len: usize,
    window_start: usize,
    window_len: usize,
    window_size: usize,
) -> (TraceCommitmentHeader, FingerprintSliceWitness) {
    let first_block = (window_start * bits_per_item) / 64;
    let last_block = ((window_start + window_len) * bits_per_item - 1) / 64;

    let mut tree = TraceBridgeTree::new(1);
    let mut marked = Vec::new();
    for (index, block) in bits.iter().enumerate() {
        tree.append(Bytes(sha256_bytes(&block.to_le_bytes())));
        if (first_block..=last_block).contains(&index) {
            marked.push(tree.mark().expect("mark fingerprint block"));
        }
    }
    let fingerprint_root = tree.root(0).expect("fingerprint root").0;

    let blocks = (first_block..=last_block)
        .zip(marked)
        .map(|(index, position)| {
            let path = tree.witness(position, 0).expect("block witness");
            FingerprintBlockWitness {
                block: bits[index],
                position: u64::from(position),
                path_elems: path.iter().map(|elem| to_node_hash(&elem.0)).collect(),
            }
        })
        .collect();

    let header = TraceCommitmentHeader {
        bits_packer: BitPacker::new(bits_per_item),
        fingerprint_len: fingerprint_len as u64,
        fingerprint_root,
        revealed_items_commitment: vec![9; 32],
        window_size: window_size as u64,
        revealed_tail_roots_commitment: vec![8; 32],
    };
    (header, FingerprintSliceWitness { blocks })
}

fn init_transition_with_window(window_start: usize, window: Fingerprint) -> InitTransition {
    InitTransition {
        // Only the frontier's position matters to the slice check: it is
        // what fixes the window's offset in the committed fingerprint.
        init_frontier: SerializableFrontier {
            position: window_start as u64,
            leaf: EMPTY_LEAF.to_vec(),
            ommers: Vec::new(),
        },
        init_storage_frontier: SerializableFrontier {
            position: 0,
            leaf: EMPTY_LEAF.to_vec(),
            ommers: Vec::new(),
        },
        init_storage_root: Vec::new(),
        init_storage_index_root: Vec::new(),
        fingerprint: window,
    }
}

/// 40 items at 4 bits each = 160 bits = 3 packed blocks, with per-item
/// values `i & 0xf` so every offset mistake changes some value.
fn fixture_bits(bits_per_item: usize, items: usize) -> Vec<u64> {
    let packer = BitPacker::new(bits_per_item);
    let hashes: Vec<Vec<u8>> = (0..items).map(|i| vec![i as u8; 32]).collect();
    packer.pack(&hashes)
}

/// Regression for the unbound-shape hole (was a proof of concept).
///
/// `window_len` comes from the challenger's `Fingerprint::len` — a metadata
/// field `Fingerprint::from` stores verbatim without checking it against
/// `bits` — and `window_start` from the challenger's frontier position. The
/// only constraint used to be `window_start + window_len <=
/// fingerprint_len`: an upper bound, not a shape.
///
/// A two-item window is the degenerate case, not a merely unusual one. Over
/// `L` items `finalize` never compares item 0, requires items `1..L-2` to
/// match, and requires item `L-1` to diverge — so at `L = 2` there are
/// **zero** matching comparisons, nothing pins the challenger's opening
/// state to reality, and a window fabricated anywhere in the trace reaches
/// `Finished`.
///
/// Note the slice itself is *genuine* here: no bits are tampered, no
/// witness is forged. The lie is only the window's shape, which is why no
/// other check catches it.
#[test]
#[should_panic(expected = "against a commitment built with a window of")]
fn a_short_window_is_refused_anywhere_in_the_commitment() {
    let (bits_per_item, items) = (4, 40);
    let bits = fixture_bits(bits_per_item, items);
    // Mid-commitment, arbitrary, and nothing like a head or terminal window.
    let (window_start, window_len, window_size) = (20, 2, 4);

    let packer = BitPacker::new(bits_per_item);
    let window_bits = packer
        .get_range(window_start, window_start + window_len, &bits)
        .expect("window slice");
    let window = Fingerprint::from(window_bits, packer, window_len);

    let (header, witness) = build_header_and_witness_for_window(
        &bits,
        bits_per_item,
        items,
        window_start,
        window_len,
        window_size,
    );
    assert_window_is_commitment_slice(
        &init_transition_with_window(window_start, window),
        &header,
        &witness,
        None,
    );
}

/// The tail roots, when supplied, are bound to the commitment — and the
/// root the terminal step will need is picked out at the same time.
#[test]
fn a_window_ending_in_the_tail_carries_its_committed_root() {
    let (bits_per_item, items) = (4, 40);
    let bits = fixture_bits(bits_per_item, items);
    // The commitment's window is 4, so its tail covers items 36..40.
    // A window ending at item 39 ends inside it.
    let (window_start, window_len) = (36, 4);
    let tail_roots: Vec<NodeHash> = (36..40u8).map(|i| [i; 32].into()).collect();

    let packer = BitPacker::new(bits_per_item);
    let window_bits = packer
        .get_range(window_start, window_start + window_len, &bits)
        .expect("window slice");
    let window = Fingerprint::from(window_bits, packer, window_len);

    let (header, witness) = build_header_and_witness_with_tail(
        &bits,
        bits_per_item,
        items,
        window_start,
        window_len,
        &tail_roots,
    );
    let binding = assert_window_is_commitment_slice(
        &init_transition_with_window(window_start, window),
        &header,
        &witness,
        Some(&tail_roots),
    );

    assert!(binding.window_is_terminal);
    // Item 39 is the last of the tail.
    assert_eq!(binding.final_committed_root, Some(vec![39u8; 32]));
}

/// A window that ends before the revealed tail gets no root, and so keeps
/// proving divergence the only way it can — on fingerprint entries.
#[test]
fn a_window_ending_before_the_tail_carries_no_committed_root() {
    let (bits_per_item, items) = (4, 40);
    let bits = fixture_bits(bits_per_item, items);
    let (window_start, window_len) = (10, 4);
    let tail_roots: Vec<NodeHash> = (36..40u8).map(|i| [i; 32].into()).collect();

    let packer = BitPacker::new(bits_per_item);
    let window_bits = packer
        .get_range(window_start, window_start + window_len, &bits)
        .expect("window slice");
    let window = Fingerprint::from(window_bits, packer, window_len);

    let (header, witness) = build_header_and_witness_with_tail(
        &bits,
        bits_per_item,
        items,
        window_start,
        window_len,
        &tail_roots,
    );
    let binding = assert_window_is_commitment_slice(
        &init_transition_with_window(window_start, window),
        &header,
        &witness,
        Some(&tail_roots),
    );

    assert!(!binding.window_is_terminal);
    assert_eq!(binding.final_committed_root, None);
}

/// Supplying roots the commitment did not commit to is refused — otherwise
/// a challenger could invent a root to "diverge" from.
#[test]
#[should_panic(expected = "do not match the commitment's tail-roots commitment")]
fn fabricated_tail_roots_are_refused() {
    let (bits_per_item, items) = (4, 40);
    let bits = fixture_bits(bits_per_item, items);
    let (window_start, window_len) = (36, 4);
    let committed: Vec<NodeHash> = (36..40u8).map(|i| [i; 32].into()).collect();

    let packer = BitPacker::new(bits_per_item);
    let window_bits = packer
        .get_range(window_start, window_start + window_len, &bits)
        .expect("window slice");
    let window = Fingerprint::from(window_bits, packer, window_len);

    let (header, witness) = build_header_and_witness_with_tail(
        &bits,
        bits_per_item,
        items,
        window_start,
        window_len,
        &committed,
    );

    // A different story about the tail, so the terminal step could claim a
    // divergence that never happened.
    let invented: Vec<NodeHash> = (36..40u8).map(|i| [i ^ 0xFF; 32].into()).collect();
    assert_window_is_commitment_slice(
        &init_transition_with_window(window_start, window),
        &header,
        &witness,
        Some(&invented),
    );
}

/// A window opening at the genesis state — the one place a short window is
/// legitimate.
fn genesis_init_transition(window: Fingerprint) -> InitTransition {
    InitTransition {
        init_frontier: SerializableFrontier {
            position: 0,
            leaf: EMPTY_LEAF.to_vec(),
            ommers: Vec::new(),
        },
        init_storage_frontier: SerializableFrontier {
            position: 0,
            leaf: EMPTY_LEAF.to_vec(),
            ommers: Vec::new(),
        },
        init_storage_root: Vec::new(),
        init_storage_index_root: coordinate_index_root(&BTreeMap::new()),
        fingerprint: window,
    }
}

fn short_genesis_window(
    bits: &[u64],
    bits_per_item: usize,
    items: usize,
    window_len: usize,
    window_size: usize,
) -> (InitTransition, TraceCommitmentHeader, FingerprintSliceWitness) {
    let packer = BitPacker::new(bits_per_item);
    let window_bits = packer
        .get_range(0, window_len, bits)
        .expect("window slice");
    let window = Fingerprint::from(window_bits, packer, window_len);
    let (header, witness) = build_header_and_witness_for_window(
        bits, bits_per_item, items, 0, window_len, window_size,
    );
    (genesis_init_transition(window), header, witness)
}

/// A divergence inside the trace's first `window_size` steps has no room
/// for a full window behind it, so the honest host emits a short one — and
/// it can only ever do so at trace index 0.
///
/// It carries no pre-divergence margin, and needs none: the margin exists
/// to pin a *challenger-supplied* opening state, and at index 0 the opening
/// state is the public genesis constant instead.
#[test]
fn a_short_window_at_genesis_is_accepted() {
    let (bits_per_item, items) = (4, 40);
    let bits = fixture_bits(bits_per_item, items);
    let (init, header, witness) = short_genesis_window(&bits, bits_per_item, items, 2, 4);

    let binding = assert_window_is_commitment_slice(&init, &header, &witness, None);
    assert!(!binding.window_is_terminal);
}

/// Even a one-item window, which is what a divergence in the trace's very
/// first step produces.
#[test]
fn a_one_item_genesis_window_is_accepted() {
    let (bits_per_item, items) = (4, 40);
    let bits = fixture_bits(bits_per_item, items);
    let (init, header, witness) = short_genesis_window(&bits, bits_per_item, items, 1, 4);

    assert_window_is_commitment_slice(&init, &header, &witness, None);
}

/// Position 0 alone is not enough: the opening *state* has to be genesis,
/// or the missing margin would be a hole rather than an irrelevance.
#[test]
#[should_panic(expected = "must open on an empty coordinate index")]
fn a_short_window_claiming_genesis_with_dirty_storage_is_refused() {
    let (bits_per_item, items) = (4, 40);
    let bits = fixture_bits(bits_per_item, items);
    let (mut init, header, witness) = short_genesis_window(&bits, bits_per_item, items, 2, 4);

    // Storage that already holds something cannot be the state before the
    // program's first step.
    init.init_storage_index_root = vec![7u8; 32];

    assert_window_is_commitment_slice(&init, &header, &witness, None);
}

/// The other direction: claiming more items than the commitment's window.
#[test]
#[should_panic(expected = "against a commitment built with a window of")]
fn a_window_longer_than_the_commitments_is_refused() {
    let (bits_per_item, items) = (4, 40);
    let bits = fixture_bits(bits_per_item, items);
    let (window_start, window_len, window_size) = (20, 8, 4);

    let packer = BitPacker::new(bits_per_item);
    let window_bits = packer
        .get_range(window_start, window_start + window_len, &bits)
        .expect("window slice");
    let window = Fingerprint::from(window_bits, packer, window_len);

    let (header, witness) = build_header_and_witness_for_window(
        &bits,
        bits_per_item,
        items,
        window_start,
        window_len,
        window_size,
    );
    assert_window_is_commitment_slice(
        &init_transition_with_window(window_start, window),
        &header,
        &witness,
        None,
    );
}

#[test]
fn accepts_a_window_slice_crossing_a_block_boundary() {
    let (bits_per_item, items) = (4, 40);
    let bits = fixture_bits(bits_per_item, items);
    // Items [14, 18) span bits [56, 72) — blocks 0 and 1.
    let (window_start, window_len) = (14, 4);

    let packer = BitPacker::new(bits_per_item);
    let window_bits = packer
        .get_range(window_start, window_start + window_len, &bits)
        .expect("window slice");
    let window = Fingerprint::from(window_bits, packer, window_len);

    let (header, witness) =
        build_header_and_witness(&bits, bits_per_item, items, window_start, window_len);
    assert_window_is_commitment_slice(
        &init_transition_with_window(window_start, window),
        &header,
        &witness,
        None,
    );
}

#[test]
#[should_panic(expected = "diverges from the committed fingerprint slice")]
fn rejects_a_window_fingerprint_not_in_the_commitment() {
    let (bits_per_item, items) = (4, 40);
    let bits = fixture_bits(bits_per_item, items);
    let (window_start, window_len) = (14, 4);

    let packer = BitPacker::new(bits_per_item);
    let mut window_bits = packer
        .get_range(window_start, window_start + window_len, &bits)
        .expect("window slice");
    // A challenger-fabricated "committed" fingerprint: one flipped value.
    window_bits[0] ^= 0b1;
    let window = Fingerprint::from(window_bits, packer, window_len);

    let (header, witness) =
        build_header_and_witness(&bits, bits_per_item, items, window_start, window_len);
    assert_window_is_commitment_slice(
        &init_transition_with_window(window_start, window),
        &header,
        &witness,
        None,
    );
}

#[test]
#[should_panic(expected = "diverges from the committed fingerprint slice")]
fn rejects_a_genuine_slice_claimed_at_the_wrong_offset() {
    let (bits_per_item, items) = (4, 40);
    let bits = fixture_bits(bits_per_item, items);
    let (window_start, window_len) = (14, 4);

    let packer = BitPacker::new(bits_per_item);
    let window_bits = packer
        .get_range(window_start, window_start + window_len, &bits)
        .expect("window slice");
    let window = Fingerprint::from(window_bits, packer, window_len);

    // The frontier (and thus the derived offset) says one item later:
    // the same committed blocks yield different values there.
    let (header, witness) =
        build_header_and_witness(&bits, bits_per_item, items, window_start + 1, window_len);
    assert_window_is_commitment_slice(
        &init_transition_with_window(window_start + 1, window),
        &header,
        &witness,
        None,
    );
}

#[test]
#[should_panic(expected = "block inclusion proof is invalid")]
fn rejects_a_tampered_block_value() {
    let (bits_per_item, items) = (4, 40);
    let bits = fixture_bits(bits_per_item, items);
    let (window_start, window_len) = (14, 4);

    let packer = BitPacker::new(bits_per_item);
    let window_bits = packer
        .get_range(window_start, window_start + window_len, &bits)
        .expect("window slice");
    let window = Fingerprint::from(window_bits, packer, window_len);

    let (header, mut witness) =
        build_header_and_witness(&bits, bits_per_item, items, window_start, window_len);
    witness.blocks[0].block ^= 0b1;
    assert_window_is_commitment_slice(
        &init_transition_with_window(window_start, window),
        &header,
        &witness,
        None,
    );
}

#[test]
#[should_panic(expected = "Window range exceeds the committed fingerprint")]
fn rejects_a_window_past_the_committed_fingerprint() {
    let (bits_per_item, items) = (4, 40);
    let bits = fixture_bits(bits_per_item, items);
    let (window_start, window_len) = (38, 4);

    let packer = BitPacker::new(bits_per_item);
    let window_bits = packer.get_range(36, 40, &bits).expect("window slice");
    let window = Fingerprint::from(window_bits, packer, window_len);

    let (header, witness) = build_header_and_witness(&bits, bits_per_item, items, 36, 4);
    assert_window_is_commitment_slice(
        &init_transition_with_window(window_start, window),
        &header,
        &witness,
        None,
    );
}

/// Build a valid window at `[window_start, window_start + window_len)` of
/// a `items`-long commitment and report whether the check calls it
/// terminal.
fn terminality_of(window_start: usize, window_len: usize) -> bool {
    let (bits_per_item, items) = (4, 40);
    let bits = fixture_bits(bits_per_item, items);

    let packer = BitPacker::new(bits_per_item);
    let window_bits = packer
        .get_range(window_start, window_start + window_len, &bits)
        .expect("window slice");
    let window = Fingerprint::from(window_bits, packer, window_len);

    let (header, witness) =
        build_header_and_witness(&bits, bits_per_item, items, window_start, window_len);
    assert_window_is_commitment_slice(
        &init_transition_with_window(window_start, window),
        &header,
        &witness,
        None,
    )
    .window_is_terminal
}

/// A window ending exactly at the committed fingerprint's end is terminal:
/// a `ProgramEnd` inside it is the commitment's last item.
#[test]
fn a_window_ending_at_the_commitment_end_is_terminal() {
    // Items [36, 40) of a 40-item commitment.
    assert!(terminality_of(36, 4));
}

/// The case the flag exists for. This window is a perfectly valid slice —
/// the check passes — but it sits mid-commitment, so a `ProgramEnd` found
/// inside it would *not* be the trace's result.
#[test]
fn a_valid_mid_commitment_window_is_not_terminal() {
    assert!(!terminality_of(14, 4));
}

/// Off by exactly one item. Terminality is an equality, not a bound.
#[test]
fn a_window_one_item_short_of_the_end_is_not_terminal() {
    assert!(!terminality_of(35, 4));
}

// ============================================================================
// The segment: append and verdict
// ============================================================================

mod segment {
    use super::*;
    use raster_core::fingerprint::{BitPacker, Fingerprint};
    use raster_core::trace_check::{TraceSegmentHeader, TraceStepEntry};

    use guest_common::merkle_tree::{deserialize_frontier, frontier_root, serialize_frontier};

    use crate::segment::verify_segment;

    const BITS: usize = 16;
    const LEN: usize = 8;

    fn record_hash(i: usize) -> Vec<u8> {
        sha256_bytes(&(i as u64).to_le_bytes())
    }

    fn genesis_frontier() -> SerializableFrontier {
        SerializableFrontier {
            position: 0,
            leaf: EMPTY_LEAF.to_vec(),
            ommers: Vec::new(),
        }
    }

    /// The real roots after each of `LEN` appends from genesis, and the
    /// frontier after the first `k`.
    fn roots_and_frontier_after(k: usize) -> (Vec<Vec<u8>>, SerializableFrontier) {
        let mut frontier = deserialize_frontier(&genesis_frontier()).unwrap();
        let mut roots = Vec::new();
        let mut after_k = genesis_frontier();
        for i in 0..LEN {
            if i == k {
                after_k = serialize_frontier(&frontier);
            }
            frontier.append(Bytes(record_hash(i)));
            roots.push(frontier_root(&frontier));
        }
        (roots, after_k)
    }

    /// A genesis window of `LEN` steps whose commitment matches the real roots
    /// except at `diverge_at` (if any), where it holds a different root.
    fn header(diverge_at: Option<usize>, start: SerializableFrontier, steps: u64) -> TraceSegmentHeader {
        let (mut committed, _) = roots_and_frontier_after(0);
        if let Some(i) = diverge_at {
            committed[i] = committed[i].iter().map(|b| !b).collect();
        }
        let bits = BitPacker::new(BITS).pack(&committed);
        let (commitment_header, slice_witness) =
            build_header_and_witness(&bits, BITS, LEN, 0, LEN);
        TraceSegmentHeader {
            init: genesis_init_transition(Fingerprint::from(bits, BitPacker::new(BITS), LEN)),
            commitment_header,
            slice_witness,
            revealed_tail_roots: None,
            records_before: [0; 32],
            start_frontier: start,
            step_count: steps,
        }
    }

    fn run(header: TraceSegmentHeader, first: usize) -> Vec<TraceStepEntry> {
        let mut next = first;
        verify_segment(header, || {
            let hash = record_hash(next);
            next += 1;
            hash
        })
        .1
    }

    #[test]
    fn a_window_diverging_at_its_last_step_is_accepted_and_entries_chain() {
        let entries = run(header(Some(LEN - 1), genesis_frontier(), LEN as u64), 0);
        let (roots, _) = roots_and_frontier_after(0);
        assert_eq!(entries.len(), LEN);
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.trace_position, i as u64);
            assert_eq!(entry.step_record_hash, record_hash(i));
            assert_eq!(entry.trace_root_after, roots[i]);
        }
        for pair in entries.windows(2) {
            assert_eq!(pair[0].trace_root_after, pair[1].trace_root_before);
        }
    }

    /// The window in one segment, or in two each starting where the other
    /// ended, commits identical entries.
    #[test]
    fn one_segment_and_two_segments_commit_the_same_entries() {
        let whole = run(header(Some(LEN - 1), genesis_frontier(), LEN as u64), 0);
        let (_, after_3) = roots_and_frontier_after(3);
        let mut split = run(header(Some(LEN - 1), genesis_frontier(), 3), 0);
        split.extend(run(header(Some(LEN - 1), after_3, (LEN - 3) as u64), 3));
        assert_eq!(whole, split);
    }

    /// A segment that stops before the window's last step judges every step
    /// it covers as "must match", and passes.
    #[test]
    fn a_segment_short_of_the_window_end_only_checks_matches() {
        assert_eq!(run(header(Some(LEN - 1), genesis_frontier(), 3), 0).len(), 3);
    }

    #[test]
    #[should_panic(expected = "final step must diverge")]
    fn a_window_that_never_diverges_is_refused() {
        run(header(None, genesis_frontier(), LEN as u64), 0);
    }

    #[test]
    #[should_panic(expected = "Every window item before the last must match")]
    fn a_divergence_before_the_last_step_is_refused() {
        run(header(Some(2), genesis_frontier(), LEN as u64), 0);
    }

    /// The opening segment's start is the window's own initial frontier, not
    /// the host's choice.
    #[test]
    #[should_panic(expected = "must start from the window's initial frontier")]
    fn an_opening_segment_from_another_frontier_is_refused() {
        let mut other = genesis_frontier();
        other.leaf = vec![1; 32];
        run(header(Some(LEN - 1), other, LEN as u64), 0);
    }

    #[test]
    #[should_panic(expected = "is outside the window")]
    fn a_segment_running_past_the_window_is_refused() {
        let (_, after_3) = roots_and_frontier_after(3);
        run(header(Some(LEN - 1), after_3, LEN as u64), 3);
    }

    fn journal_of(header: TraceSegmentHeader, first: usize) -> raster_core::fraud_window::PartSegmentJournal {
        let window = raster_core::fraud_window::window_id(&header.init);
        let records_before = header.records_before;
        let mut next = first;
        let (binding, entries) = verify_segment(header, || {
            let hash = record_hash(next);
            next += 1;
            hash
        });
        crate::segment::segment_journal(window, records_before, binding, &entries)
    }

    /// Two segments' journals continue each other, and together end where the
    /// whole window's does: what FRAUD WINDOW stitches.
    #[test]
    fn segment_journals_stitch_to_the_whole_window() {
        let whole = journal_of(header(Some(LEN - 1), genesis_frontier(), LEN as u64), 0);
        let head = journal_of(header(Some(LEN - 1), genesis_frontier(), 3), 0);
        let (_, after_3) = roots_and_frontier_after(3);
        let mut tail_header = header(Some(LEN - 1), after_3, LEN as u64 - 3);
        tail_header.records_before = head.records.after;
        let tail = journal_of(tail_header, 3);

        assert_eq!(head.state.after, tail.state.before);
        assert_eq!((head.records.before, tail.records.after), (whole.records.before, whole.records.after));
        assert_eq!((head.state.before, tail.state.after), (whole.state.before, whole.state.after));
        assert_eq!((tail.first_position, tail.step_count), (3, LEN as u64 - 3));
        assert_eq!(head.out, tail.out, "every segment binds the window the same way");
        assert!(head.trace_output().is_some());
    }
}
