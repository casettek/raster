//! The window binding: the window's committed fingerprint is a slice of the
//! disputed `commit.bin`, a window at trace index 0 opens on the genesis state,
//! and the revealed tail roots match the commitment (`transition-split`).

use std::collections::BTreeMap;

use raster_core::coordinate_index::coordinate_index_root;
use raster_core::merkle::NodeHash;
use raster_core::transition::{FingerprintSliceWitness, InitTransition, TraceCommitmentHeader};

use guest_common::merkle_tree::{combine_merkle_level, sha256_bytes, EMPTY_LEAF};

/// Hold a window that opens at trace index 0 to the genesis state.
///
/// Every other window carries a *margin*: the steps before the divergence must
/// reproduce the committed fingerprint, and that is the only thing tying the
/// challenger-supplied opening state to reality. A window opening at index 0
/// cannot have one — there are no steps before it — which is why the head of a
/// trace looked unprovable.
///
/// It does not need one. At index 0 the opening state is not the challenger's
/// to choose: the trace tree holds only the seed, which is the same public
/// constant for every program (`raster_core::merkle::EMPTY_LEAF`, on both
/// sides), storage is empty, and no draft or loop is in flight. Asserting that
/// removes the need for the margin rather than excusing its absence.
///
/// What the seed does *not* say is which program ran with which inputs — the
/// seed is program-independent. That comes from the window's first step being
/// `ProgramStart`, whose `output_commitment` must equal the combined root over
/// the authorization journal's entry-argument commitments
/// (`checks::entrypoint::verify_step`), and whose structural fields are pinned
/// by `verify_exec_index` and `verify_sequence_id`. Together those make the
/// record unique, which is what a one-item window needs.
fn assert_opens_at_genesis(init_transition: &InitTransition) {
    let frontier = &init_transition.init_frontier;
    assert!(
        frontier.position == 0 && frontier.leaf == EMPTY_LEAF && frontier.ommers.is_empty(),
        "A window opening at trace index 0 must open on the seed leaf alone",
    );

    let storage = &init_transition.init_storage_frontier;
    assert!(
        storage.position == 0 && storage.leaf == EMPTY_LEAF && storage.ommers.is_empty(),
        "A window opening at trace index 0 must open on empty storage",
    );
    assert!(
        init_transition.init_storage_index_root == coordinate_index_root(&BTreeMap::new()),
        "A window opening at trace index 0 must open on an empty coordinate index",
    );
}

pub struct WindowBinding {
    /// `TraceCommitmentHeader::digest()` of the commitment the window is a
    /// slice of: the commitment a fraud proof over this window refutes.
    pub refuted_trace_commitment: Vec<u8>,
    /// Whether this window ends exactly where the commitment's fingerprint
    /// ends. See `FraudWindowJournal::window_is_terminal`.
    pub window_is_terminal: bool,
    /// The committed trace root for this window's final index, when that index
    /// falls inside the revealed tail. See
    /// `FraudWindowJournal::final_committed_root`.
    pub final_committed_root: Option<Vec<u8>>,
}

/// Prove the window's committed fingerprint is a slice of the named
/// commitment — closing the framing gap where a challenger supplies a
/// "committed" window fingerprint that appears nowhere in the `commit.bin`
/// the receipt ends up blamed on.
///
/// The offset is never host-claimed: the initial trace frontier holds the
/// seed plus one leaf per pre-window step, so its position *is* the window's
/// start index `s`. The witness must then cover exactly the packed blocks
/// spanning items `[s, s + w)`, each proven against the header's
/// `fingerprint_root` at its derived index, and every window item's
/// fingerprint value must equal the value at its bit offset inside those
/// proven blocks.
///
/// Returns whether the window is **terminal** in that commitment — it ends
/// exactly where the committed fingerprint ends. Derived here because this is
/// the one place holding `s`, `w` and `header.fingerprint_len` at once; the
/// header is not kept after the binding. See `FraudWindowJournal::window_is_terminal`.
pub fn assert_window_is_commitment_slice(
    init_transition: &InitTransition,
    header: &TraceCommitmentHeader,
    slice_witness: &FingerprintSliceWitness,
    revealed_tail_roots: Option<&Vec<NodeHash>>,
) -> WindowBinding {
    let window_fingerprint = &init_transition.fingerprint;
    assert!(
        window_fingerprint.bits_packer == header.bits_packer,
        "Window fingerprint bit packing does not match the committed fingerprint's"
    );
    let bits_per_item = header.bits_packer.bits_per_item();
    let window_len = window_fingerprint.len();
    assert!(window_len > 0, "Window fingerprint is empty");

    // The window's *shape*, which nothing constrained before the header carried
    // `window_size`. `window_len` comes from the challenger's `Fingerprint::len`
    // — a metadata field `Fingerprint::from` stores verbatim without checking it
    // against `bits` — and `window_start` from the challenger's frontier
    // position, leaving only the upper bound below to hold them.
    //
    // A short window is the degenerate case rather than a merely unusual one:
    // counting what `finalize` compares over `L` items, item 0 is `First` and
    // never compared, items `1..L-2` must match, and item `L-1` must diverge.
    // At `L = 2` that is *zero* matching comparisons, so nothing ties the
    // challenger's opening state to reality and a window fabricated anywhere in
    // the trace reaches `Finished`.
    let declared_window_size =
        usize::try_from(header.window_size).expect("Window size overflows usize");

    let window_start = usize::try_from(init_transition.init_frontier.position)
        .expect("Window start position overflows usize");

    if window_len != declared_window_size {
        // A short window is legal in exactly one place, and it is not a
        // concession: a divergence inside the trace's first `window_size` steps
        // has no room for a full window behind it, and the rolling buffer
        // correctly yields `min(i + 1, w)` items starting at 0. So the honest
        // host can produce a short window *only* at trace index 0.
        assert!(
            window_start == 0 && window_len < declared_window_size,
            "Window declares {} items against a commitment built with a window of {}, and \
             only a window opening at trace index 0 may be short",
            window_len,
            declared_window_size,
        );
        // And there it needs no pre-divergence margin. The margin exists to
        // pin a *challenger-supplied* opening state to reality by replaying
        // forward from it; at index 0 the opening state is not supplied at all,
        // it is the public genesis constant. Assert that directly and the
        // missing margin stops mattering rather than being tolerated.
        assert_opens_at_genesis(init_transition);
    }
    let fingerprint_len =
        usize::try_from(header.fingerprint_len).expect("Fingerprint length overflows usize");
    assert!(
        window_start + window_len <= fingerprint_len,
        "Window range exceeds the committed fingerprint"
    );

    // The covering block range is derived, not supplied; a witness for any
    // other range fails here.
    let first_block = (window_start * bits_per_item) / 64;
    let last_block = ((window_start + window_len) * bits_per_item - 1) / 64;
    assert!(
        slice_witness.blocks.len() == last_block - first_block + 1,
        "Fingerprint slice witness does not cover exactly the window's blocks"
    );

    let mut proven_blocks: Vec<u64> = Vec::with_capacity(slice_witness.blocks.len());
    for (offset, block_witness) in slice_witness.blocks.iter().enumerate() {
        let expected_position = (first_block + offset) as u64;
        assert!(
            block_witness.position == expected_position,
            "Fingerprint block witness is at the wrong index"
        );
        let mut current = sha256_bytes(&block_witness.block.to_le_bytes());
        for (level, sibling) in block_witness.path_elems.iter().enumerate() {
            current = if ((block_witness.position >> level) & 1) == 0 {
                combine_merkle_level(level, &current, sibling.as_slice())
            } else {
                combine_merkle_level(level, sibling.as_slice(), &current)
            };
        }
        assert!(
            current == header.fingerprint_root,
            "Fingerprint block inclusion proof is invalid"
        );
        proven_blocks.push(block_witness.block);
    }

    for item in 0..window_len {
        let bit_offset = (window_start + item) * bits_per_item - first_block * 64;
        let committed_value = header
            .bits_packer
            .try_get_at_bit_offset(bit_offset, &proven_blocks)
            .expect("Window item exceeds the proven fingerprint blocks");
        let window_value = window_fingerprint
            .bits_packer
            .try_get(item, &window_fingerprint.bits)
            .expect("Window fingerprint is shorter than its declared length");
        assert!(
            committed_value == window_value,
            "Window fingerprint diverges from the committed fingerprint slice"
        );
    }

    // The revealed tail, if the host supplied it. Binding is unconditional
    // once supplied; whether it is *useful* depends on where this window ends.
    let final_committed_root = revealed_tail_roots.and_then(|roots| {
        let roots_bytes =
            postcard::to_allocvec(roots).expect("revealed tail roots are serializable");
        assert!(
            sha256_bytes(&roots_bytes) == header.revealed_tail_roots_commitment,
            "Revealed tail roots do not match the commitment's tail-roots commitment",
        );
        assert!(
            roots.len() == declared_window_size,
            "Commitment reveals {} tail roots but declares a window of {}",
            roots.len(),
            declared_window_size,
        );

        // The tail covers the fingerprint's final `window_size` indices.
        let tail_start = fingerprint_len - roots.len();
        let final_index = window_start + window_len - 1;
        final_index
            .checked_sub(tail_start)
            .and_then(|offset| roots.get(offset))
            .map(|root| root.to_vec())
    });

    WindowBinding {
        refuted_trace_commitment: header.digest(),
        // `<=` was asserted above; equality is the terminal case.
        window_is_terminal: window_start + window_len == fingerprint_len,
        final_committed_root,
    }
}

