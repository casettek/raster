use raster_core::cfs::CfsCoordinates;
use raster_core::coordinate_index::coordinate_index_root;
use raster_core::draft::{
    draft_root_from_witness, draft_tree_from_witness, draft_value_from_serialize,
    schema_hash as compute_schema_hash, DraftFieldValue, DraftOp, DraftReplayTransition,
    DraftStateWitness, DraftTransitionWitness,
};
use raster_core::input::{InternalRef, InternalValue, Schema, SchemaFieldMode, SchemaNode};
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

use crate::input::{typed_value_from_tree, TreeValue};
use crate::Sha256Commitment;

type Anchor = [u8; 32];

#[derive(Debug, Clone)]
struct DraftRuntimeState {
    schema: SchemaNode,
    current_root: [u8; 32],
    fields: BTreeMap<String, DraftFieldValue>,
    ops: Vec<DraftOp>,
}

#[derive(Debug, Clone)]
pub struct DraftCaptureSnapshot {
    anchor: Anchor,
    schema_hash: [u8; 32],
    root_before: [u8; 32],
    pre_state: DraftStateWitness,
    op_count_before: usize,
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

fn runtime_tree_value(value: &raster_core::draft::DraftValue) -> TreeValue {
    match value {
        raster_core::draft::DraftValue::Unit => TreeValue::Unit,
        raster_core::draft::DraftValue::Bool(value) => TreeValue::Bool(*value),
        raster_core::draft::DraftValue::U8(value) => TreeValue::U8(*value),
        raster_core::draft::DraftValue::U16(value) => TreeValue::U16(*value),
        raster_core::draft::DraftValue::U32(value) => TreeValue::U32(*value),
        raster_core::draft::DraftValue::U64(value) => TreeValue::U64(*value),
        raster_core::draft::DraftValue::I8(value) => TreeValue::I8(*value),
        raster_core::draft::DraftValue::I16(value) => TreeValue::I16(*value),
        raster_core::draft::DraftValue::I32(value) => TreeValue::I32(*value),
        raster_core::draft::DraftValue::I64(value) => TreeValue::I64(*value),
        raster_core::draft::DraftValue::String(value) => TreeValue::String(value.clone()),
        raster_core::draft::DraftValue::Struct(fields) => TreeValue::Struct(
            fields
                .iter()
                .map(|(name, child)| (name.clone(), runtime_tree_value(child)))
                .collect(),
        ),
        raster_core::draft::DraftValue::List(values) => {
            TreeValue::List(values.iter().map(runtime_tree_value).collect())
        }
        raster_core::draft::DraftValue::Map(entries) => TreeValue::Map(
            entries
                .iter()
                .map(|(key, value)| (runtime_tree_value(key), runtime_tree_value(value)))
                .collect(),
        ),
        raster_core::draft::DraftValue::EnumUnit(variant) => TreeValue::EnumUnit(variant.clone()),
        raster_core::draft::DraftValue::EnumNewtype(variant, value) => {
            TreeValue::EnumNewtype(variant.clone(), Box::new(runtime_tree_value(value)))
        }
        raster_core::draft::DraftValue::EnumTuple(variant, values) => TreeValue::EnumTuple(
            variant.clone(),
            values.iter().map(runtime_tree_value).collect(),
        ),
        raster_core::draft::DraftValue::EnumStruct(variant, fields) => TreeValue::EnumStruct(
            variant.clone(),
            fields
                .iter()
                .map(|(name, child)| (name.clone(), runtime_tree_value(child)))
                .collect(),
        ),
    }
}

fn build_draft_tree(
    schema: &SchemaNode,
    fields: &BTreeMap<String, DraftFieldValue>,
    require_complete: bool,
) -> Result<TreeValue> {
    let tree = draft_tree_from_witness(schema, fields, require_complete)?;
    Ok(runtime_tree_value(&tree))
}

fn draft_root(
    schema: &SchemaNode,
    fields: &BTreeMap<String, DraftFieldValue>,
    require_complete: bool,
) -> Result<[u8; 32]> {
    draft_root_from_witness(schema, fields, require_complete)
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

fn first_unset_set_once_field<'a>(
    schema: &'a SchemaNode,
    fields: &BTreeMap<String, DraftFieldValue>,
) -> Result<Option<&'a str>> {
    for field in schema_struct_fields(schema)? {
        if field.mode == SchemaFieldMode::SetOnce && !fields.contains_key(&field.name) {
            return Ok(Some(field.name.as_str()));
        }
    }
    Ok(None)
}

fn take_draft_state(
    anchor: &Anchor,
    expected_root: &[u8; 32],
    operation: &str,
) -> Result<DraftRuntimeState> {
    THREAD_DRAFT_STORAGE.with(|drafts| {
        let mut drafts = drafts.borrow_mut();
        let state = drafts
            .remove(anchor)
            .ok_or_else(|| Error::Other("Unknown draft anchor".into()))?;
        if state.current_root != *expected_root {
            return Err(Error::Other(format!(
                "Draft root mismatch during {}: expected {:?}, found {:?}",
                operation, expected_root, state.current_root
            )));
        }
        Ok(state)
    })
}

fn draft_state_witness(state: &DraftRuntimeState) -> DraftStateWitness {
    DraftStateWitness {
        schema: state.schema.clone(),
        fields: state
            .fields
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect(),
    }
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

pub fn begin_draft_step_capture<S>(
    anchor: &Anchor,
    expected_root: &[u8; 32],
) -> Result<DraftCaptureSnapshot>
where
    S: Schema,
{
    THREAD_DRAFT_STORAGE.with(|drafts| {
        let drafts = drafts.borrow();
        let state = drafts
            .get(anchor)
            .ok_or_else(|| Error::Other("Unknown draft anchor".into()))?;
        if state.current_root != *expected_root {
            return Err(Error::Other(format!(
                "Draft root mismatch during step capture start: expected {:?}, found {:?}",
                expected_root, state.current_root
            )));
        }
        Ok(DraftCaptureSnapshot {
            anchor: *anchor,
            schema_hash: compute_schema_hash(&state.schema),
            root_before: *expected_root,
            pre_state: draft_state_witness(state),
            op_count_before: state.ops.len(),
        })
    })
}

pub fn finish_draft_step_capture<S>(
    snapshot: DraftCaptureSnapshot,
    expected_root: &[u8; 32],
) -> Result<DraftTransitionWitness>
where
    S: Schema,
{
    let native_transition = THREAD_DRAFT_STORAGE.with(|drafts| {
        let drafts = drafts.borrow();
        let state = drafts
            .get(&snapshot.anchor)
            .ok_or_else(|| Error::Other("Unknown draft anchor".into()))?;
        if state.current_root != *expected_root {
            return Err(Error::Other(format!(
                "Draft root mismatch during step capture finish: expected {:?}, found {:?}",
                expected_root, state.current_root
            )));
        }
        Ok(DraftReplayTransition {
            draft_id: snapshot.anchor,
            schema_hash: snapshot.schema_hash,
            root_before: snapshot.root_before,
            ops: state.ops[snapshot.op_count_before..].to_vec(),
        })
    })?;

    Ok(DraftTransitionWitness {
        pre_state: snapshot.pre_state,
        native_transition: Some(native_transition),
    })
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
    let tree = draft_value_from_serialize(value)?;
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
        state.ops.push(DraftOp::Set {
            field: field.to_string(),
            value: draft_value_from_serialize(value)?,
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
    let tree = draft_value_from_serialize(value)?;
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
        state.ops.push(DraftOp::Push {
            field: field.to_string(),
            value: draft_value_from_serialize(value)?,
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
    let state = take_draft_state(anchor, expected_root, "finalize")?;
    let tree = build_draft_tree(&state.schema, &state.fields, true)?;
    let value = typed_value_from_tree::<S>(&tree).map_err(|error| {
        Error::Serialization(format!(
            "Failed to materialize finalized draft value: {}",
            error
        ))
    })?;

    let reference = store_internal_value(&value)?;
    Ok(reference)
}

pub fn finalize_empty_draft<S>(anchor: &Anchor, expected_root: &[u8; 32]) -> Result<InternalRef>
where
    S: Schema + DeserializeOwned + Serialize,
{
    let state = take_draft_state(anchor, expected_root, "empty finalize")?;
    let tree = build_draft_tree(&state.schema, &state.fields, false)?;
    let value = typed_value_from_tree::<S>(&tree).map_err(|error| {
        if let Ok(Some(field)) = first_unset_set_once_field(&state.schema, &state.fields) {
            return Error::Other(format!(
                "Empty recur input cannot finalize draft '{}': field '{}' was never written and the schema cannot materialize a default value",
                core::any::type_name::<S>(),
                field
            ));
        }
        Error::Serialization(format!(
            "Failed to materialize finalized empty draft value: {}",
            error
        ))
    })?;

    let reference = store_internal_value(&value)?;
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
    use raster_core::input::{SchemaField, SchemaNode, Selectable};
    use serde::{Deserialize, Serialize};

    struct SequenceScopeGuard;

    impl SequenceScopeGuard {
        fn enter(sequence_id: &str) -> Self {
            enter_sequence_scope(sequence_id);
            Self
        }
    }

    impl Drop for SequenceScopeGuard {
        fn drop(&mut self) {
            exit_sequence_scope();
        }
    }

    #[derive(Debug, Deserialize, Serialize)]
    struct RequiredFieldDraft {
        value: u64,
    }

    impl Selectable for RequiredFieldDraft {
        fn schema() -> SchemaNode {
            SchemaNode::Struct {
                type_name: "RequiredFieldDraft".into(),
                fields: vec![SchemaField::new(
                    "value",
                    "value",
                    SchemaNode::Leaf {
                        type_name: "u64".into(),
                    },
                )],
            }
        }
    }

    #[test]
    #[should_panic(expected = "Duplicate internal store write at coordinates")]
    fn rejects_duplicate_coordinate_writes() {
        let mut manager = InternalStorageManager::new();
        let coordinates = CfsCoordinates(vec![1, 2, 3]);

        manager.append_serialized_bytes(b"first", coordinates.clone());
        manager.append_serialized_bytes(b"second", coordinates);
    }

    #[test]
    fn failed_finalize_removes_draft_anchor() {
        let _guard = SequenceScopeGuard::enter("failed_finalize_removes_draft_anchor");
        let (anchor, current_root) =
            create_draft::<RequiredFieldDraft>().expect("draft should be created");

        assert!(THREAD_DRAFT_STORAGE.with(|drafts| drafts.borrow().contains_key(&anchor)));

        let error = finalize_draft::<RequiredFieldDraft>(&anchor, &current_root)
            .unwrap_err()
            .to_string();

        assert!(error.contains("must be written before finalize"));
        assert!(THREAD_DRAFT_STORAGE.with(|drafts| !drafts.borrow().contains_key(&anchor)));
    }
}
