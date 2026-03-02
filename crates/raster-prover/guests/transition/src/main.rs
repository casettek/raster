//! RISC0 guest program for trace state transitions.
//!
//! This guest performs a single state transition of the bridge tree by:
//! 1. Taking a serialized frontier + trace item data as input
//! 2. Hashing the trace item and appending it to the frontier
//! 3. Returning the new frontier

use std::cmp::Ordering;

use bridgetree::{Hashable, Level, NonEmptyFrontier, Position};
use risc0_zkvm::guest::env;

use raster_core::fingerprint::{Fingerprint, FingerprintAccumulator};

use raster_core::trace::StepRecord;

use serde::{Deserialize, Serialize};

use sha2::{Digest, Sha256};

// ============================================================================
// Wire-format types (same layout as host for postcard compatibility)
// ============================================================================
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bytes(pub Vec<u8>);

pub type TraceBridgeTree = bridgetree::BridgeTree<Bytes, u64, 32>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SerializableFrontier {
    pub position: u64,
    pub leaf: Vec<u8>,
    pub ommers: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionInput {
    pub trace_item: StepRecord,

    pub replay_image_id: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Transition {
    pub frontier: SerializableFrontier,
    pub actual_fingerprint_acc: FingerprintAccumulator,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct InitTransitionState {
    pub init_frontier: SerializableFrontier,
    pub fingerprint: Fingerprint,
    // TODO: Init Transition should verify proof of inclusion of reference fingerprint
    // pub ref_fingerprint_inclusion_proof: Vec<u8>,
    // TODO: Init Transition should contain reference to CFS
    // pub cfs: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum TransitionState {
    Init(InitTransitionState),
    Next(Transition),
    Finished,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TransitionJournal {
    pub init_state: InitTransitionState,
    pub current_state: TransitionState,

    pub self_image_id: Vec<u8>,
}

// ============================================================================
// Bytes + Hashable for bridgetree (matches prover's empty leaf and combine)
// ============================================================================

const HASH_SIZE: usize = 32;

/// Empty leaf hash (precomputed SHA256 of "empty"); matches prover EMPTY_TRIE_NODES[0].
const EMPTY_LEAF: [u8; 32] =
    hex_literal::hex!("6d97a6c02676a41a9636c6cd4e5d2d47d14d27a35d18e608115fd93cd42e6b3a");

impl PartialEq for Bytes {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for Bytes {}

impl PartialOrd for Bytes {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Bytes {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl Hashable for Bytes {
    fn empty_leaf() -> Self {
        Bytes(EMPTY_LEAF.to_vec())
    }

    fn combine(level: Level, a: &Self, b: &Self) -> Self {
        let mut data = Vec::with_capacity(1 + HASH_SIZE + HASH_SIZE);
        data.push(u8::from(level));
        data.extend_from_slice(&a.0);
        data.extend_from_slice(&b.0);
        let mut hasher = Sha256::new();
        hasher.update(&data);
        Bytes(hasher.finalize().to_vec())
    }
}

// ============================================================================
// SerializableFrontier <-> NonEmptyFrontier<Bytes>
// ============================================================================

impl SerializableFrontier {
    fn to_frontier(&self) -> Option<NonEmptyFrontier<Bytes>> {
        NonEmptyFrontier::from_parts(
            Position::from(self.position),
            Bytes(self.leaf.clone()),
            self.ommers.iter().map(|o| Bytes(o.clone())).collect(),
        )
        .ok()
    }

    fn from_frontier(frontier: &NonEmptyFrontier<Bytes>) -> Self {
        Self {
            position: frontier.position().into(),
            leaf: frontier.leaf().0.clone(),
            ommers: frontier.ommers().iter().map(|o| o.0.clone()).collect(),
        }
    }
}

// ============================================================================
// Hashing
// ============================================================================

/// Hash a StepRecord using SHA256 of its postcard-serialized form.
fn hash_trace_item(item: &StepRecord) -> Vec<u8> {
    let data = postcard::to_allocvec(item).expect("Failed to serialize StepRecord");
    let mut hasher = Sha256::new();
    hasher.update(&data);
    hasher.finalize().to_vec()
}

// ============================================================================
// Main
// ============================================================================

fn main() {
    let self_image_id: Vec<u8> = env::read();

    let input: TransitionInput = env::read();
    let state: TransitionState = env::read();

    match state {
        TransitionState::Init(init_transition) => {
            let mut init_frontier = init_transition
                .init_frontier
                .to_frontier()
                .expect("Invalid frontier in input");

            let replay_image_id_digest =
                risc0_zkvm::sha::Digest::try_from(input.replay_image_id.as_slice())
                    .expect("image_id must be 32 bytes");
            env::verify(replay_image_id_digest, &input.trace_item.fn_call_record.output_data)
                .expect("Failed to verify trace replay image id");

            let item_hash = hash_trace_item(&input.trace_item);
            init_frontier.append(Bytes(item_hash.clone()));

            let tree = TraceBridgeTree::from_frontier(1, init_frontier.clone());
            let Some(tree_root) = tree.root(0) else {
                panic!("Can't get tree root");
            };

            let mut actual_fingerprint_acc =
                FingerprintAccumulator::new(init_transition.fingerprint.bits_packer.clone());
            actual_fingerprint_acc.append(&tree_root.0);

            let new_frontier = SerializableFrontier::from_frontier(&init_frontier);

            let current_state = TransitionState::Next(Transition {
                frontier: new_frontier,
                actual_fingerprint_acc,
            });

            let journal = TransitionJournal {
                init_state: init_transition,
                current_state,
                self_image_id,
            };

            env::commit(&journal);
        }
        TransitionState::Next(transition) => {
            let prev_journal: TransitionJournal = env::read();

            let self_image_id_digest = risc0_zkvm::sha::Digest::try_from(self_image_id.as_slice())
                .expect("image_id must be 32 bytes");
            // TODO: Did as_bytes actually available?
            env::verify(
                self_image_id_digest,
                &risc0_zkvm::serde::to_vec(&prev_journal).unwrap(),
            )
            .expect("Failed to verify trace replay image id");

            assert!(
                self_image_id == prev_journal.self_image_id,
                "Self image id does not match"
            );

            let TransitionState::Next(prev_transition) = prev_journal.current_state else {
                panic!("Provided Transition in Next State does not match the expected Transition State in Journal");
            };
            assert!(
                prev_transition == transition.clone(),
                "Transition Expected to be the same as the previous journal Transition"
            );

            let mut current_frontier = transition
                .frontier
                .to_frontier()
                .expect("Invalid frontier in input");

            let replay_image_id_digest =
                risc0_zkvm::sha::Digest::try_from(input.replay_image_id.as_slice())
                    .expect("image_id must be 32 bytes");
            // TODO: Maybe replay guest should have full trace item as output journal
            env::verify(replay_image_id_digest, &input.trace_item.fn_call_record.output_data)
                .expect("Failed to verify trace replay image id");

            let item_hash = hash_trace_item(&input.trace_item);
            current_frontier.append(Bytes(item_hash.clone()));

            let tree = TraceBridgeTree::from_frontier(1, current_frontier.clone());
            let Some(tree_root) = tree.root(0) else {
                panic!("Can't get tree root");
            };

            let mut actual_fingerprint_acc = transition.actual_fingerprint_acc.clone();
            actual_fingerprint_acc.append(&tree_root.0);

            let new_frontier = SerializableFrontier::from_frontier(&current_frontier);

            let actual_fingerprint: Fingerprint = actual_fingerprint_acc.into_fingerprint();

            let current_state =
                if actual_fingerprint.len() == prev_journal.init_state.fingerprint.len() {
                    assert!(actual_fingerprint.diff_at_index(
                        actual_fingerprint.len() - 1,
                        &prev_journal.init_state.fingerprint
                    ));

                    TransitionState::Finished
                } else {
                    assert!(!actual_fingerprint.diff_at_index(
                        actual_fingerprint.len() - 1,
                        &prev_journal.init_state.fingerprint
                    ));

                    TransitionState::Next(Transition {
                        frontier: new_frontier,
                        actual_fingerprint_acc: FingerprintAccumulator::from(actual_fingerprint),
                    })
                };
            let journal = TransitionJournal {
                init_state: prev_journal.init_state,
                current_state,
                self_image_id,
            };

            env::commit(&journal);
        }

        TransitionState::Finished => {
            panic!("Finished Transition");
        }
    }
}
