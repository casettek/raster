//! Optional, bounded context capture while the CLI loads a native trace.

use std::collections::{BTreeMap, BTreeSet};

use raster_core::recur_progress::RecurProgressStack;
use raster_core::trace::{StepRecord, TraceEvent};
use raster_core::transition::{
    CoordinateIndexMembershipProof, CoordinateIndexNonMembershipProof, StorageEntry,
    StorageWriteWitness,
};
use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::trace::{
    serializable_frontier_from_trace_frontier, Bytes, BytesHashable, TraceTree, TraceTreeFrontier,
};
use raster_prover::transition_profile::TransitionProfileContext;

use crate::storage::StorageManager;

#[derive(Debug, Clone)]
pub struct ReplayProfileSample {
    pub exec_index: u64,
    pub context: TransitionProfileContext,
    pub reads: Vec<CoordinateIndexMembershipProof>,
    pub write: Option<StorageWriteWitness>,
}

type CapturedSample = std::result::Result<ReplayProfileSample, String>;
pub(crate) type PendingSample = Option<(String, CapturedSample)>;

#[derive(Debug, Clone)]
pub(crate) struct ReplayProfileCapture {
    pending: BTreeSet<String>,
    frontier: TraceTreeFrontier,
    samples: BTreeMap<String, CapturedSample>,
}

impl ReplayProfileCapture {
    pub fn new(selected: impl IntoIterator<Item = String>) -> Self {
        let mut tree = TraceTree::new(1);
        tree.append(Bytes(EMPTY_TRIE_NODES[0].to_vec()));
        Self {
            pending: selected.into_iter().collect(),
            frontier: tree.frontier().unwrap().clone(),
            samples: BTreeMap::new(),
        }
    }

    pub fn sample(&self, tile: &str) -> Option<&CapturedSample> {
        self.samples.get(tile)
    }

    pub fn before(
        &self,
        event: &TraceEvent,
        storage: &StorageManager,
        recur: &RecurProgressStack,
    ) -> PendingSample {
        let call = match event {
            TraceEvent::TileExec(call) | TraceEvent::RecurTileIterationExec(call) => call,
            _ => return None,
        };
        if !self.pending.contains(&call.fn_name) {
            return None;
        }
        let sample = (|| {
            let reads = call
                .input
                .iter()
                .flat_map(|input| input.storage().values())
                .map(|input| {
                    storage
                        .profile_index_witness(&input.coordinates)
                        .ok_or_else(|| format!("Missing storage input at {:?}", input.coordinates))
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let snapshot = storage.snapshot();
            Ok(ReplayProfileSample {
                exec_index: 0,
                context: TransitionProfileContext {
                    frontier: serializable_frontier_from_trace_frontier(self.frontier.clone()),
                    storage_frontier: snapshot.frontier,
                    storage_root: snapshot.root,
                    storage_index_root: snapshot.index_root,
                    recur_progress: recur.clone(),
                },
                reads,
                write: None,
            })
        })();
        Some((call.fn_name.clone(), sample))
    }

    pub fn after(&mut self, pending: PendingSample, step: &StepRecord, storage: &StorageManager) {
        if let Some((tile, sample)) = pending {
            let sample = sample.map(|mut sample| {
                sample.exec_index = step.exec_index;
                if let Some(proof) = storage.profile_index_witness(step.coordinates()) {
                    // An insertion changes only this leaf. Its siblings are
                    // identical before/after, so the post-insertion path also
                    // proves nonmembership against the recorded before-root.
                    sample.write = Some(StorageWriteWitness {
                        entry: StorageEntry {
                            coordinates: proof.coordinates.clone(),
                            object_commitment: proof.value.object_commitment.clone(),
                        },
                        index_non_membership_witness: CoordinateIndexNonMembershipProof {
                            coordinates: proof.coordinates.clone(),
                            siblings: proof.siblings.clone(),
                        },
                        index_membership_witness: proof,
                    });
                }
                sample
            });
            self.pending.remove(&tile);
            self.samples.insert(tile, sample);
        }
        if !self.pending.is_empty() {
            self.frontier.append(Bytes(step.hash()));
        }
    }
}
