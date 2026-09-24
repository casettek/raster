use raster_core::cfs::{CfsCoordinate, CfsCoordinates, FIRST_COORDINATE};
use raster_core::coordinate_index::IncrementalCoordinateIndex;
use raster_core::draft::{
    draft_root_from_field_roots, draft_tree_from_fields, draft_value_from_serialize,
    draft_value_root, schema_hash as compute_schema_hash, DraftFieldValue, DraftOp,
    DraftReplayTransition, DraftStateWitness, DraftTransitionWitness, DraftValue,
    DraftWitnessField,
};
use raster_core::input::{
    AppendFrontier, AuthenticatedListMetadata, ExternalEncoding, Schema, SchemaFieldMode,
    SchemaNode, SelectionPayloadKind, SelectionWitness, SelectorPath, StorageRef, StorageValue,
};
use raster_core::trace::RasterPayload;
use raster_core::transition::{SerializableFrontier, StorageEntry, StorageIndexValue};
use raster_core::{Error, Result};
use raster_prover::precomputed::EMPTY_TRIE_NODES;
use raster_prover::trace::{
    frontier_root, serializable_frontier_from_trace_frontier, Bytes, TraceTree, TraceTreeFrontier,
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
    encode_raster_value, list_metadata_payload, list_metadata_witness,
    selected_payload_from_raster_location, selection_witness_from_raster_selection,
    tree_value_from_raster_location, typed_value_from_tree, TreeValue,
};
use crate::raster_index::RasterIndex;
use crate::source::SourceResolver;
use crate::Sha256Commitment;

type Anchor = [u8; 32];

/// One draft field as the runtime holds it: the real value, plus the digest
/// state needed to move the draft root forward without re-reading the value.
///
/// Keeping both is what makes a push O(log N) here as well as in the guest. The
/// values are still the truth — `finalize` materializes the whole object from
/// them — but they are no longer walked on every op. See
/// `docs/proposals/incremental-draft-witness.md`.
#[derive(Debug, Clone)]
enum DraftFieldRuntime {
    Set {
        value: DraftValue,
        root: [u8; 32],
    },
    Append {
        values: Vec<DraftValue>,
        frontier: AppendFrontier,
    },
}

impl DraftFieldRuntime {
    fn root(&self) -> Result<[u8; 32]> {
        match self {
            Self::Set { root, .. } => Ok(*root),
            Self::Append { frontier, .. } => frontier
                .root()
                .ok_or_else(|| Error::Other("Draft append frontier is malformed".into())),
        }
    }

    /// The runtime's own representation, rebuilt for the finalize path.
    fn field_value(&self) -> DraftFieldValue {
        match self {
            Self::Set { value, .. } => DraftFieldValue::Set(value.clone()),
            Self::Append { values, .. } => DraftFieldValue::Append(values.clone()),
        }
    }

    /// What crosses into the trace: a frontier, never the accumulated log.
    fn witness_field(&self) -> DraftWitnessField {
        match self {
            Self::Set { value, .. } => DraftWitnessField::Set(value.clone()),
            Self::Append { frontier, .. } => DraftWitnessField::Append(frontier.clone()),
        }
    }
}

#[derive(Debug, Clone)]
struct DraftRuntimeState {
    schema: SchemaNode,
    current_root: [u8; 32],
    fields: BTreeMap<String, DraftFieldRuntime>,
    ops: Vec<DraftOp>,
}

impl DraftRuntimeState {
    /// Recompose the draft root from the per-field roots the fields already
    /// hold — O(#fields), with no element ever touched.
    fn recompose_root(&self) -> Result<[u8; 32]> {
        let mut roots = BTreeMap::new();
        for (name, field) in &self.fields {
            roots.insert(name.clone(), field.root()?);
        }
        draft_root_from_field_roots(&self.schema, &roots)
    }

    fn field_values(&self) -> BTreeMap<String, DraftFieldValue> {
        self.fields
            .iter()
            .map(|(name, field)| (name.clone(), field.field_value()))
            .collect()
    }
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

/// The objects a running program wrote, keyed by the coordinates that address
/// them, plus the resolver a `Referenced` object dispatches to.
///
/// This is what *executing* a program needs and no more: a sequence binds
/// references rather than values, so a later tile's `call!` has to read back
/// the bytes an earlier one produced. It authenticates nothing — no frontier,
/// no coordinate index, no roots. [`AuthenticatedObjectStore`] wraps it with
/// those, for the one role that reads them.
/// See `docs/proposals/storage-role-split.md`.
#[derive(Clone)]
pub struct ObjectStore {
    objects: BTreeMap<CfsCoordinates, StoredObject>,
    /// Set once (via `start_program`) for programs that declare `main` entry
    /// arguments; `None` otherwise, and never consulted unless a `Referenced`
    /// object actually needs resolving.
    source_resolver: Option<Arc<dyn SourceResolver>>,
}

impl std::fmt::Debug for ObjectStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectStore")
            .field("objects", &self.objects)
            .field("has_source_resolver", &self.source_resolver.is_some())
            .finish()
    }
}

/// An [`ObjectStore`] plus the structures that make its contents provable: an
/// append-only log of `(coordinates, object_commitment)` entries and a
/// coordinate-keyed Merkle index, whose roots every write reports.
///
/// The trace recorder holds one of these, because it is the only role that
/// reads those roots — it commits them per step and builds the membership and
/// selection witnesses the guest checks against them.
#[derive(Clone)]
pub struct AuthenticatedObjectStore {
    objects: ObjectStore,
    frontier: TraceTreeFrontier,
    /// The storage root as of the last mutation.
    ///
    /// Recomputing `frontier_root` per read is not free even after the direct
    /// ommer fold: it hashes the whole right spine. It was measured at ~110 µs
    /// per `append` back when it rebuilt a `TraceTree` from a cloned frontier —
    /// 32–55% of an append's whole cost, and paid by every storage write in
    /// every program. `store_root_before` is by construction the previous
    /// append's `store_root_after`, so one recompute per mutation is all that
    /// is ever needed.
    /// See `docs/proposals/storage-write-cost.md`.
    cached_root: Vec<u8>,
    coordinate_index: IncrementalCoordinateIndex,
}

impl std::fmt::Debug for AuthenticatedObjectStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthenticatedObjectStore")
            .field("objects", &self.objects)
            .finish()
    }
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

/// The backing and commitment for a value the program produced. Shared by both
/// stores so the commitment rule has exactly one definition.
fn owned_backing(bytes: &[u8], raster: Option<RasterPayload>) -> (ObjectBacking, Vec<u8>) {
    let object_commitment = internal_object_commitment(bytes, raster.as_ref());
    let owned = OwnedObject {
        bytes: bytes.to_vec(),
        raster,
    };
    (ObjectBacking::Owned(owned), object_commitment)
}

/// The backing and commitment for an authorized set of named sources.
fn referenced_backing(load: AuthorizedSourceLoad) -> (ObjectBacking, Vec<u8>) {
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
    (ObjectBacking::Referenced(referenced), combined_root)
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
        // Draft list fields are `List<T>` append targets (never `Block`), so they
        // finalize to `(root, len)` handles — matching how a `List` field encodes
        // through `encode_raster_value`. The list Merkle root is unchanged, so the
        // finalized root still equals the incrementally-tracked draft root.
        raster_core::draft::DraftValue::List(values) => {
            TreeValue::ListHandle(values.iter().map(runtime_tree_value).collect())
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
        raster_core::draft::DraftValue::BytesPage {
            index,
            offset,
            len,
            bytes,
        } => TreeValue::BytesPage {
            index: *index,
            offset: *offset,
            len: *len,
            bytes: bytes.clone(),
        },
    }
}

fn build_draft_tree(
    schema: &SchemaNode,
    fields: &BTreeMap<String, DraftFieldValue>,
    require_complete: bool,
) -> Result<TreeValue> {
    let tree = draft_tree_from_fields(schema, fields, require_complete)?;
    Ok(runtime_tree_value(&tree))
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
    fields: &BTreeMap<String, DraftFieldRuntime>,
) -> Result<Option<&'a str>> {
    for field in schema_struct_fields(schema)? {
        if field.mode == SchemaFieldMode::SetOnce && !fields.contains_key(&field.name) {
            return Ok(Some(field.name.as_str()));
        }
    }
    Ok(None)
}

/// Stand-in root for a draft in an unauthenticated run.
///
/// The root is a commitment, and an unauthenticated run computes none — but
/// `Draft<S>` still threads a `[u8; 32]` from op to op, and the trace-facing
/// mismatch checks compare against it. Rather than make the field optional
/// through every signature, the mode uses one fixed value and skips the
/// comparisons. See `docs/proposals/unauthenticated-execution.md` §7.
const UNAUTHENTICATED_DRAFT_ROOT: [u8; 32] = [0u8; 32];

/// Whether draft operations should compute and check commitments.
fn drafts_are_authenticated() -> bool {
    crate::auth::auth_mode().is_authenticated()
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
        if drafts_are_authenticated() && state.current_root != *expected_root {
            return Err(Error::Other(format!(
                "Draft root mismatch during {}: expected {:?}, found {:?}",
                operation, expected_root, state.current_root
            )));
        }
        Ok(state)
    })
}

/// The pre-state a tile step carries into the trace.
///
/// This used to clone `state.fields` wholesale — every element pushed so far,
/// on every step, which is what made a draft cost O(N) trace bytes per step and
/// O(N²) overall. It is now O(#fields · log N).
fn draft_state_witness(state: &DraftRuntimeState) -> DraftStateWitness {
    DraftStateWitness {
        schema: state.schema.clone(),
        fields: state
            .fields
            .iter()
            .map(|(name, field)| (name.clone(), field.witness_field()))
            .collect(),
    }
}

impl ObjectStore {
    pub fn new() -> Self {
        Self {
            objects: BTreeMap::new(),
            source_resolver: None,
        }
    }

    /// Injects the resolver a `Referenced` object dispatches to. Installed
    /// by runtime initialization or replaced between program runs by an
    /// embedding caller that supplies its own input context.
    pub(crate) fn set_source_resolver(&mut self, resolver: Arc<dyn SourceResolver>) {
        self.source_resolver = Some(resolver);
    }

    /// The installed input context, if this runtime has one.
    pub(crate) fn source_resolver(&self) -> Option<Arc<dyn SourceResolver>> {
        self.source_resolver.clone()
    }

    /// Inserts `backing` at `coordinates` under `object_commitment`, and
    /// returns the entry naming it.
    fn put(
        &mut self,
        coordinates: CfsCoordinates,
        object_commitment: Vec<u8>,
        backing: ObjectBacking,
    ) -> StorageEntry {
        // `objects` and the authenticated coordinate index are written in
        // lockstep, so their key sets are identical: this is the same guard
        // `AuthenticatedObjectStore::append` states against the index, and the
        // one a store without an index keeps. `insert` overwrites, and two
        // writes to one coordinate would silently replace an object that
        // outstanding references still point at, surfacing later as a
        // commitment mismatch in `verify_reference` with nothing to say why.
        assert!(
            !self.objects.contains_key(&coordinates),
            "Duplicate storage write at coordinates {:?}",
            coordinates
        );
        let reference = StorageRef::new(coordinates.clone(), object_commitment.clone());
        self.objects
            .insert(coordinates.clone(), StoredObject { reference, backing });
        StorageEntry {
            coordinates,
            object_commitment,
        }
    }

    /// Stores a value the program produced. The returned entry's
    /// `object_commitment` is the half of a [`StorageRef`] that says *which
    /// value*, and it is the only thing a write gives the running program —
    /// see `docs/proposals/storage-role-split.md`.
    pub fn append_serialized_bytes(
        &mut self,
        bytes: &[u8],
        coordinates: CfsCoordinates,
        raster: Option<RasterPayload>,
    ) -> StorageEntry {
        let (backing, object_commitment) = owned_backing(bytes, raster);
        self.put(coordinates, object_commitment, backing)
    }

    /// Loads an authorized set of named sources as one storage object. Today
    /// this is called only for `main`'s entrypoint binding at coordinate `[0]`.
    pub(crate) fn load_authorized_sources(
        &mut self,
        load: AuthorizedSourceLoad,
        coordinates: CfsCoordinates,
    ) -> StorageEntry {
        let (backing, object_commitment) = referenced_backing(load);
        self.put(coordinates, object_commitment, backing)
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

    /// Rebuild the witness for a recorded selection.
    ///
    /// `payload_kind` is not a preference — it is what the recorded commitment
    /// says the witness bytes must be, and the two forms are indistinguishable
    /// from `(coordinates, commitment, selector)` alone: a metadata selection
    /// and a whole-list selection share a path, a source root and a set of
    /// proof steps. The commit pipeline replays a trace it did not produce
    /// (`raster-cli`'s recorder runs in its own process), so it has to be told
    /// which view to regenerate rather than inferring one. See
    /// [`SelectionPayloadKind`].
    pub fn selection_witness(
        &self,
        reference: &StorageRef,
        selector: &SelectorPath,
        payload_kind: SelectionPayloadKind,
    ) -> Result<SelectionWitness> {
        let stored = self.verify_reference(reference)?;
        match &stored.backing {
            ObjectBacking::Owned(owned) => {
                let raster = Self::require_raster(owned, &reference.coordinates)?;
                let index = RasterIndex::from_bytes(&raster.index_bytes)?;
                let selection = index.select(selector)?;
                match payload_kind {
                    SelectionPayloadKind::Raw => {
                        selection_witness_from_raster_selection(&raster.bytes, selector, selection)
                    }
                    SelectionPayloadKind::List => {
                        let (len, elements_root) = index.list_metadata(selector)?;
                        Ok(list_metadata_witness(selector, selection, len, elements_root))
                    }
                }
            }
            ObjectBacking::Referenced(referenced) => {
                let resolver = self.source_resolver.as_deref().ok_or_else(|| {
                    Error::Other(
                        "Storage has a referenced object but no source resolver configured".into(),
                    )
                })?;
                referenced.selection_witness(selector, payload_kind, resolver)
            }
        }
    }

    /// A recur source's authenticated `(len, elements_root)`, as a selection
    /// whose payload is 41 bytes (9 when empty) instead of the whole list.
    ///
    /// This is the read that replaces resolving a recur source
    /// (`docs/proposals/lazy-list-recur.md` §1–§2): it touches the index only,
    /// never an element and never the data file.
    pub fn list_metadata_selection(
        &self,
        reference: &StorageRef,
        selector: &SelectorPath,
    ) -> Result<AuthenticatedListMetadata> {
        let stored = self.verify_reference(reference)?;
        match &stored.backing {
            ObjectBacking::Owned(owned) => {
                let raster = Self::require_raster(owned, &reference.coordinates)?;
                let index = RasterIndex::from_bytes(&raster.index_bytes)?;
                let (len, elements_root) = index.list_metadata(selector)?;
                // Every selection into an owned object anchors to the object's
                // own root, which is what `locate` returns as `root_hash`.
                Ok(list_metadata_payload(
                    selector,
                    index.root_commitment,
                    len,
                    elements_root,
                ))
            }
            ObjectBacking::Referenced(referenced) => {
                let resolver = self.source_resolver.as_deref().ok_or_else(|| {
                    Error::Other(
                        "Storage has a referenced object but no source resolver configured".into(),
                    )
                })?;
                referenced.list_metadata_selection(selector, resolver)
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

impl AuthenticatedObjectStore {
    pub fn new() -> Self {
        let mut tree = TraceTree::new(1);
        tree.append(Bytes(EMPTY_TRIE_NODES[0].to_vec()));
        let frontier = tree
            .frontier()
            .cloned()
            .expect("storage frontier should exist after seed append");
        let cached_root = frontier_root(&frontier);
        Self {
            objects: ObjectStore::new(),
            frontier,
            cached_root,
            coordinate_index: IncrementalCoordinateIndex::new(),
        }
    }

    pub fn snapshot(&self) -> StorageSnapshot {
        StorageSnapshot {
            frontier: serializable_frontier_from_trace_frontier(self.frontier.clone()),
            root: self.current_root(),
            index_root: self.current_index_root(),
        }
    }

    pub fn current_root(&self) -> Vec<u8> {
        self.cached_root.clone()
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

        let timing = crate::profiling::profiling_enabled();

        // `current_root`/`frontier_root` fold the frontier's ommers, and are
        // called once here and once below — so the roots term is timed apart
        // from the work that actually mutates state.
        let roots_start = timing.then(std::time::Instant::now);
        let store_root_before = self.current_root();
        let index_root_before = self.current_index_root();
        let mut roots_ns = elapsed_ns(roots_start);

        let entry = StorageEntry {
            coordinates: coordinates.clone(),
            object_commitment,
        };

        let frontier_start = timing.then(std::time::Instant::now);
        let leaf_hash: Vec<u8> = Sha256Commitment::from(entry.to_bytes().as_slice()).into();
        self.frontier.append(Bytes(leaf_hash));
        let frontier_ns = elapsed_ns(frontier_start);

        // The one recompute per mutation; every read below is served from it.
        // Timed as roots work, not frontier work, so a before/after against the
        // pre-cache numbers compares the same thing.
        let recompute_start = timing.then(std::time::Instant::now);
        self.cached_root = frontier_root(&self.frontier);
        let root_recompute_ns = elapsed_ns(recompute_start);
        roots_ns = roots_ns.saturating_add(root_recompute_ns);

        let log_position: u64 = self.frontier.position().into();
        let index_value = StorageIndexValue {
            log_position,
            object_commitment: entry.object_commitment.clone(),
        };

        let index_start = timing.then(std::time::Instant::now);
        self.coordinate_index
            .insert(coordinates.clone(), index_value);
        let index_ns = elapsed_ns(index_start);

        self.objects
            .put(coordinates, entry.object_commitment.clone(), backing);

        // `frontier_after` clones the frontier and converts it; timed apart
        // from the root recompute so the roots term says whether it is hashing
        // or allocation.
        let frontier_after_start = timing.then(std::time::Instant::now);
        let frontier_after = serializable_frontier_from_trace_frontier(self.frontier.clone());
        let frontier_after_ns = elapsed_ns(frontier_after_start);

        let roots_after_start = timing.then(std::time::Instant::now);
        let record = StorageWriteRecord {
            entry,
            log_position,
            store_root_before,
            store_root_after: self.current_root(),
            index_root_before,
            index_root_after: self.current_index_root(),
            frontier_after,
        };
        roots_ns = roots_ns
            .saturating_add(elapsed_ns(roots_after_start))
            .saturating_add(frontier_after_ns);

        crate::profiling::record_draft_append_phase(
            roots_ns,
            frontier_ns,
            index_ns,
            root_recompute_ns,
            frontier_after_ns,
        );
        record
    }

    pub fn append_serialized_bytes(
        &mut self,
        bytes: &[u8],
        coordinates: CfsCoordinates,
        raster: Option<RasterPayload>,
    ) -> StorageWriteRecord {
        let (backing, object_commitment) = owned_backing(bytes, raster);
        self.append(backing, object_commitment, coordinates)
    }

    /// Loads an authorized set of named sources as one storage object. Today
    /// this is called only for `main`'s entrypoint binding at coordinate `[0]`.
    pub(crate) fn load_authorized_sources(
        &mut self,
        load: AuthorizedSourceLoad,
        coordinates: CfsCoordinates,
    ) -> StorageWriteRecord {
        let (backing, object_commitment) = referenced_backing(load);
        self.append(backing, object_commitment, coordinates)
    }

    /// Injects the resolver a `Referenced` object dispatches to. Set once
    /// per runtime — by runtime initialization in production, or
    /// directly by a caller that supplies its own input context (the trace
    /// recorder, tests).
    pub(crate) fn set_source_resolver(&mut self, resolver: Arc<dyn SourceResolver>) {
        self.objects.set_source_resolver(resolver);
    }

    /// The installed input context, if this runtime has one.
    pub(crate) fn source_resolver(&self) -> Option<Arc<dyn SourceResolver>> {
        self.objects.source_resolver()
    }

    pub fn resolve<T: DeserializeOwned>(&self, reference: &StorageRef) -> Result<StorageValue<T>> {
        self.objects.resolve(reference)
    }

    pub fn select<T: DeserializeOwned>(
        &self,
        reference: &StorageRef,
        selector: &SelectorPath,
    ) -> Result<StorageValue<T>> {
        self.objects.select(reference, selector)
    }

    pub fn selection_witness(
        &self,
        reference: &StorageRef,
        selector: &SelectorPath,
        payload_kind: SelectionPayloadKind,
    ) -> Result<SelectionWitness> {
        self.objects
            .selection_witness(reference, selector, payload_kind)
    }

    pub fn list_metadata_selection(
        &self,
        reference: &StorageRef,
        selector: &SelectorPath,
    ) -> Result<AuthenticatedListMetadata> {
        self.objects.list_metadata_selection(reference, selector)
    }
}

impl Default for ObjectStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for AuthenticatedObjectStore {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
struct SequenceFrame {
    coordinates: CfsCoordinates,
    next_child_index: CfsCoordinate,
    next_synthetic_index: CfsCoordinate,
}

#[derive(Debug, Clone)]
struct RecurFrame {
    site_coordinates: CfsCoordinates,
    next_iteration_index: CfsCoordinate,
    /// Whether a recur *sequence* iteration body is currently open.
    ///
    /// A recur **tile**'s iteration is one tile execution and pushes no
    /// `SequenceFrame`, so its coordinate is `site ++ [iteration]` and the recur
    /// frame is the right place to reserve it. A recur **sequence**'s iteration
    /// is a body of several steps: `enter_recur_sequence_iteration` pushes a
    /// frame at `site ++ [iteration]`, and the body's steps belong *under* it as
    /// `site ++ [iteration, item]`.
    ///
    /// Without this flag `reserve_execution_coordinates` took the recur branch
    /// for body steps too, addressing them as `site ++ [flat]` and advancing
    /// `next_iteration_index` once per step rather than once per iteration —
    /// so the coordinates disagreed with the trace recorder's, and the iteration
    /// numbering drifted on top. See
    /// `docs/issues/fraud-evidence-storage-unavailable.md` §2b.
    iteration_open: bool,
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
            next_child_index: FIRST_COORDINATE,
            next_synthetic_index: FIRST_COORDINATE,
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
            next_iteration_index: FIRST_COORDINATE,
            iteration_open: false,
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
        recur_frame.iteration_open = true;
        self.stack.push(SequenceFrame {
            coordinates,
            next_child_index: FIRST_COORDINATE,
            next_synthetic_index: FIRST_COORDINATE,
        });
        Ok(())
    }

    fn exit_recur_sequence_iteration(&mut self) {
        self.stack
            .pop()
            .expect("Corrupted recur sequence iteration context");
        if let Some(recur_frame) = self.recur_stack.last_mut() {
            recur_frame.iteration_open = false;
        }
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
        // Only a recur *tile* site reserves from the recur frame: its iteration
        // is one tile execution, so `site ++ [iteration]` is the step's own
        // coordinate. Inside a recur *sequence* iteration the body's frame is
        // already on `self.stack` at `site ++ [iteration]`, and the step belongs
        // under it — so fall through and let the frame below assign
        // `site ++ [iteration, item]`, which is what the trace recorder and the
        // CFS both use.
        if let Some(recur_frame) = self
            .recur_stack
            .last_mut()
            .filter(|frame| !frame.iteration_open)
        {
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
        coordinates.push(raster_core::cfs::DRAFT_NAMESPACE);
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
    pub(crate) static THREAD_STORAGE: RefCell<ObjectStore> = RefCell::new(ObjectStore::new());
    pub(crate) static THREAD_SEQUENCE_CONTEXT: RefCell<SequenceExecutionContext> =
        RefCell::new(SequenceExecutionContext::default());
    static THREAD_ACTIVE_EXECUTION_COORDINATES: RefCell<Vec<CfsCoordinates>> = RefCell::new(Vec::new());
    static THREAD_PENDING_OUTPUT_COORDINATES: RefCell<Option<CfsCoordinates>> = const { RefCell::new(None) };
    static THREAD_PENDING_OUTPUT_ENCODING: RefCell<Option<PendingOutputEncoding>> = const { RefCell::new(None) };
    static THREAD_PENDING_RECUR_ITEM: RefCell<Option<PendingRecurItemBinding>> = const { RefCell::new(None) };
    static THREAD_DRAFT_STORAGE: RefCell<BTreeMap<Anchor, DraftRuntimeState>> =
        RefCell::new(BTreeMap::new());
}

fn reset_thread_storage() {
    THREAD_STORAGE.with(|storage| {
        let mut storage = storage.borrow_mut();
        // The input context is installed by `init` or an embedding caller
        // before entering the root sequence, so preserve it when resetting
        // the previous program's storage.
        let source_resolver = storage.source_resolver();
        *storage = ObjectStore::new();
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

pub fn create_draft<S>() -> Result<(Anchor, [u8; 32])>
where
    S: Schema,
{
    let schema = S::schema();
    let coordinates = THREAD_SEQUENCE_CONTEXT
        .with(|context| context.borrow_mut().reserve_synthetic_coordinates())?;
    let anchor = anchor_for_schema(&coordinates, S::schema_hash());
    // The anchor is kept in both modes — it is the draft's identity in the
    // thread-local map, and reserving a coordinate is O(1). Only the root is
    // skipped.
    let current_root = if drafts_are_authenticated() {
        draft_root_from_field_roots(&schema, &BTreeMap::new())?
    } else {
        UNAUTHENTICATED_DRAFT_ROOT
    };
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
        let authenticated = drafts_are_authenticated();
        if authenticated && state.current_root != *expected_root {
            return Err(Error::Other(format!(
                "Draft root mismatch for field '{}': expected {:?}, found {:?}",
                field, expected_root, state.current_root
            )));
        }
        // Schema and set-once checks run in both modes: they are the draft's
        // semantics, not its authentication.
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
        if !authenticated {
            // The field value is what `finalize` materializes from, so it is
            // kept. The per-value root, the op log (replay only) and the root
            // recomposition are all commitment work with no reader here.
            state.fields.insert(
                field.to_string(),
                DraftFieldRuntime::Set {
                    value: tree,
                    root: UNAUTHENTICATED_DRAFT_ROOT,
                },
            );
            return Ok(UNAUTHENTICATED_DRAFT_ROOT);
        }
        let root = draft_value_root(&tree)?;
        state.fields.insert(
            field.to_string(),
            DraftFieldRuntime::Set { value: tree, root },
        );
        state.ops.push(DraftOp::Set {
            field: field.to_string(),
            value: draft_value_from_serialize(value)?,
        });
        state.current_root = state.recompose_root()?;
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
        let authenticated = drafts_are_authenticated();
        if authenticated && state.current_root != *expected_root {
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
        // Hash the new element once, then move the frontier — O(log N). This
        // used to re-Merkleize the entire accumulated list on every push, which
        // dominated the host cost of a large draft. Unauthenticated runs skip
        // the leaf hash and leave the frontier empty: nothing reads it, since
        // `root()` is only reached through root recomposition and the witness,
        // both of which are off.
        let leaf = if authenticated {
            draft_value_root(&tree)?
        } else {
            UNAUTHENTICATED_DRAFT_ROOT
        };
        match state.fields.entry(field.to_string()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                let mut frontier = AppendFrontier::empty();
                if authenticated {
                    frontier.push(leaf);
                }
                entry.insert(DraftFieldRuntime::Append {
                    values: vec![tree],
                    frontier,
                });
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => match entry.get_mut() {
                DraftFieldRuntime::Append { values, frontier } => {
                    values.push(tree);
                    if authenticated {
                        frontier.push(leaf);
                    }
                }
                DraftFieldRuntime::Set { .. } => {
                    return Err(Error::Other(format!(
                        "Draft field '{}' is not appendable",
                        field
                    )))
                }
            },
        }
        if !authenticated {
            return Ok(UNAUTHENTICATED_DRAFT_ROOT);
        }
        state.ops.push(DraftOp::Push {
            field: field.to_string(),
            value: draft_value_from_serialize(value)?,
        });
        state.current_root = state.recompose_root()?;
        Ok(state.current_root)
    })
}

fn store_value_at_coordinates<T: Serialize>(
    value: &T,
    coordinates: CfsCoordinates,
) -> Result<StorageRef> {
    // Phase timings land in the draft-store accumulator only while a
    // `finalize` has armed it; an ordinary tile-output store records nothing.
    let timing = crate::profiling::profiling_enabled();

    let postcard_start = timing.then(std::time::Instant::now);
    let bytes = raster_core::postcard::to_allocvec(value).map_err(|error| {
        Error::Serialization(format!(
            "Failed to serialize storage object for current sequence step: {}",
            error
        ))
    })?;
    let postcard_ns = elapsed_ns(postcard_start);

    let payload_start = timing.then(std::time::Instant::now);
    let raster_payload = Some(raster_payload_for_value(value)?);
    let payload_ns = elapsed_ns(payload_start);

    let append_start = timing.then(std::time::Instant::now);
    let result = THREAD_STORAGE.with(|storage| {
        let entry = storage.borrow_mut().append_serialized_bytes(
            &bytes,
            coordinates.clone(),
            raster_payload,
        );
        Ok(StorageRef::new(coordinates, entry.object_commitment))
    });
    let append_ns = elapsed_ns(append_start);

    crate::profiling::record_draft_store_phase(postcard_ns, payload_ns, append_ns);
    result
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

/// What authorized the item a recur iteration is about to run on, stashed by
/// the driver for the tile wrapper that immediately follows.
///
/// Materializing an authorized item has **two** outputs — the value the tile
/// sees and the binding that proves where it came from — and the tile ABI can
/// only carry the first. `RecurInput<T> { value, index, len }` is what crosses
/// into the replay guest, and widening it would move every recur tile's
/// `input_commitment` to carry data the replay guest cannot use. So the second
/// output travels host-side, through the same hand-off shape
/// [`stash_pending_output_encoding`] already uses for a tile's output
/// encoding: set by the producer, taken by the one consumer that runs next.
///
/// Dropping it instead — which is what `build_recur_input` did — does not
/// merely lose provenance. For a source reached through a bound index it makes
/// a legitimate program *un-auditable*: the item's path still carries the
/// `BoundIndex` segment, and `verify_bound_index_bindings` rejects a selection
/// whose cited source never reaches the step. See
/// `docs/proposals/lazy-list-recur.md` §4.
pub struct PendingRecurItemBinding {
    pub storage: raster_core::trace::StorageData,
    /// Citations inherited from the source binding's path, keyed by the name
    /// the `BoundIndex` segment cites.
    pub index_bindings: Vec<(String, raster_core::trace::StorageData)>,
}

pub fn stash_recur_item_binding(
    storage: raster_core::trace::StorageData,
    index_bindings: Vec<(String, raster_core::trace::StorageData)>,
) {
    THREAD_PENDING_RECUR_ITEM.with(|pending| {
        *pending.borrow_mut() = Some(PendingRecurItemBinding {
            storage,
            index_bindings,
        });
    });
}

/// Take the stashed item binding, if the caller is a recur iteration.
///
/// Returns `None` for every ordinary tile, which is what keeps this from
/// leaking across sites: a non-recur tile never asks, and a recur tile always
/// finds exactly what its own driver just put there.
pub fn take_recur_item_binding() -> Option<PendingRecurItemBinding> {
    THREAD_PENDING_RECUR_ITEM.with(|pending| pending.borrow_mut().take())
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
        let entry = storage.borrow_mut().append_serialized_bytes(
            &bytes,
            coordinates.clone(),
            Some(raster_payload),
        );
        Ok(StorageRef::new(coordinates, entry.object_commitment))
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

/// Materialize a draft into its value, consuming the draft state.
///
/// The half both modes share. Set-once completeness and the empty-recur default
/// rules live here, so an unauthenticated finalize enforces them identically
/// rather than reimplementing them — the two modes can only disagree about
/// commitments, never about whether a draft was validly built.
///
/// `require_complete` is `false` for the empty-recur path, where an untouched
/// output must still materialize if the schema allows it.
pub fn finalize_draft_value<S>(
    anchor: &Anchor,
    expected_root: &[u8; 32],
    require_complete: bool,
) -> Result<S>
where
    S: Schema + DeserializeOwned + Serialize,
{
    let operation = if require_complete {
        "finalize"
    } else {
        "empty finalize"
    };
    let state = take_draft_state(anchor, expected_root, operation)?;
    // Materializing the whole object here is correct and stays: it is O(N)
    // once, which was never the problem.
    let tree = build_draft_tree(&state.schema, &state.field_values(), require_complete)?;
    typed_value_from_tree::<S>(&tree).map_err(|error| {
        if !require_complete {
            if let Ok(Some(field)) = first_unset_set_once_field(&state.schema, &state.fields) {
                return Error::Other(format!(
                    "Empty recur input cannot finalize draft '{}': field '{}' was never written and the schema cannot materialize a default value",
                    core::any::type_name::<S>(),
                    field
                ));
            }
            return Error::Serialization(format!(
                "Failed to materialize finalized empty draft value: {}",
                error
            ));
        }
        Error::Serialization(format!(
            "Failed to materialize finalized draft value: {}",
            error
        ))
    })
}

fn store_finalized_draft<S>(value: &S) -> Result<StorageRef>
where
    S: Serialize,
{
    if let Some(coordinates) = current_recur_site_coordinates() {
        store_value_at_coordinates(value, coordinates)
    } else {
        store_value(value)
    }
}

pub fn finalize_draft<S>(anchor: &Anchor, expected_root: &[u8; 32]) -> Result<StorageRef>
where
    S: Schema + DeserializeOwned + Serialize,
{
    finalize_and_store::<S>(anchor, expected_root, true)
}

pub fn finalize_empty_draft<S>(anchor: &Anchor, expected_root: &[u8; 32]) -> Result<StorageRef>
where
    S: Schema + DeserializeOwned + Serialize,
{
    finalize_and_store::<S>(anchor, expected_root, false)
}

/// The two halves of closing a draft, timed separately.
///
/// They are separated because they are the two costs
/// `docs/proposals/incremental-draft-materialization.md` removes, and it
/// removes them for different reasons: *materialize* rebuilds the whole object
/// from the draft's field values, and *store* then encodes it and re-derives
/// every element root that the draft's `AppendFrontier` already folded. A
/// before/after needs to see which half moved.
fn finalize_and_store<S>(
    anchor: &Anchor,
    expected_root: &[u8; 32],
    require_complete: bool,
) -> Result<StorageRef>
where
    S: Schema + DeserializeOwned + Serialize,
{
    let profiling_enabled = crate::profiling::profiling_enabled();

    let materialize_start = profiling_enabled.then(std::time::Instant::now);
    let value = finalize_draft_value::<S>(anchor, expected_root, require_complete)?;
    let materialize_ns = elapsed_ns(materialize_start);

    crate::profiling::arm_draft_store_phases();
    let store_start = profiling_enabled.then(std::time::Instant::now);
    let reference = store_finalized_draft(&value);
    let store_ns = elapsed_ns(store_start);
    let phases = crate::profiling::take_draft_store_phases();

    crate::profiling::record_sequence_draft_finalize(materialize_ns, store_ns, phases);
    reference
}

fn elapsed_ns(start: Option<std::time::Instant>) -> u64 {
    start
        .map(|start| u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
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

/// The authenticated `(len, elements_root)` of the list at `selector`, as a
/// 41-byte selection (9 when empty).
///
/// Unlike [`select_stored_value`] this deserializes nothing and reads no
/// element — it is the whole reason a recur source no longer has to be
/// materialized to be traced.
pub fn stored_list_metadata(
    reference: &StorageRef,
    selector: &SelectorPath,
) -> Result<AuthenticatedListMetadata> {
    THREAD_STORAGE.with(|storage| storage.borrow().list_metadata_selection(reference, selector))
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
        let mut manager = AuthenticatedObjectStore::new();
        let coordinates = CfsCoordinates(vec![1, 2, 3]);

        manager.append_serialized_bytes(b"first", coordinates.clone(), None);
        manager.append_serialized_bytes(b"second", coordinates, None);
    }

    /// The same guard, on the store a *running program* uses. The
    /// authenticated store states it against the coordinate index; an
    /// `ObjectStore` has no index, so it states it against `objects` — and the
    /// running program depends on this one, not the test above.
    /// See `docs/proposals/storage-role-split.md`.
    #[test]
    #[should_panic(expected = "Duplicate storage write at coordinates")]
    fn object_store_rejects_duplicate_coordinate_writes() {
        let mut objects = ObjectStore::new();
        let coordinates = CfsCoordinates(vec![1, 2, 3]);

        objects.append_serialized_bytes(b"first", coordinates.clone(), None);
        objects.append_serialized_bytes(b"second", coordinates, None);
    }

    /// A write gives the running program its object commitment and nothing
    /// else, and a read finds the value back under it.
    #[test]
    fn object_store_round_trips_a_value_through_its_commitment() {
        let mut objects = ObjectStore::new();
        let coordinates = CfsCoordinates(vec![7]);

        let entry = objects.append_serialized_bytes(b"payload", coordinates.clone(), None);

        assert_eq!(entry.coordinates, coordinates);
        assert_eq!(
            entry.object_commitment,
            internal_object_commitment(b"payload", None),
        );

        let reference = StorageRef::new(coordinates, entry.object_commitment);
        let stored = objects
            .verify_reference(&reference)
            .expect("the object just written must verify");
        assert_eq!(stored.reference, reference);
    }

    #[test]
    fn authorized_source_load_commits_to_declared_sources_in_order() {
        let mut manager = AuthenticatedObjectStore::new();
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
                        kind: ReferencedSourceKind::Raster {
                            schema: || SchemaNode::Leaf {
                                type_name: String::new(),
                            },
                        },
                    },
                    AuthorizedSource {
                        name: "beta".into(),
                        encoding: ExternalEncoding::Raster,
                        commitment: beta_commitment.clone(),
                        kind: ReferencedSourceKind::Raster {
                            schema: || SchemaNode::Leaf {
                                type_name: String::new(),
                            },
                        },
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

    /// A draft with a `List<String>` field, for the large-draft measurement.
    #[derive(Debug, Deserialize, Serialize)]
    struct BigDraft {
        lines: Vec<String>,
    }

    impl Selectable for BigDraft {
        fn schema() -> SchemaNode {
            SchemaNode::Struct {
                type_name: "BigDraft".into(),
                fields: vec![SchemaField::new(
                    "lines",
                    "lines",
                    SchemaNode::List {
                        type_name: "List<String>".into(),
                        element: Box::new(SchemaNode::Leaf {
                            type_name: "String".into(),
                        }),
                    },
                )],
            }
        }
    }

    /// How a draft's close scales in its element count.
    ///
    /// `docs/proposals/incremental-draft-materialization.md` is entirely about
    /// an `O(N)` term, and every number measured for it so far came from
    /// `hello-tiles`, whose drafts hold **two** elements — a size at which an
    /// `O(N)` term and a constant are indistinguishable. This walks N so the
    /// growth is visible instead of inferred.
    ///
    /// Ignored by default: it is a measurement, not an assertion.
    /// `cargo test -p raster-runtime --release --lib large_draft -- --ignored --nocapture`
    #[test]
    #[ignore = "measurement, not an assertion"]
    fn large_draft_finalize_scaling() {
        println!();
        println!(
            "{:>8}  {:>12}  {:>12}  {:>12}  {:>12}  {:>12}  {:>10}",
            "N", "push_total", "materialize", "encode", "append", "finalize", "ns/element"
        );

        for n in [1usize, 16, 64, 256, 1024, 4096, 16384] {
            let _scope = SequenceScopeGuard::enter("bench");
            let (anchor, mut root) =
                create_draft::<BigDraft>().expect("draft is created");

            let push_start = std::time::Instant::now();
            for index in 0..n {
                let line = format!("line-{index:08}");
                root = apply_draft_push::<BigDraft, String>(&anchor, &root, "lines", &line)
                    .expect("push applies");
            }
            let push_ns = push_start.elapsed().as_nanos() as u64;

            // The two halves of the close, timed apart exactly as
            // `finalize_and_store` times them in a real run.
            let materialize_start = std::time::Instant::now();
            let value = finalize_draft_value::<BigDraft>(&anchor, &root, true)
                .expect("draft materializes");
            let materialize_ns = materialize_start.elapsed().as_nanos() as u64;
            assert_eq!(value.lines.len(), n, "draft holds every pushed element");

            // `encode_raster_value` is Stage 1's target: the second hash of
            // every element. Measured on its own, outside the storage write.
            let encode_start = std::time::Instant::now();
            let _payload = raster_payload_for_value(&value).expect("value encodes");
            let encode_ns = encode_start.elapsed().as_nanos() as u64;

            let append_start = std::time::Instant::now();
            let stored = store_finalized_draft(&value).expect("value stores");
            let append_ns = append_start.elapsed().as_nanos() as u64;
            let _ = stored;

            let finalize_ns = materialize_ns + append_ns;
            println!(
                "{:>8}  {:>10.2}ms  {:>10.2}ms  {:>10.2}ms  {:>10.2}ms  {:>10.2}ms  {:>10}",
                n,
                push_ns as f64 / 1e6,
                materialize_ns as f64 / 1e6,
                encode_ns as f64 / 1e6,
                append_ns as f64 / 1e6,
                finalize_ns as f64 / 1e6,
                finalize_ns / n as u64,
            );
        }
        println!();
        println!("`encode` is included in `append` (append calls it); it is timed twice on purpose.");
    }
}
