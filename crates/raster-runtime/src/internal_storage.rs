use raster_core::cfs::CfsCoordinates;
use raster_core::coordinate_index::coordinate_index_root;
use raster_core::input::{InternalArg, InternalRef};
use raster_core::transition::{InternalStoreEntry, InternalStoreIndexValue, SerializableFrontier};
use raster_core::{Error, Result};
use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::trace::{
    serializable_frontier_from_trace_frontier, Bytes, TraceTree, TraceTreeFrontier,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::vec::Vec;

use crate::Sha256Commitment;

#[derive(Debug, Clone)]
pub struct StoredInternalObject {
    pub reference: InternalRef,
    pub log_position: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct InternalStoreSnapshot {
    pub frontier: SerializableFrontier,
    pub root: Vec<u8>,
    pub index_root: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct InternalWriteRecord {
    pub entry: InternalStoreEntry,
    pub log_position: u64,
    pub store_root_before: Vec<u8>,
    pub store_root_after: Vec<u8>,
    pub index_root_before: Vec<u8>,
    pub index_root_after: Vec<u8>,
    pub frontier_after: SerializableFrontier,
}

#[derive(Debug, Clone)]
pub struct InternalStorageManager {
    frontier: TraceTreeFrontier,
    objects: BTreeMap<CfsCoordinates, StoredInternalObject>,
    coordinate_index: BTreeMap<CfsCoordinates, InternalStoreIndexValue>,
}

fn frontier_root(frontier: &TraceTreeFrontier) -> Vec<u8> {
    TraceTree::from_frontier(1, frontier.clone())
        .root(0)
        .expect("internal store root should exist")
        .0
}

impl InternalStorageManager {
    pub fn new() -> Self {
        let mut tree = TraceTree::new(1);
        tree.append(Bytes(EMPTY_TRIE_NODES[0].to_vec()));
        let frontier = tree
            .frontier()
            .cloned()
            .expect("internal store frontier should exist after seed append");
        Self {
            frontier,
            objects: BTreeMap::new(),
            coordinate_index: BTreeMap::new(),
        }
    }

    pub fn snapshot(&self) -> InternalStoreSnapshot {
        InternalStoreSnapshot {
            frontier: serializable_frontier_from_trace_frontier(self.frontier.clone()),
            root: self.current_root(),
            index_root: self.current_index_root(),
        }
    }

    pub fn current_root(&self) -> Vec<u8> {
        frontier_root(&self.frontier)
    }

    pub fn current_index_root(&self) -> Vec<u8> {
        coordinate_index_root(&self.coordinate_index)
    }

    pub fn append_serialized_bytes(
        &mut self,
        bytes: &[u8],
        coordinates: CfsCoordinates,
    ) -> InternalWriteRecord {
        assert!(
            !self.coordinate_index.contains_key(&coordinates),
            "Duplicate internal store write at coordinates {:?}",
            coordinates
        );

        let store_root_before = self.current_root();
        let index_root_before = self.current_index_root();
        let object_commitment = Sha256Commitment::from(bytes);
        let entry = InternalStoreEntry {
            coordinates: coordinates.clone(),
            object_commitment: object_commitment.into(),
        };
        let leaf_hash: Vec<u8> = Sha256Commitment::from(entry.to_bytes().as_slice()).into();

        self.frontier.append(Bytes(leaf_hash));
        let log_position: u64 = self.frontier.position().into();
        let index_value = InternalStoreIndexValue {
            log_position,
            object_commitment: entry.object_commitment.clone(),
        };
        self.coordinate_index
            .insert(coordinates.clone(), index_value);

        let reference = InternalRef::new(coordinates.clone(), entry.object_commitment.clone());

        self.objects.insert(
            coordinates,
            StoredInternalObject {
                reference,
                log_position,
                bytes: bytes.to_vec(),
            },
        );

        InternalWriteRecord {
            entry,
            log_position,
            store_root_before,
            store_root_after: self.current_root(),
            index_root_before,
            index_root_after: self.current_index_root(),
            frontier_after: serializable_frontier_from_trace_frontier(self.frontier.clone()),
        }
    }

    pub fn resolve<T: DeserializeOwned>(&self, reference: &InternalRef) -> Result<InternalArg<T>> {
        let stored = self.objects.get(&reference.coordinates).ok_or_else(|| {
            Error::Other(format!(
                "Missing internal store object at coordinates {:?}",
                reference.coordinates
            ))
        })?;
        if stored.reference.commitment != reference.commitment {
            return Err(Error::Other(format!(
                "Internal store commitment mismatch at coordinates {:?}",
                reference.coordinates
            )));
        }
        let actual_commitment: Vec<u8> = Sha256Commitment::from(stored.bytes.as_slice()).into();
        if actual_commitment != reference.commitment {
            return Err(Error::Other(format!(
                "Internal store object at coordinates {:?} failed integrity check",
                reference.coordinates
            )));
        }
        let value = raster_core::postcard::from_bytes(&stored.bytes).map_err(|e| {
            Error::Serialization(format!(
                "Failed to deserialize internal store object at coordinates {:?}: {}",
                reference.coordinates, e
            ))
        })?;
        Ok(InternalArg::new(
            reference.clone(),
            stored.bytes.clone(),
            value,
        ))
    }
}

impl Default for InternalStorageManager {
    fn default() -> Self {
        Self::new()
    }
}

static GLOBAL_INTERNAL_STORAGE: OnceLock<Mutex<InternalStorageManager>> = OnceLock::new();

fn global_manager() -> &'static Mutex<InternalStorageManager> {
    GLOBAL_INTERNAL_STORAGE.get_or_init(|| Mutex::new(InternalStorageManager::new()))
}

pub fn global_internal_store_snapshot() -> InternalStoreSnapshot {
    global_manager().lock().unwrap().snapshot()
}

pub fn store_internal_value<T: Serialize>(value: &T) -> Result<InternalRef> {
    let _ = value;
    Err(Error::Other(
        "Standalone internal-store writes require execution coordinates".into(),
    ))
}

pub fn resolve_internal_value<T: DeserializeOwned>(
    reference: &InternalRef,
) -> Result<InternalArg<T>> {
    let manager = global_manager().lock().unwrap();
    manager.resolve(reference)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "Duplicate internal store write at coordinates")]
    fn rejects_duplicate_coordinate_writes() {
        let mut manager = InternalStorageManager::new();
        let coordinates = CfsCoordinates(vec![1, 2, 3]);

        manager.append_serialized_bytes(b"first", coordinates.clone());
        manager.append_serialized_bytes(b"second", coordinates);
    }
}
