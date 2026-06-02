use raster_core::input::{InternalArg, InternalRef};
use raster_core::transition::{InternalStoreWriteWitness, SerializableFrontier};
use raster_core::{Error, Result};
use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::trace::{
    serializable_frontier_from_trace_frontier, Bytes, TraceTree,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::vec::Vec;

#[derive(Debug, Clone)]
pub struct StoredInternalObject {
    pub reference: InternalRef,
    pub bytes: Vec<u8>,
    pub store_root: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct InternalStoreSnapshot {
    pub frontier: SerializableFrontier,
    pub root: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct InternalWriteRecord {
    pub write_index: u64,
    pub object_commitment: Vec<u8>,
    pub store_root_before: Vec<u8>,
    pub store_root_after: Vec<u8>,
    pub frontier_after: SerializableFrontier,
}

#[derive(Debug, Clone)]
pub struct InternalStorageManager {
    next_write_index: u64,
    frontier: raster_prover::trace::TraceTreeFrontier,
    objects: BTreeMap<u64, StoredInternalObject>,
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
            next_write_index: 0,
            frontier,
            objects: BTreeMap::new(),
        }
    }

    pub fn snapshot(&self) -> InternalStoreSnapshot {
        InternalStoreSnapshot {
            frontier: serializable_frontier_from_trace_frontier(self.frontier.clone()),
            root: self.current_root(),
        }
    }

    pub fn current_root(&self) -> Vec<u8> {
        TraceTree::from_frontier(1, self.frontier.clone())
            .root(0)
            .expect("internal store root should exist")
            .0
    }

    pub fn append_serialized_bytes(&mut self, bytes: &[u8]) -> InternalWriteRecord {
        let store_root_before = self.current_root();
        let object_commitment = sha256_bytes(bytes);
        let write_index = self.next_write_index;
        self.next_write_index += 1;

        self.frontier.append(Bytes(object_commitment.clone()));
        let store_root_after = self.current_root();
        let reference = InternalRef::new(write_index, object_commitment.clone());
        self.objects.insert(
            write_index,
            StoredInternalObject {
                reference,
                bytes: bytes.to_vec(),
                store_root: store_root_after.clone(),
            },
        );

        InternalWriteRecord {
            write_index,
            object_commitment,
            store_root_before,
            store_root_after,
            frontier_after: serializable_frontier_from_trace_frontier(self.frontier.clone()),
        }
    }

    pub fn resolve<T: DeserializeOwned>(&self, reference: &InternalRef) -> Result<InternalArg<T>> {
        let stored = self.objects.get(&reference.write_index).ok_or_else(|| {
            Error::Other(format!(
                "Missing internal store object at write index {}",
                reference.write_index
            ))
        })?;
        if stored.reference.commitment != reference.commitment {
            return Err(Error::Other(format!(
                "Internal store commitment mismatch at write index {}",
                reference.write_index
            )));
        }
        let actual_commitment = sha256_bytes(&stored.bytes);
        if actual_commitment != reference.commitment {
            return Err(Error::Other(format!(
                "Internal store object {} failed integrity check",
                reference.write_index
            )));
        }
        let value = raster_core::postcard::from_bytes(&stored.bytes).map_err(|e| {
            Error::Serialization(format!(
                "Failed to deserialize internal store object {}: {}",
                reference.write_index, e
            ))
        })?;
        Ok(InternalArg::new(
            reference.clone(),
            stored.store_root.clone(),
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

pub fn internal_write_witness(record: &InternalWriteRecord) -> InternalStoreWriteWitness {
    InternalStoreWriteWitness {
        write_index: record.write_index,
        object_commitment: record.object_commitment.clone(),
    }
}

pub fn sha256_bytes(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

static GLOBAL_INTERNAL_STORAGE: OnceLock<Mutex<InternalStorageManager>> = OnceLock::new();

fn global_manager() -> &'static Mutex<InternalStorageManager> {
    GLOBAL_INTERNAL_STORAGE.get_or_init(|| Mutex::new(InternalStorageManager::new()))
}

pub fn global_internal_store_snapshot() -> InternalStoreSnapshot {
    global_manager().lock().unwrap().snapshot()
}

pub fn store_internal_value<T: Serialize>(value: &T) -> Result<InternalRef> {
    let bytes = raster_core::postcard::to_allocvec(value).map_err(|e| {
        Error::Serialization(format!("Failed to serialize internal store value: {}", e))
    })?;
    let mut manager = global_manager().lock().unwrap();
    let record = manager.append_serialized_bytes(&bytes);
    Ok(InternalRef::new(record.write_index, record.object_commitment))
}

pub fn resolve_internal_value<T: DeserializeOwned>(reference: &InternalRef) -> Result<InternalArg<T>> {
    let manager = global_manager().lock().unwrap();
    manager.resolve(reference)
}

