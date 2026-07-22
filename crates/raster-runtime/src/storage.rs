use raster_core::cfs::CfsCoordinates;
use raster_core::coordinate_index::IncrementalCoordinateIndex;
use raster_core::draft::{
    draft_root_from_witness, draft_tree_from_witness, draft_value_from_serialize,
    schema_hash as compute_schema_hash, DraftFieldValue, DraftOp, DraftReplayTransition,
    DraftStateWitness, DraftTransitionWitness,
};
use raster_core::input::{
    ExternalEncoding, Schema, SchemaFieldMode, SchemaNode, SelectionWitness, SelectorPath,
    StorageRef, StorageValue,
};
use raster_core::trace::RasterPayload;
use raster_core::transition::{SerializableFrontier, StorageEntry, StorageIndexValue};
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
use std::sync::Arc;
use std::vec::Vec;

use crate::backing::{
    ObjectBacking, OwnedObject, ReferencedObject, ReferencedSource, ReferencedSourceKind,
};
use crate::input::{
    encode_raster_value, selected_payload_from_raster_location,
    selection_witness_from_raster_selection, tree_value_from_raster_location,
    typed_value_from_tree, TreeValue,
};
use crate::raster_index::RasterIndex;
use crate::source::SourceResolver;
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
pub struct StoredObject {
    pub reference: StorageRef,
    pub log_position: u64,
    pub(crate) backing: ObjectBacking,
}

#[derive(Debug, Clone)]
pub struct StorageSnapshot {
    pub frontier: SerializableFrontier,
    pub root: Vec<u8>,
    pub index_root: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct StorageWriteRecord {
    pub entry: StorageEntry,
    pub log_position: u64,
    pub store_root_before: Vec<u8>,
    pub store_root_after: Vec<u8>,
    pub index_root_before: Vec<u8>,
    pub index_root_after: Vec<u8>,
    pub frontier_after: SerializableFrontier,
}

#[derive(Debug, Clone)]
pub(crate) struct AuthorizedSource {
    pub name: String,
    pub encoding: ExternalEncoding,
    pub commitment: Vec<u8>,
    pub kind: ReferencedSourceKind,
}

#[derive(Debug, Clone)]
pub(crate) struct AuthorizedSourceLoad {
    pub sources: Vec<AuthorizedSource>,
}

#[derive(Clone)]
pub struct StorageManager {
    frontier: TraceTreeFrontier,
    objects: BTreeMap<CfsCoordinates, StoredObject>,
    coordinate_index: IncrementalCoordinateIndex,
    /// Set once (via `start_program`) for programs that declare `main` entry
    /// arguments; `None` otherwise, and never consulted unless a `Referenced`
    /// object actually needs resolving.
    source_resolver: Option<Arc<dyn SourceResolver>>,
}

impl std::fmt::Debug for StorageManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageManager")
            .field("objects", &self.objects)
            .field("has_source_resolver", &self.source_resolver.is_some())
            .finish()
    }
}

fn frontier_root(frontier: &TraceTreeFrontier) -> Vec<u8> {
    TraceTree::from_frontier(1, frontier.clone())
        .root(0)
        .expect("storage root should exist")
        .0
}

pub(crate) fn decode_hex_bytes(input: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 2);
    let chars: Vec<char> = input.chars().collect();
    for pair in chars.chunks(2) {
        if pair.len() != 2 {
            return Err(Error::Serialization("Malformed raster root hex".into()));
        }
        let hi = pair[0]
            .to_digit(16)
            .ok_or_else(|| Error::Serialization("Malformed raster root hex".into()))?;
        let lo = pair[1]
            .to_digit(16)
            .ok_or_else(|| Error::Serialization("Malformed raster root hex".into()))?;
        out.push(((hi << 4) | lo) as u8);
    }
    Ok(out)
}

fn decode_hex_hash(input: &str) -> Result<[u8; 32]> {
    let bytes = decode_hex_bytes(input)?;
    bytes
        .try_into()
        .map_err(|_| Error::Serialization("Malformed raster root hash length".into()))
}

fn raster_payload_for_value<T: Serialize>(value: &T) -> Result<RasterPayload> {
    let (bytes, index_bytes, root_hex) = encode_raster_value(value)?;
    Ok(RasterPayload {
        bytes,
        index_bytes,
        root_hash: decode_hex_hash(&root_hex)?,
    })
}

fn internal_object_commitment(bytes: &[u8], raster: Option<&RasterPayload>) -> Vec<u8> {
    raster
        .map(|payload| payload.root_hash.to_vec())
        .unwrap_or_else(|| Sha256Commitment::from(bytes).into())
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

impl StorageManager {
    pub fn new() -> Self {
        let mut tree = TraceTree::new(1);
        tree.append(Bytes(EMPTY_TRIE_NODES[0].to_vec()));
        let frontier = tree
            .frontier()
            .cloned()
            .expect("storage frontier should exist after seed append");
        Self {
            frontier,
            objects: BTreeMap::new(),
            coordinate_index: IncrementalCoordinateIndex::new(),
            source_resolver: None,
        }
    }

    /// Injects the resolver a `Referenced` object dispatches to. Set once
    /// per runtime — by runtime initialization in production, or
    /// directly by a caller that supplies its own input context (the trace
    /// recorder, tests).
    pub(crate) fn set_source_resolver(&mut self, resolver: Arc<dyn SourceResolver>) {
        self.source_resolver = Some(resolver);
    }

    /// The installed input context, if this runtime has one.
    pub(crate) fn source_resolver(&self) -> Option<Arc<dyn SourceResolver>> {
        self.source_resolver.clone()
    }

    pub fn snapshot(&self) -> StorageSnapshot {
        StorageSnapshot {
            frontier: serializable_frontier_from_trace_frontier(self.frontier.clone()),
            root: self.current_root(),
            index_root: self.current_index_root(),
        }
    }

    pub fn current_root(&self) -> Vec<u8> {
        frontier_root(&self.frontier)
    }

    pub fn current_index_root(&self) -> Vec<u8> {
        self.coordinate_index.root()
    }

    fn append(
        &mut self,
        backing: ObjectBacking,
        object_commitment: Vec<u8>,
        coordinates: CfsCoordinates,
    ) -> StorageWriteRecord {
        assert!(
            !self.coordinate_index.contains_key(&coordinates),
            "Duplicate storage write at coordinates {:?}",
            coordinates
        );

        let store_root_before = self.current_root();
        let index_root_before = self.current_index_root();
        let entry = StorageEntry {
            coordinates: coordinates.clone(),
            object_commitment,
        };
        let leaf_hash: Vec<u8> = Sha256Commitment::from(entry.to_bytes().as_slice()).into();

        self.frontier.append(Bytes(leaf_hash));
        let log_position: u64 = self.frontier.position().into();
        let index_value = StorageIndexValue {
            log_position,
            object_commitment: entry.object_commitment.clone(),
        };
        self.coordinate_index
            .insert(coordinates.clone(), index_value);

        let reference = StorageRef::new(coordinates.clone(), entry.object_commitment.clone());

        self.objects.insert(
            coordinates,
            StoredObject {
                reference,
                log_position,
                backing,
            },
        );

        StorageWriteRecord {
            entry,
            log_position,
            store_root_before,
            store_root_after: self.current_root(),
            index_root_before,
            index_root_after: self.current_index_root(),
            frontier_after: serializable_frontier_from_trace_frontier(self.frontier.clone()),
        }
    }

    pub fn append_serialized_bytes(
        &mut self,
        bytes: &[u8],
        coordinates: CfsCoordinates,
        raster: Option<RasterPayload>,
    ) -> StorageWriteRecord {
        let object_commitment = internal_object_commitment(bytes, raster.as_ref());
        let owned = OwnedObject {
            bytes: bytes.to_vec(),
            raster,
        };
        self.append(ObjectBacking::Owned(owned), object_commitment, coordinates)
    }

    /// Loads an authorized set of named sources as one storage object. Today
    /// this is called only for `main`'s entrypoint binding at coordinate `[0]`.
    pub(crate) fn load_authorized_sources(
        &mut self,
        load: AuthorizedSourceLoad,
        coordinates: CfsCoordinates,
    ) -> StorageWriteRecord {
        let referenced = ReferencedObject {
            sources: load
                .sources
                .into_iter()
                .map(|source| ReferencedSource {
                    name: source.name,
                    commitment: source.commitment,
                    kind: {
                        let _authorized_encoding = source.encoding;
                        source.kind
                    },
                })
                .collect(),
        };
        let combined_root = referenced.combined_root();
        self.append(
            ObjectBacking::Referenced(referenced),
            combined_root,
            coordinates,
        )
    }

    pub fn resolve<T: DeserializeOwned>(&self, reference: &StorageRef) -> Result<StorageValue<T>> {
        let stored = self.verify_reference(reference)?;
        match &stored.backing {
            ObjectBacking::Owned(owned) => {
                let (bytes, selection, value) = owned.resolve_whole::<T>(&reference.coordinates)?;
                Ok(StorageValue::new_with_selection(
                    reference.clone(),
                    bytes,
                    SelectorPath::default(),
                    selection,
                    value,
                ))
            }
            ObjectBacking::Referenced(_) => Err(Error::Other(
                "Referenced object requires a field selector naming a declared entry argument"
                    .into(),
            )),
        }
    }

    fn require_raster<'a>(
        owned: &'a OwnedObject,
        coordinates: &CfsCoordinates,
    ) -> Result<&'a RasterPayload> {
        owned.raster.as_ref().ok_or_else(|| {
            Error::Other(format!(
                "Storage object at coordinates {:?} is missing raster selection metadata",
                coordinates
            ))
        })
    }

    fn verify_reference(&self, reference: &StorageRef) -> Result<&StoredObject> {
        let stored = self.objects.get(&reference.coordinates).ok_or_else(|| {
            Error::Other(format!(
                "Missing storage object at coordinates {:?}",
                reference.coordinates
            ))
        })?;
        if stored.reference.commitment != reference.commitment {
            return Err(Error::Other(format!(
                "Storage commitment mismatch at coordinates {:?}",
                reference.coordinates
            )));
        }
        // A `Referenced` object's commitment was fixed once at bind time
        // from already-authorized manifest metadata — there are no raw
        // bytes here to recompute it from. Only `Owned` objects hold bytes
        // to double-check against.
        if let ObjectBacking::Owned(owned) = &stored.backing {
            let actual_commitment =
                internal_object_commitment(owned.bytes.as_slice(), owned.raster.as_ref());
            if actual_commitment != reference.commitment {
                return Err(Error::Other(format!(
                    "Storage object at coordinates {:?} failed integrity check",
                    reference.coordinates
                )));
            }
        }
        Ok(stored)
    }

    pub fn selection_witness(
        &self,
        reference: &StorageRef,
        selector: &SelectorPath,
    ) -> Result<SelectionWitness> {
        let stored = self.verify_reference(reference)?;
        match &stored.backing {
            ObjectBacking::Owned(owned) => {
                let raster = Self::require_raster(owned, &reference.coordinates)?;
                let index = RasterIndex::from_bytes(&raster.index_bytes)?;
                let selection = index.select(selector)?;
                selection_witness_from_raster_selection(&raster.bytes, selector, selection)
            }
            ObjectBacking::Referenced(referenced) => {
                let resolver = self.source_resolver.as_deref().ok_or_else(|| {
                    Error::Other(
                        "Storage has a referenced object but no source resolver configured".into(),
                    )
                })?;
                referenced.selection_witness(selector, resolver)
            }
        }
    }

    pub fn select<T: DeserializeOwned>(
        &self,
        reference: &StorageRef,
        selector: &SelectorPath,
    ) -> Result<StorageValue<T>> {
        let stored = self.verify_reference(reference)?;
        match &stored.backing {
            ObjectBacking::Owned(owned) => {
                let raster = Self::require_raster(owned, &reference.coordinates)?;
                let index = RasterIndex::from_bytes(&raster.index_bytes)?;
                let selection = index.locate(selector)?;
                let tree = tree_value_from_raster_location(&index, &raster.bytes, &selection)?;
                let selected =
                    selected_payload_from_raster_location(&raster.bytes, selector, selection)?;
                if selected.commitment.source_root_hash.to_vec() != reference.commitment {
                    return Err(Error::Other(format!(
                        "Storage selection root mismatch at coordinates {:?}",
                        reference.coordinates
                    )));
                }
                let value = typed_value_from_tree(&tree)?;
                Ok(StorageValue::new_with_selection(
                    reference.clone(),
                    selected.bytes,
                    selector.clone(),
                    selected.commitment,
                    value,
                ))
            }
            ObjectBacking::Referenced(referenced) => {
                let resolver = self.source_resolver.as_deref().ok_or_else(|| {
                    Error::Other(
                        "Storage has a referenced object but no source resolver configured".into(),
                    )
                })?;
                let (tree, selected) = referenced.select(selector, resolver)?;
                let value = typed_value_from_tree::<T>(&tree)?;
                Ok(StorageValue::new_with_selection(
                    reference.clone(),
                    selected.bytes,
                    selector.clone(),
                    selected.commitment,
                    value,
                ))
            }
        }
    }
}

impl Default for StorageManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
struct SequenceFrame {
    coordinates: CfsCoordinates,
    next_child_index: u32,
    next_synthetic_index: u32,
}

#[derive(Debug, Clone)]
struct RecurFrame {
    site_coordinates: CfsCoordinates,
    next_iteration_index: u32,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct SequenceExecutionContext {
    stack: Vec<SequenceFrame>,
    recur_stack: Vec<RecurFrame>,
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
            next_synthetic_index: 0,
        });
    }

    fn exit_sequence(&mut self) {
        assert!(
            self.recur_stack.is_empty(),
            "Cannot exit a sequence while a recur site is still active"
        );
        self.stack
            .pop()
            .expect("Corrupted sequence execution context");
    }

    fn enter_recur_site(&mut self) -> Result<()> {
        let frame = self.stack.last_mut().ok_or_else(|| {
            Error::Other("Recursive execution requires active sequence context".into())
        })?;
        let mut site_coordinates = frame.coordinates.clone();
        site_coordinates.push(frame.next_child_index);
        frame.next_child_index += 1;
        self.recur_stack.push(RecurFrame {
            site_coordinates,
            next_iteration_index: 0,
        });
        Ok(())
    }

    fn enter_recur_sequence_iteration(&mut self) -> Result<()> {
        let recur_frame = self.recur_stack.last_mut().ok_or_else(|| {
            Error::Other("Recursive sequence iteration requires active recur site context".into())
        })?;
        let mut coordinates = recur_frame.site_coordinates.clone();
        coordinates.push(recur_frame.next_iteration_index);
        recur_frame.next_iteration_index += 1;
        self.stack.push(SequenceFrame {
            coordinates,
            next_child_index: 0,
            next_synthetic_index: 0,
        });
        Ok(())
    }

    fn exit_recur_sequence_iteration(&mut self) {
        self.stack
            .pop()
            .expect("Corrupted recur sequence iteration context");
    }

    fn exit_recur_site(&mut self) {
        self.recur_stack
            .pop()
            .expect("Corrupted recur execution context");
    }

    /// The coordinates of the current sequence frame itself (`[]` for
    /// `main`), where the program's `ProgramStart` step loads `main`'s entry
    /// arguments — as opposed to a reserved child coordinate. Unlike
    /// [`reserve_execution_coordinates`], this claims no child slot, so it
    /// must only be used once, before any child of the frame is reserved.
    pub(crate) fn sequence_root_coordinates(&self) -> Result<CfsCoordinates> {
        self.stack
            .last()
            .map(|frame| frame.coordinates.clone())
            .ok_or_else(|| {
                Error::Other("Entry-argument binding requires active sequence context".into())
            })
    }

    pub(crate) fn reserve_execution_coordinates(&mut self) -> Result<CfsCoordinates> {
        if let Some(recur_frame) = self.recur_stack.last_mut() {
            let mut coordinates = recur_frame.site_coordinates.clone();
            coordinates.push(recur_frame.next_iteration_index);
            recur_frame.next_iteration_index += 1;
            return Ok(coordinates);
        }

        let frame = self
            .stack
            .last_mut()
            .ok_or_else(|| Error::Other("Storage writes require active sequence context".into()))?;
        let mut coordinates = frame.coordinates.clone();
        coordinates.push(frame.next_child_index);
        frame.next_child_index += 1;
        Ok(coordinates)
    }

    fn reserve_synthetic_coordinates(&mut self) -> Result<CfsCoordinates> {
        let profiling_enabled = crate::profiling::profiling_enabled();
        let synthetic_coordinate_alloc_start = profiling_enabled.then(std::time::Instant::now);
        let should_record_sequence_overhead =
            THREAD_ACTIVE_EXECUTION_COORDINATES.with(|stack| stack.borrow().is_empty());
        let synthetic_index = {
            let frame = self.stack.last_mut().ok_or_else(|| {
                Error::Other("Storage writes require active sequence context".into())
            })?;
            let synthetic_index = frame.next_synthetic_index;
            frame.next_synthetic_index += 1;
            synthetic_index
        };

        let mut coordinates = if let Some(active_coordinates) =
            THREAD_ACTIVE_EXECUTION_COORDINATES.with(|stack| stack.borrow().last().cloned())
        {
            active_coordinates
        } else if let Some(recur_frame) = self.recur_stack.last() {
            recur_frame.site_coordinates.clone()
        } else {
            self.stack
                .last()
                .ok_or_else(|| {
                    Error::Other("Storage writes require active sequence context".into())
                })?
                .coordinates
                .clone()
        };
        coordinates.push(u32::MAX);
        coordinates.push(synthetic_index);
        if should_record_sequence_overhead {
            if let Some(start) = synthetic_coordinate_alloc_start {
                let duration_ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
                crate::profiling::record_sequence_synthetic_coordinate_alloc(duration_ns);
            }
        }
        Ok(coordinates)
    }
}

std::thread_local! {
    pub(crate) static THREAD_STORAGE: RefCell<StorageManager> =
        RefCell::new(StorageManager::new());
    pub(crate) static THREAD_SEQUENCE_CONTEXT: RefCell<SequenceExecutionContext> =
        RefCell::new(SequenceExecutionContext::default());
    static THREAD_ACTIVE_EXECUTION_COORDINATES: RefCell<Vec<CfsCoordinates>> = RefCell::new(Vec::new());
    static THREAD_PENDING_OUTPUT_COORDINATES: RefCell<Option<CfsCoordinates>> = const { RefCell::new(None) };
    static THREAD_PENDING_OUTPUT_ENCODING: RefCell<Option<PendingOutputEncoding>> = const { RefCell::new(None) };
    static THREAD_DRAFT_STORAGE: RefCell<BTreeMap<Anchor, DraftRuntimeState>> =
        RefCell::new(BTreeMap::new());
}

fn reset_thread_storage() {
    THREAD_STORAGE.with(|storage| {
        let mut storage = storage.borrow_mut();
        // Where the process's inputs come from is a property of how it was
        // started, not of the execution being reset: it is installed once by
        // `init`, before any sequence runs, and must outlive the store it was
        // installed into.
        let source_resolver = storage.source_resolver();
        *storage = StorageManager::new();
        if let Some(resolver) = source_resolver {
            storage.set_source_resolver(resolver);
        }
    });
    THREAD_ACTIVE_EXECUTION_COORDINATES.with(|coordinates| {
        coordinates.borrow_mut().clear();
    });
    THREAD_PENDING_OUTPUT_COORDINATES.with(|coordinates| {
        coordinates.borrow_mut().take();
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

pub fn enter_recur_site_scope() -> Result<()> {
    THREAD_SEQUENCE_CONTEXT.with(|context| context.borrow_mut().enter_recur_site())
}

pub fn exit_recur_site_scope() {
    THREAD_SEQUENCE_CONTEXT.with(|context| {
        context.borrow_mut().exit_recur_site();
    });
}

pub fn enter_recur_sequence_iteration_scope() -> Result<()> {
    THREAD_SEQUENCE_CONTEXT.with(|context| context.borrow_mut().enter_recur_sequence_iteration())
}

pub fn exit_recur_sequence_iteration_scope() {
    THREAD_SEQUENCE_CONTEXT.with(|context| {
        context.borrow_mut().exit_recur_sequence_iteration();
    });
}

pub fn global_storage_snapshot() -> StorageSnapshot {
    THREAD_STORAGE.with(|storage| storage.borrow().snapshot())
}

pub fn create_draft<S>() -> Result<(Anchor, [u8; 32])>
where
    S: Schema,
{
    let schema = S::schema();
    let coordinates = THREAD_SEQUENCE_CONTEXT
        .with(|context| context.borrow_mut().reserve_synthetic_coordinates())?;
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

fn store_value_at_coordinates<T: Serialize>(
    value: &T,
    coordinates: CfsCoordinates,
) -> Result<StorageRef> {
    let bytes = raster_core::postcard::to_allocvec(value).map_err(|error| {
        Error::Serialization(format!(
            "Failed to serialize storage object for current sequence step: {}",
            error
        ))
    })?;
    let raster_payload = Some(raster_payload_for_value(value)?);
    THREAD_STORAGE.with(|storage| {
        let write = storage.borrow_mut().append_serialized_bytes(
            &bytes,
            coordinates.clone(),
            raster_payload,
        );
        Ok(StorageRef::new(coordinates, write.entry.object_commitment))
    })
}

pub fn store_value<T: Serialize>(value: &T) -> Result<StorageRef> {
    let coordinates = THREAD_SEQUENCE_CONTEXT
        .with(|context| context.borrow_mut().reserve_synthetic_coordinates())?;
    store_value_at_coordinates(value, coordinates)
}

/// One tile output's already-computed encoding, stashed by the tile wrapper
/// (which encodes the value for the trace payload) for the immediately
/// following `store_execution_output_value` in the caller's bind. The store
/// only reuses `raster` when both its freshly serialized bytes and the
/// stored value's type match the stash, so a broken pairing degrades to
/// re-encoding, never to a wrong commitment. The type must participate
/// because postcard is not self-describing: a transparent wrapper (e.g. a
/// recur control/state) serializes to bytes identical to its inner value
/// while its raster tree — and therefore its index and root — differs.
pub struct PendingOutputEncoding {
    pub type_name: &'static str,
    pub bytes: Vec<u8>,
    pub raster: RasterPayload,
}

pub fn stash_pending_output_encoding(
    type_name: &'static str,
    bytes: Vec<u8>,
    raster: RasterPayload,
) {
    THREAD_PENDING_OUTPUT_ENCODING.with(|pending| {
        *pending.borrow_mut() = Some(PendingOutputEncoding {
            type_name,
            bytes,
            raster,
        });
    });
}

pub fn store_execution_output_value<T: Serialize>(value: &T) -> Result<StorageRef> {
    let coordinates = THREAD_PENDING_OUTPUT_COORDINATES
        .with(|coordinates| coordinates.borrow_mut().take())
        .or_else(current_recur_site_coordinates)
        .ok_or_else(|| {
            Error::Other("Missing pending execution output coordinates for tile output".into())
        })?;
    let bytes = raster_core::postcard::to_allocvec(value).map_err(|error| {
        Error::Serialization(format!(
            "Failed to serialize storage object for current sequence step: {}",
            error
        ))
    })?;
    let stashed = THREAD_PENDING_OUTPUT_ENCODING.with(|pending| pending.borrow_mut().take());
    let raster_payload = match stashed {
        Some(encoding)
            if encoding.type_name == core::any::type_name::<T>() && encoding.bytes == bytes =>
        {
            encoding.raster
        }
        _ => raster_payload_for_value(value)?,
    };
    THREAD_STORAGE.with(|storage| {
        let write = storage.borrow_mut().append_serialized_bytes(
            &bytes,
            coordinates.clone(),
            Some(raster_payload),
        );
        Ok(StorageRef::new(coordinates, write.entry.object_commitment))
    })
}

fn current_recur_site_coordinates() -> Option<CfsCoordinates> {
    THREAD_SEQUENCE_CONTEXT.with(|context| {
        context
            .borrow()
            .recur_stack
            .last()
            .map(|frame| frame.site_coordinates.clone())
    })
}

#[derive(Debug, Clone)]
pub struct TileExecutionScopeGuard {
    coordinates: CfsCoordinates,
}

impl TileExecutionScopeGuard {
    pub fn enter() -> Result<Self> {
        let coordinates = THREAD_SEQUENCE_CONTEXT
            .with(|context| context.borrow_mut().reserve_execution_coordinates())?;
        THREAD_ACTIVE_EXECUTION_COORDINATES.with(|active| {
            active.borrow_mut().push(coordinates.clone());
        });
        THREAD_PENDING_OUTPUT_COORDINATES.with(|pending| {
            pending.borrow_mut().take();
        });
        Ok(Self { coordinates })
    }

    pub fn coordinates(&self) -> &CfsCoordinates {
        &self.coordinates
    }
}

impl Drop for TileExecutionScopeGuard {
    fn drop(&mut self) {
        THREAD_ACTIVE_EXECUTION_COORDINATES.with(|active| {
            let mut active = active.borrow_mut();
            let popped = active
                .pop()
                .expect("Corrupted active execution coordinate stack");
            assert_eq!(
                popped, self.coordinates,
                "Mismatched execution coordinate scope teardown"
            );
        });
    }
}

pub fn publish_pending_output_coordinates(coordinates: CfsCoordinates) {
    THREAD_PENDING_OUTPUT_COORDINATES.with(|pending| {
        *pending.borrow_mut() = Some(coordinates);
    });
}

pub fn finalize_draft<S>(anchor: &Anchor, expected_root: &[u8; 32]) -> Result<StorageRef>
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

    let reference = if let Some(coordinates) = current_recur_site_coordinates() {
        store_value_at_coordinates(&value, coordinates)?
    } else {
        store_value(&value)?
    };
    Ok(reference)
}

pub fn finalize_empty_draft<S>(anchor: &Anchor, expected_root: &[u8; 32]) -> Result<StorageRef>
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

    let reference = if let Some(coordinates) = current_recur_site_coordinates() {
        store_value_at_coordinates(&value, coordinates)?
    } else {
        store_value(&value)?
    };
    Ok(reference)
}

pub fn resolve_storage_value<T: DeserializeOwned>(
    reference: &StorageRef,
) -> Result<StorageValue<T>> {
    THREAD_STORAGE.with(|storage| storage.borrow().resolve(reference))
}

pub fn select_stored_value<T: DeserializeOwned>(
    reference: &StorageRef,
    selector: &SelectorPath,
) -> Result<StorageValue<T>> {
    THREAD_STORAGE.with(|storage| storage.borrow().select(reference, selector))
}

pub fn resolve_storage_ok_value<T: DeserializeOwned>(
    reference: &StorageRef,
) -> Result<StorageValue<T>> {
    let resolved: StorageValue<std::result::Result<T, String>> = resolve_storage_value(reference)?;
    let StorageValue {
        reference,
        bytes,
        selector,
        selection,
        value,
    } = resolved;
    match value {
        Ok(value) => Ok(StorageValue::new_with_selection(
            reference, bytes, selector, selection, value,
        )),
        Err(error) => Err(Error::Other(format!(
            "Stored tile result at coordinates {:?} resolved to error: {}",
            reference.coordinates, error
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use raster_core::input::{
        struct_commitments_root, ExternalEncoding, SchemaField, SchemaNode, Selectable,
    };
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
    #[should_panic(expected = "Duplicate storage write at coordinates")]
    fn rejects_duplicate_coordinate_writes() {
        let mut manager = StorageManager::new();
        let coordinates = CfsCoordinates(vec![1, 2, 3]);

        manager.append_serialized_bytes(b"first", coordinates.clone(), None);
        manager.append_serialized_bytes(b"second", coordinates, None);
    }

    #[test]
    fn authorized_source_load_commits_to_declared_sources_in_order() {
        let mut manager = StorageManager::new();
        let alpha_commitment = vec![1; 32];
        let beta_commitment = vec![2; 32];
        let coordinates = CfsCoordinates(vec![0]);

        let write = manager.load_authorized_sources(
            AuthorizedSourceLoad {
                sources: vec![
                    AuthorizedSource {
                        name: "alpha".into(),
                        encoding: ExternalEncoding::Raster,
                        commitment: alpha_commitment.clone(),
                        kind: ReferencedSourceKind::Raster,
                    },
                    AuthorizedSource {
                        name: "beta".into(),
                        encoding: ExternalEncoding::Raster,
                        commitment: beta_commitment.clone(),
                        kind: ReferencedSourceKind::Raster,
                    },
                ],
            },
            coordinates.clone(),
        );

        let expected = struct_commitments_root([
            ("alpha", alpha_commitment.as_slice()),
            ("beta", beta_commitment.as_slice()),
        ])
        .to_vec();

        assert_eq!(write.entry.coordinates, coordinates);
        assert_eq!(write.entry.object_commitment, expected);
    }

    #[test]
    fn stored_internal_reference_commits_to_raster_root() {
        reset_thread_storage();
        let _guard = SequenceScopeGuard::enter("stored_internal_reference_commits_to_raster_root");
        let reference = store_value(&vec!["alpha".to_string(), "beta".to_string()])
            .expect("value should store");
        let resolved: StorageValue<Vec<String>> =
            resolve_storage_value(&reference).expect("value should resolve");

        assert_eq!(reference.commitment, resolved.selection.source_root_hash);
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
