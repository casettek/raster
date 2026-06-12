use raster_core::cfs::CfsCoordinates;
use raster_core::coordinate_index::coordinate_index_root;
use raster_core::input::{InternalRef, InternalValue, Op, Schema, SchemaFieldMode, SchemaNode};
use raster_core::transition::{InternalStoreEntry, InternalStoreIndexValue, SerializableFrontier};
use raster_core::{Error, Result};
use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::trace::{
    serializable_frontier_from_trace_frontier, Bytes, TraceTree, TraceTreeFrontier,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::vec::Vec;

use crate::input::{
    subtree_payload_and_root, tree_value_from_serialize, typed_value_from_tree, TreeValue,
};
use crate::Sha256Commitment;

type Anchor = [u8; 32];

#[derive(Debug, Clone)]
enum DraftFieldValue {
    Set(TreeValue),
    Append(Vec<TreeValue>),
}

#[derive(Debug, Clone)]
struct DraftRuntimeState {
    schema: SchemaNode,
    current_root: [u8; 32],
    fields: BTreeMap<String, DraftFieldValue>,
    ops: Vec<Op>,
}

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

fn anchor_for_schema(coordinates: &CfsCoordinates, schema_hash: [u8; 32]) -> Anchor {
    let mut hasher = Sha256::new();
    hasher.update(b"raster.draft.v1");
    hasher.update(raster_core::postcard::to_allocvec(coordinates).unwrap_or_default());
    hasher.update(schema_hash);
    hasher.finalize().into()
}

fn schema_struct_fields(schema: &SchemaNode) -> Result<&[raster_core::input::SchemaField]> {
    match schema {
        SchemaNode::Struct { fields, .. } => Ok(fields.as_slice()),
        _ => Err(Error::Other(
            "Drafts currently support only struct schemas at the root".into(),
        )),
    }
}

fn build_draft_tree(
    schema: &SchemaNode,
    fields: &BTreeMap<String, DraftFieldValue>,
    require_complete: bool,
) -> Result<TreeValue> {
    let schema_fields = schema_struct_fields(schema)?;
    let mut values = Vec::with_capacity(schema_fields.len());
    for field in schema_fields {
        let value = match (field.mode, fields.get(&field.name)) {
            (SchemaFieldMode::SetOnce, Some(DraftFieldValue::Set(value))) => value.clone(),
            (SchemaFieldMode::SetOnce, Some(DraftFieldValue::Append(_))) => {
                return Err(Error::Other(format!(
                    "Draft field '{}' expected a set-once value, found append log",
                    field.name
                )))
            }
            (SchemaFieldMode::SetOnce, None) if require_complete => {
                return Err(Error::Other(format!(
                    "Draft field '{}' must be written before finalize",
                    field.name
                )))
            }
            (SchemaFieldMode::SetOnce, None) => TreeValue::Unit,
            (SchemaFieldMode::AppendOnlyVec, Some(DraftFieldValue::Append(values))) => {
                TreeValue::List(values.clone())
            }
            (SchemaFieldMode::AppendOnlyVec, Some(DraftFieldValue::Set(_))) => {
                return Err(Error::Other(format!(
                    "Draft field '{}' expected an append-only vector, found scalar value",
                    field.name
                )))
            }
            (SchemaFieldMode::AppendOnlyVec, None) => TreeValue::List(Vec::new()),
        };
        values.push((field.name.clone(), value));
    }
    Ok(TreeValue::Struct(values))
}

fn draft_root(
    schema: &SchemaNode,
    fields: &BTreeMap<String, DraftFieldValue>,
    require_complete: bool,
) -> Result<[u8; 32]> {
    let tree = build_draft_tree(schema, fields, require_complete)?;
    let (_, root) = subtree_payload_and_root(&tree)?;
    root.as_slice()
        .try_into()
        .map_err(|_| Error::Other("Draft root must be 32 bytes".into()))
}

fn locate_schema_field<'a>(
    schema: &'a SchemaNode,
    name: &str,
) -> Result<&'a raster_core::input::SchemaField> {
    schema_struct_fields(schema)?
        .iter()
        .find(|field| field.name == name)
        .ok_or_else(|| Error::Other(format!("Unknown draft field '{}'", name)))
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

    pub fn resolve<T: DeserializeOwned>(
        &self,
        reference: &InternalRef,
    ) -> Result<InternalValue<T>> {
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
        Ok(InternalValue::new(
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

#[derive(Debug, Clone)]
struct SequenceFrame {
    coordinates: CfsCoordinates,
    next_child_index: u32,
}

#[derive(Debug, Default, Clone)]
struct SequenceExecutionContext {
    stack: Vec<SequenceFrame>,
}

impl SequenceExecutionContext {
    fn enter_sequence(&mut self) {
        let coordinates = if let Some(parent) = self.stack.last_mut() {
            let mut coordinates = parent.coordinates.clone();
            coordinates.push(parent.next_child_index);
            parent.next_child_index += 1;
            coordinates
        } else {
            CfsCoordinates::new()
        };

        self.stack.push(SequenceFrame {
            coordinates,
            next_child_index: 0,
        });
    }

    fn exit_sequence(&mut self) {
        self.stack
            .pop()
            .expect("Corrupted sequence execution context");
    }

    fn reserve_tile_coordinates(&mut self) -> Result<CfsCoordinates> {
        let frame = self.stack.last_mut().ok_or_else(|| {
            Error::Other("Internal-store writes require active sequence context".into())
        })?;
        let mut coordinates = frame.coordinates.clone();
        coordinates.push(frame.next_child_index);
        frame.next_child_index += 1;
        Ok(coordinates)
    }
}

std::thread_local! {
    static THREAD_INTERNAL_STORAGE: RefCell<InternalStorageManager> =
        RefCell::new(InternalStorageManager::new());
    static THREAD_SEQUENCE_CONTEXT: RefCell<SequenceExecutionContext> =
        RefCell::new(SequenceExecutionContext::default());
    static THREAD_DRAFT_STORAGE: RefCell<BTreeMap<Anchor, DraftRuntimeState>> =
        RefCell::new(BTreeMap::new());
}

fn reset_thread_storage() {
    THREAD_INTERNAL_STORAGE.with(|storage| {
        *storage.borrow_mut() = InternalStorageManager::new();
    });
    THREAD_DRAFT_STORAGE.with(|drafts| {
        drafts.borrow_mut().clear();
    });
}

pub fn enter_sequence_scope(_sequence_id: &str) {
    THREAD_SEQUENCE_CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        if context.stack.is_empty() {
            reset_thread_storage();
        }
        context.enter_sequence();
    });
}

pub fn exit_sequence_scope() {
    THREAD_SEQUENCE_CONTEXT.with(|context| {
        context.borrow_mut().exit_sequence();
    });
}

pub fn global_internal_store_snapshot() -> InternalStoreSnapshot {
    THREAD_INTERNAL_STORAGE.with(|storage| storage.borrow().snapshot())
}

pub fn create_draft<S>() -> Result<(Anchor, [u8; 32])>
where
    S: Schema,
{
    let schema = S::schema();
    let coordinates =
        THREAD_SEQUENCE_CONTEXT.with(|context| context.borrow_mut().reserve_tile_coordinates())?;
    let anchor = anchor_for_schema(&coordinates, S::schema_hash());
    let current_root = draft_root(&schema, &BTreeMap::new(), false)?;
    THREAD_DRAFT_STORAGE.with(|drafts| {
        drafts.borrow_mut().insert(
            anchor,
            DraftRuntimeState {
                schema,
                current_root,
                fields: BTreeMap::new(),
                ops: Vec::new(),
            },
        );
    });
    Ok((anchor, current_root))
}

pub fn apply_draft_set<S, T>(
    anchor: &Anchor,
    expected_root: &[u8; 32],
    field: &str,
    value: &T,
) -> Result<[u8; 32]>
where
    S: Schema,
    T: Serialize,
{
    let tree = tree_value_from_serialize(value)?;
    let value_bytes = raster_core::postcard::to_allocvec(value).map_err(|error| {
        Error::Serialization(format!(
            "Failed to encode draft set op for field '{}': {}",
            field, error
        ))
    })?;
    THREAD_DRAFT_STORAGE.with(|drafts| {
        let mut drafts = drafts.borrow_mut();
        let state = drafts
            .get_mut(anchor)
            .ok_or_else(|| Error::Other("Unknown draft anchor".into()))?;
        if state.current_root != *expected_root {
            return Err(Error::Other(format!(
                "Draft root mismatch for field '{}': expected {:?}, found {:?}",
                field, expected_root, state.current_root
            )));
        }
        let schema_field = locate_schema_field(&state.schema, field)?;
        if schema_field.mode != SchemaFieldMode::SetOnce {
            return Err(Error::Other(format!(
                "Draft field '{}' does not support set; use push",
                field
            )));
        }
        if state.fields.contains_key(field) {
            return Err(Error::Other(format!(
                "Draft field '{}' can only be written once",
                field
            )));
        }
        state
            .fields
            .insert(field.to_string(), DraftFieldValue::Set(tree));
        state.ops.push(Op::Set {
            field: field.to_string(),
            value_bytes,
        });
        state.current_root = draft_root(&state.schema, &state.fields, false)?;
        Ok(state.current_root)
    })
}

pub fn apply_draft_push<S, T>(
    anchor: &Anchor,
    expected_root: &[u8; 32],
    field: &str,
    value: &T,
) -> Result<[u8; 32]>
where
    S: Schema,
    T: Serialize,
{
    let tree = tree_value_from_serialize(value)?;
    let value_bytes = raster_core::postcard::to_allocvec(value).map_err(|error| {
        Error::Serialization(format!(
            "Failed to encode draft push op for field '{}': {}",
            field, error
        ))
    })?;
    THREAD_DRAFT_STORAGE.with(|drafts| {
        let mut drafts = drafts.borrow_mut();
        let state = drafts
            .get_mut(anchor)
            .ok_or_else(|| Error::Other("Unknown draft anchor".into()))?;
        if state.current_root != *expected_root {
            return Err(Error::Other(format!(
                "Draft root mismatch for field '{}': expected {:?}, found {:?}",
                field, expected_root, state.current_root
            )));
        }
        let schema_field = locate_schema_field(&state.schema, field)?;
        if schema_field.mode != SchemaFieldMode::AppendOnlyVec {
            return Err(Error::Other(format!(
                "Draft field '{}' does not support push; use set",
                field
            )));
        }
        match state.fields.entry(field.to_string()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(DraftFieldValue::Append(vec![tree]));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => match entry.get_mut() {
                DraftFieldValue::Append(values) => values.push(tree),
                DraftFieldValue::Set(_) => {
                    return Err(Error::Other(format!(
                        "Draft field '{}' is not appendable",
                        field
                    )))
                }
            },
        }
        state.ops.push(Op::Push {
            field: field.to_string(),
            value_bytes,
        });
        state.current_root = draft_root(&state.schema, &state.fields, false)?;
        Ok(state.current_root)
    })
}

pub fn store_internal_value<T: Serialize>(value: &T) -> Result<InternalRef> {
    let bytes = raster_core::postcard::to_allocvec(value).map_err(|error| {
        Error::Serialization(format!(
            "Failed to serialize internal store object for current sequence step: {}",
            error
        ))
    })?;
    let coordinates =
        THREAD_SEQUENCE_CONTEXT.with(|context| context.borrow_mut().reserve_tile_coordinates())?;
    THREAD_INTERNAL_STORAGE.with(|storage| {
        let write = storage
            .borrow_mut()
            .append_serialized_bytes(&bytes, coordinates.clone());
        Ok(InternalRef::new(coordinates, write.entry.object_commitment))
    })
}

pub fn finalize_draft<S>(anchor: &Anchor, expected_root: &[u8; 32]) -> Result<InternalRef>
where
    S: Schema + DeserializeOwned + Serialize,
{
    let value = THREAD_DRAFT_STORAGE.with(|drafts| {
        let drafts = drafts.borrow();
        let state = drafts
            .get(anchor)
            .ok_or_else(|| Error::Other("Unknown draft anchor".into()))?;
        if state.current_root != *expected_root {
            return Err(Error::Other(format!(
                "Draft root mismatch during finalize: expected {:?}, found {:?}",
                expected_root, state.current_root
            )));
        }
        let tree = build_draft_tree(&state.schema, &state.fields, true)?;
        typed_value_from_tree::<S>(&tree).map_err(|error| {
            Error::Serialization(format!(
                "Failed to materialize finalized draft value: {}",
                error
            ))
        })
    })?;

    let reference = store_internal_value(&value)?;
    THREAD_DRAFT_STORAGE.with(|drafts| {
        drafts.borrow_mut().remove(anchor);
    });
    Ok(reference)
}

pub fn resolve_internal_value<T: DeserializeOwned>(
    reference: &InternalRef,
) -> Result<InternalValue<T>> {
    THREAD_INTERNAL_STORAGE.with(|storage| storage.borrow().resolve(reference))
}

pub fn resolve_internal_ok_value<T: DeserializeOwned>(
    reference: &InternalRef,
) -> Result<InternalValue<T>> {
    let resolved: InternalValue<std::result::Result<T, String>> =
        resolve_internal_value(reference)?;
    let InternalValue {
        reference,
        bytes,
        value,
    } = resolved;
    match value {
        Ok(value) => Ok(InternalValue::new(reference, bytes, value)),
        Err(error) => Err(Error::Other(format!(
            "Stored tile result at coordinates {:?} resolved to error: {}",
            reference.coordinates, error
        ))),
    }
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
