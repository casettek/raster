//! What backs the bytes committed at a storage coordinate.
//!
//! An object at a coordinate is either `Owned` — bytes this run computed,
//! held in memory — or `Referenced`: no bytes at all, just a
//! struct-of-commitments over `main`'s entry arguments, resolved lazily from
//! disk the first time something selects into it. Keeping the two behind one
//! enum is what lets every reader treat "a value at a coordinate" uniformly
//! while only one of them can touch the filesystem.

use std::sync::{Arc, OnceLock};
use std::vec::Vec;

use raster_core::cfs::CfsCoordinates;
use raster_core::input::{
    struct_commitments_root, AuthenticatedListMetadata, Hash32, SchemaNode, SelectedPayload,
    SelectionCommitment, SelectionPayloadKind, SelectionProof, SelectionProofStep,
    SelectionWitness, SelectorPath, SelectorSegment,
};
use raster_core::trace::RasterPayload;
use raster_core::{Error, Result};
use serde::de::DeserializeOwned;

use crate::input::{
    hex_string, list_metadata_payload, list_metadata_witness, prove_selection,
    selected_payload_from_proven, selected_payload_from_raster_location,
    selection_witness_from_raster_selection, subtree_payload_and_root,
    tree_value_from_raster_location, typed_value_from_tree, RasterData, TreeValue,
};
use crate::raster_index::RasterIndex;
use crate::source::{ResolvedSourceData, SourceResolver};

/// What backs the bytes committed at a storage coordinate.
#[derive(Debug, Clone)]
pub(crate) enum ObjectBacking {
    /// Bytes were computed this run (tile output, finalized draft, recur
    /// iteration, ...) and live in memory — reads are served directly.
    Owned(OwnedObject),
    /// This coordinate holds no bytes at all, only a struct-of-commitments
    /// over `main`'s declared entry arguments. A selection must name which
    /// argument it wants before anything can be resolved into it.
    Referenced(ReferencedObject),
    /// An object a recur site derived from another (`output = base`): the
    /// base's bytes and index nodes shared by reference, plus what the sweep
    /// appended (`incremental-draft-materialization` §How a derived object
    /// maps onto buffers). Reads answer exactly as for the same object stored
    /// contiguously.
    Derived(DerivedObject),
}

#[derive(Debug, Clone)]
pub(crate) struct OwnedObject {
    pub bytes: Vec<u8>,
    pub raster: Option<Arc<RasterObject>>,
}

/// A raster payload as storage holds it, with its index parsed once, on the
/// first read, and shared from then on — by every read of this object and by
/// every object derived from it.
#[derive(Debug)]
pub(crate) struct RasterObject {
    pub payload: RasterPayload,
    index: OnceLock<Arc<RasterIndex>>,
}

impl RasterObject {
    pub(crate) fn new(payload: RasterPayload) -> Self {
        Self {
            payload,
            index: OnceLock::new(),
        }
    }

    /// An object whose index is already in hand (a seal's), so it is never
    /// parsed from its encoded bytes.
    pub(crate) fn with_index(payload: RasterPayload, index: RasterIndex) -> Self {
        let object = Self::new(payload);
        let _ = object.index.set(Arc::new(index));
        object
    }

    pub(crate) fn index(&self) -> Result<Arc<RasterIndex>> {
        if let Some(index) = self.index.get() {
            return Ok(index.clone());
        }
        let parsed = Arc::new(RasterIndex::from_bytes(&self.payload.index_bytes)?);
        Ok(self.index.get_or_init(|| parsed).clone())
    }
}

/// A derived object: its logical payload as pieces, and its index layered
/// over its base's.
#[derive(Debug, Clone)]
pub(crate) struct DerivedObject {
    pub pieces: Arc<PieceTable>,
    pub index: Arc<RasterIndex>,
    pub root: Hash32,
    /// The delta this object was built from, for the site close event that
    /// carries it to the recorder. Set on the child's side.
    pub delta: Option<Arc<raster_core::trace::DerivedPayload>>,
}

/// A logical payload assembled from pieces of other buffers, in order and
/// without gaps. A read that stays inside one piece is a slice of it; one that
/// spans pieces gathers them. An element never straddles the base/tail
/// boundary, so selecting one never gathers.
#[derive(Debug, Clone, Default)]
pub(crate) struct PieceTable {
    pub pieces: Vec<Piece>,
    pub len: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct Piece {
    /// Where the piece starts in the logical payload.
    pub start: u64,
    pub len: u64,
    pub source: PieceSource,
    pub source_offset: u64,
}

#[derive(Debug, Clone)]
pub(crate) enum PieceSource {
    /// A contiguous stored object's bytes — the first object of a chain.
    Object(Arc<RasterObject>),
    /// Bytes a derivation added: headers it rewrote and elements it appended.
    Tail(Arc<Vec<u8>>),
}

impl PieceSource {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Object(object) => &object.payload.bytes,
            Self::Tail(bytes) => bytes,
        }
    }
}

impl PieceTable {
    /// Append a piece, merging it into the previous one when it continues the
    /// same source contiguously.
    pub(crate) fn push(&mut self, source: PieceSource, source_offset: u64, len: u64) {
        if len == 0 {
            return;
        }
        if let Some(last) = self.pieces.last_mut() {
            let same = match (&last.source, &source) {
                (PieceSource::Object(a), PieceSource::Object(b)) => Arc::ptr_eq(a, b),
                (PieceSource::Tail(a), PieceSource::Tail(b)) => Arc::ptr_eq(a, b),
                _ => false,
            };
            if same && last.source_offset + last.len == source_offset {
                last.len += len;
                self.len += len;
                return;
            }
        }
        self.pieces.push(Piece {
            start: self.len,
            len,
            source,
            source_offset,
        });
        self.len += len;
    }

    /// The pieces covering logical `[offset, offset + len)`, each as its
    /// source and range — what a derivation of a derivation copies, so a
    /// chain's pieces always point at original buffers.
    pub(crate) fn ranges(&self, offset: u64, len: u64) -> Result<Vec<(PieceSource, u64, u64)>> {
        let end = offset
            .checked_add(len)
            .filter(|end| *end <= self.len)
            .ok_or_else(|| Error::Serialization("Raster subtree points outside the object".into()))?;
        let mut out = Vec::new();
        let first = self
            .pieces
            .partition_point(|piece| piece.start + piece.len <= offset);
        let mut cursor = offset;
        for piece in &self.pieces[first..] {
            if cursor >= end {
                break;
            }
            let within = cursor - piece.start;
            let take = (piece.len - within).min(end - cursor);
            out.push((piece.source.clone(), piece.source_offset + within, take));
            cursor += take;
        }
        Ok(out)
    }
}

impl RasterData for PieceTable {
    fn read_subtree(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len as usize);
        for (source, from, take) in self.ranges(offset, len)? {
            out.extend_from_slice(&source.bytes()[from as usize..(from + take) as usize]);
        }
        Ok(out)
    }
}

/// An object's bytes as a read sees them.
#[derive(Debug, Clone)]
pub(crate) enum ObjectBytes {
    Contiguous(Arc<RasterObject>),
    Pieces(Arc<PieceTable>),
}

impl RasterData for ObjectBytes {
    fn read_subtree(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        match self {
            Self::Contiguous(object) => object.payload.bytes.read_subtree(offset, len),
            Self::Pieces(pieces) => pieces.read_subtree(offset, len),
        }
    }
}

impl ObjectBytes {
    pub(crate) fn len(&self) -> u64 {
        match self {
            Self::Contiguous(object) => object.payload.bytes.len() as u64,
            Self::Pieces(pieces) => pieces.len,
        }
    }
}

/// A program-written object as a raster read sees it: its index, its bytes
/// and its root — one shape for a contiguous object and a derived one.
pub(crate) struct RasterView {
    pub index: Arc<RasterIndex>,
    pub bytes: ObjectBytes,
    pub root: Hash32,
}

#[derive(Debug, Clone)]
pub(crate) struct ReferencedObject {
    pub sources: Vec<ReferencedSource>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReferencedSource {
    pub name: String,
    pub commitment: Vec<u8>,
    pub kind: ReferencedSourceKind,
}

#[derive(Clone)]
pub(crate) enum ReferencedSourceKind {
    /// Self-describing on disk (an `.rindex` carries the tree), plus the
    /// declared `Selectable` schema so `Bytes<N>` can be checked at load.
    Raster {
        schema: fn() -> SchemaNode,
    },
    /// Postcard bytes carry no schema of their own; the macro-generated
    /// bind site supplies these two monomorphized, zero-capture function
    /// pointers so this stays a plain `Clone` enum rather than a trait
    /// object.
    Postcard {
        to_tree: fn(&[u8]) -> Result<TreeValue>,
        schema: fn() -> SchemaNode,
    },
}

impl std::fmt::Debug for ReferencedSourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Raster { .. } => f.write_str("Raster"),
            Self::Postcard { .. } => f.write_str("Postcard"),
        }
    }
}

impl ReferencedObject {
    /// The struct-of-commitments root over all declared sources, in
    /// declaration order. Uses the shared `TreeValue::Struct` convention, so
    /// the combined entry object is an ordinary struct node as far as
    /// selection is concerned — selecting one argument out of it composes as
    /// one ordinary proof step rather than a special case — and the guest's
    /// `checks::entrypoint::combined_root` recomputes the identical bytes by
    /// calling the same function.
    pub fn combined_root(&self) -> Vec<u8> {
        struct_commitments_root(
            self.sources
                .iter()
                .map(|source| (source.name.as_str(), source.commitment.as_slice())),
        )
        .to_vec()
    }

    fn find_source(&self, selector: &SelectorPath) -> Result<(&ReferencedSource, SelectorPath)> {
        let Some((head, rest)) = selector.segments.split_first() else {
            return Err(Error::Other(
                "Referenced object requires a field selector naming a declared entry argument"
                    .into(),
            ));
        };
        let SelectorSegment::Field(name) = head else {
            return Err(Error::Other(
                "Referenced object selector must start with a named field".into(),
            ));
        };
        let source = self
            .sources
            .iter()
            .find(|source| &source.name == name)
            .ok_or_else(|| Error::Other(format!("Unknown entry argument '{}'", name)))?;
        Ok((source, SelectorPath::new(rest.to_vec())))
    }

    fn verify_source_commitment(
        source: &ReferencedSource,
        resolved: &ResolvedSourceData,
    ) -> Result<()> {
        if !resolved
            .commitment()
            .eq_ignore_ascii_case(&hex_string(&source.commitment))
        {
            return Err(Error::Other(format!(
                "Entry argument '{}' resolved to a different commitment than authorized at bind time",
                source.name
            )));
        }
        Ok(())
    }

    /// The outer struct proof step over the named source: its position among
    /// the declared arguments, every argument's name, and the other
    /// arguments' already-public commitments as siblings (ascending position
    /// order, `field_index` skipped) — the exact shape
    /// `SelectionProofStep::Struct` expects.
    fn struct_step(&self, name: &str) -> Result<SelectionProofStep> {
        let index = self
            .sources
            .iter()
            .position(|source| source.name == name)
            .ok_or_else(|| Error::Other(format!("Unknown entry argument '{}'", name)))?;
        let siblings = self
            .sources
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != index)
            .map(|(_, source)| -> Result<Hash32> {
                source.commitment.as_slice().try_into().map_err(|_| {
                    Error::Other(format!(
                        "Entry argument '{}' commitment is not 32 bytes",
                        source.name
                    ))
                })
            })
            .collect::<Result<Vec<Hash32>>>()?;
        Ok(SelectionProofStep::Struct {
            field_index: index as u64,
            field_names: self
                .sources
                .iter()
                .map(|source| source.name.clone())
                .collect(),
            siblings,
        })
    }

    /// Resolve `selector` (must start with `Field(name)`) against the named
    /// source, returning the selected tree value and its selection payload
    /// anchored to the combined root.
    pub fn select(
        &self,
        selector: &SelectorPath,
        resolver: &dyn SourceResolver,
    ) -> Result<(TreeValue, SelectedPayload)> {
        let (source, remaining) = self.find_source(selector)?;
        let resolved = resolver.resolve(&source.name)?;
        Self::verify_source_commitment(source, &resolved)?;

        let (tree, mut selected) = self.select_from_resolved(source, &remaining, &resolved)?;

        selected.commitment.path = full_selector_path(&source.name, &remaining);
        selected.commitment.source_root_hash = self
            .combined_root()
            .try_into()
            .map_err(|_| Error::Other("Combined root is not 32 bytes".into()))?;
        Ok((tree, selected))
    }

    fn select_from_resolved(
        &self,
        source: &ReferencedSource,
        remaining: &SelectorPath,
        resolved: &ResolvedSourceData,
    ) -> Result<(TreeValue, SelectedPayload)> {
        match (&source.kind, resolved) {
            (ReferencedSourceKind::Raster { schema }, ResolvedSourceData::Raster { .. }) => {
                let index = resolved
                    .raster_index()
                    .ok_or_else(|| Error::Other("Expected raster index metadata".into()))?;
                let data = resolved
                    .raster_data_file()
                    .ok_or_else(|| Error::Other("Expected raster data file".into()))?;
                check_schema_page_sizes(&schema(), index, data)?;
                let location = index.locate(remaining)?;
                let tree = tree_value_from_raster_location(index, data, &location)?;
                let selected = selected_payload_from_raster_location(data, remaining, location)?;
                Ok((tree, selected))
            }
            (
                ReferencedSourceKind::Postcard { to_tree, schema },
                ResolvedSourceData::Postcard { .. },
            ) => {
                let tree = to_tree(resolved.bytes())?;
                let (_, root_hash) = subtree_payload_and_root(&tree)?;
                if root_hash.to_vec() != source.commitment {
                    return Err(Error::Other(format!(
                        "Entry argument '{}' failed structural integrity check",
                        source.name
                    )));
                }
                let proven = prove_selection(&schema(), &tree, &remaining.segments)?;
                let selected_tree = proven.selected_value.clone();
                let selected = selected_payload_from_proven(remaining, proven);
                Ok((selected_tree, selected))
            }
            _ => Err(Error::Other(format!(
                "Entry argument '{}' encoding does not match its declared kind",
                source.name
            ))),
        }
    }

    /// Same resolution as `select`, but produces a full `SelectionWitness`
    /// (with the recombination proof steps) for guest verification —
    /// prepending the outer struct step over the other sources' already-
    /// public commitments to whatever inner steps locate the value inside
    /// the named source.
    pub fn selection_witness(
        &self,
        selector: &SelectorPath,
        payload_kind: SelectionPayloadKind,
        resolver: &dyn SourceResolver,
    ) -> Result<SelectionWitness> {
        let (source, remaining) = self.find_source(selector)?;
        let resolved = resolver.resolve(&source.name)?;
        Self::verify_source_commitment(source, &resolved)?;

        let inner = match (&source.kind, &resolved) {
            (ReferencedSourceKind::Raster { schema }, ResolvedSourceData::Raster { .. }) => {
                let index = resolved
                    .raster_index()
                    .ok_or_else(|| Error::Other("Expected raster index metadata".into()))?;
                let data = resolved
                    .raster_data_file()
                    .ok_or_else(|| Error::Other("Expected raster data file".into()))?;
                check_schema_page_sizes(&schema(), index, data)?;
                let selection = index.select(&remaining)?;
                match payload_kind {
                    SelectionPayloadKind::Raw => {
                        selection_witness_from_raster_selection(data, &remaining, selection)?
                    }
                    SelectionPayloadKind::List => {
                        let (len, elements_root) = index.list_metadata(&remaining)?;
                        list_metadata_witness(&remaining, selection, len, elements_root)
                    }
                }
            }
            (
                ReferencedSourceKind::Postcard { to_tree, schema },
                ResolvedSourceData::Postcard { .. },
            ) => {
                // A metadata view is derived from a `.rindex`; a postcard
                // source has none. This is the same class of refusal as the
                // recur-source rule in `lazy-list-recur.md` §3, reached from
                // the witness side.
                if payload_kind == SelectionPayloadKind::List {
                    return Err(Error::Other(format!(
                        "Entry argument '{}' is postcard-encoded and cannot supply list metadata; \
                         re-encode this input with encoding = \"raster\"",
                        source.name
                    )));
                }
                let tree = to_tree(resolved.bytes())?;
                let (_, root_hash) = subtree_payload_and_root(&tree)?;
                if root_hash.to_vec() != source.commitment {
                    return Err(Error::Other(format!(
                        "Entry argument '{}' failed structural integrity check",
                        source.name
                    )));
                }
                let proven = prove_selection(&schema(), &tree, &remaining.segments)?;
                SelectionWitness::from_payload(
                    proven.selected_bytes.clone(),
                    SelectionProof {
                        path: remaining.clone(),
                        root_hash: proven.root_hash,
                        steps: proven.steps.clone(),
                    },
                )
            }
            _ => {
                return Err(Error::Other(format!(
                    "Entry argument '{}' encoding does not match its declared kind",
                    source.name
                )))
            }
        };

        let mut steps = inner.proof.steps;
        // `verify_selection_proof` walks `steps` via `.rev()`, from the leaf
        // outward — so the outermost step (combining this source's own root
        // with its siblings into the combined root) must be the *first*
        // element, not appended after the source's own (more inner) steps.
        steps.insert(0, self.struct_step(&source.name)?);
        Ok(SelectionWitness::from_payload(
            inner.bytes,
            SelectionProof {
                path: full_selector_path(&source.name, &remaining),
                root_hash: self
                    .combined_root()
                    .try_into()
                    .map_err(|_| Error::Other("Combined root is not 32 bytes".into()))?,
                steps,
            },
        ))
    }

    /// A recur source's `(len, elements_root)` from an external input, without
    /// resolving an element.
    ///
    /// Raster only: a postcard source is sequential and carries no index, so
    /// `rows[i]` cannot be located without decoding everything before it. That
    /// is the case `lazy-list-recur.md` §3 refuses at `open` rather than
    /// servicing at `O(list)`.
    pub fn list_metadata_selection(
        &self,
        selector: &SelectorPath,
        resolver: &dyn SourceResolver,
    ) -> Result<AuthenticatedListMetadata> {
        let (source, remaining) = self.find_source(selector)?;
        let resolved = resolver.resolve(&source.name)?;
        Self::verify_source_commitment(source, &resolved)?;

        let (ReferencedSourceKind::Raster { schema }, ResolvedSourceData::Raster { .. }) =
            (&source.kind, &resolved)
        else {
            return Err(Error::Other(format!(
                "a recur source must be a raster-indexed List (call_recur! or call_recur_seq!); \
                 re-encode this input with encoding = \"raster\" (input '{}')",
                source.name
            )));
        };

        let index = resolved
            .raster_index()
            .ok_or_else(|| Error::Other("Expected raster index metadata".into()))?;
        if let Some(data) = resolved.raster_data_file() {
            check_schema_page_sizes(&schema(), index, data)?;
        }
        let (len, elements_root) = index.list_metadata(&remaining)?;

        let mut selected =
            list_metadata_payload(&remaining, index.root_commitment, len, elements_root);
        // Re-anchor to the combined root, exactly as `select` does: an entry
        // argument's selections are proven against the object that binds every
        // declared source, not against one source's own root.
        selected.selected.commitment.path = full_selector_path(&source.name, &remaining);
        selected.selected.commitment.source_root_hash = self
            .combined_root()
            .try_into()
            .map_err(|_| Error::Other("Combined root is not 32 bytes".into()))?;
        Ok(selected)
    }
}

fn parse_bytes_type_page_size(type_name: &str) -> Option<u64> {
    type_name
        .strip_prefix("$raster::Bytes<")?
        .strip_suffix('>')?
        .parse()
        .ok()
}

/// `position` is the leaf's data-file position (index offsets are
/// parent-relative, `rindex04`).
fn read_u64_leaf(
    index: &RasterIndex,
    data: &impl RasterData,
    node_id: u64,
    position: u64,
) -> Result<u64> {
    let node = index.get_node(node_id)?;
    let subtree = data.read_subtree(position, node.len)?;
    if subtree.first().copied() != Some(0x00) || subtree.len() < 17 {
        return Err(Error::Other("expected a u64 leaf payload".into()));
    }
    let len = u64::from_le_bytes(subtree[1..9].try_into().unwrap());
    if len != 8 {
        return Err(Error::Other("u64 leaf has unexpected width".into()));
    }
    Ok(u64::from_le_bytes(subtree[9..17].try_into().unwrap()))
}

fn schema_mentions_bytes(schema: &SchemaNode) -> bool {
    match schema {
        SchemaNode::Struct { type_name, fields } => {
            parse_bytes_type_page_size(type_name).is_some()
                || fields.iter().any(|field| schema_mentions_bytes(&field.schema))
        }
        SchemaNode::List { element, .. } => schema_mentions_bytes(element),
        SchemaNode::Leaf { .. } => false,
    }
}

fn check_schema_page_sizes(
    schema: &SchemaNode,
    index: &RasterIndex,
    data: &impl RasterData,
) -> Result<()> {
    walk_schema_page_sizes(schema, index.root_node, index.root_position()?, index, data)
}

fn walk_schema_page_sizes(
    schema: &SchemaNode,
    node_id: u64,
    position: u64,
    index: &RasterIndex,
    data: &impl RasterData,
) -> Result<()> {
    match schema {
        SchemaNode::Struct { type_name, fields } => {
            if let Some(declared) = parse_bytes_type_page_size(type_name) {
                let node = index.get_node(node_id)?;
                let crate::raster_index::RasterNodeKind::Struct { fields: idx_fields } =
                    &node.kind
                else {
                    return Err(Error::Other(
                        "schema names Bytes but the artifact node is not a struct".into(),
                    ));
                };
                let page_size_id = idx_fields
                    .iter()
                    .find(|field| field.name == "page_size")
                    .map(|field| field.child)
                    .ok_or_else(|| Error::Other("Bytes artifact is missing page_size".into()))?;
                let artifact = read_u64_leaf(
                    index,
                    data,
                    page_size_id,
                    index.child_position(position, page_size_id)?,
                )?;
                if artifact != declared {
                    return Err(Error::PageSizeMismatch { declared, artifact });
                }
                if let (Some(byte_len_id), Some(pages_id)) = (
                    idx_fields
                        .iter()
                        .find(|field| field.name == "byte_len")
                        .map(|field| field.child),
                    idx_fields
                        .iter()
                        .find(|field| field.name == "pages")
                        .map(|field| field.child),
                ) {
                    let byte_len = read_u64_leaf(
                        index,
                        data,
                        byte_len_id,
                        index.child_position(position, byte_len_id)?,
                    )?;
                    if let Some(len) = index.list_len(pages_id)? {
                        raster_core::check_page_partition(byte_len, declared, len)?;
                    }
                }
            }
            let node = index.get_node(node_id)?;
            if let crate::raster_index::RasterNodeKind::Struct { fields: idx_fields } = &node.kind
            {
                for field in fields {
                    if let Some(child) = idx_fields.iter().find(|f| f.name == field.name) {
                        walk_schema_page_sizes(
                            &field.schema,
                            child.child,
                            index.child_position(position, child.child)?,
                            index,
                            data,
                        )?;
                    }
                }
            }
            Ok(())
        }
        SchemaNode::List { element, .. } => {
            if !schema_mentions_bytes(element) {
                return Ok(());
            }
            if index.list_len(node_id)?.unwrap_or(0) > 0 {
                let first = index.list_element(node_id, 0)?;
                walk_schema_page_sizes(
                    element,
                    first,
                    index.child_position(position, first)?,
                    index,
                    data,
                )?;
            }
            Ok(())
        }
        SchemaNode::Leaf { .. } => Ok(()),
    }
}

fn full_selector_path(name: &str, remaining: &SelectorPath) -> SelectorPath {
    let mut segments = Vec::with_capacity(remaining.segments.len() + 1);
    segments.push(SelectorSegment::Field(name.to_string()));
    segments.extend(remaining.segments.iter().cloned());
    SelectorPath::new(segments)
}

impl OwnedObject {
    /// Whole-value resolve (no selector): the raster case walks the same
    /// path a selector-based read would with an empty selector; the
    /// non-raster case deserializes directly (there is no selection tree
    /// to prove against, hence the placeholder commitment — matches the
    /// pre-refactor behavior exactly).
    pub(crate) fn resolve_whole<T: DeserializeOwned>(
        &self,
        coordinates: &CfsCoordinates,
    ) -> Result<(Vec<u8>, SelectionCommitment, T)> {
        if let Some(raster) = self.raster.as_ref() {
            resolve_whole_view(&RasterView {
                index: raster.index()?,
                bytes: ObjectBytes::Contiguous(raster.clone()),
                root: raster.payload.root_hash,
            })        } else {
            let value = raster_core::postcard::from_bytes(&self.bytes).map_err(|e| {
                Error::Serialization(format!(
                    "Failed to deserialize storage object at coordinates {:?}: {}",
                    coordinates, e
                ))
            })?;
            Ok((
                self.bytes.clone(),
                SelectionCommitment {
                    path: SelectorPath::default(),
                    source_root_hash: [0; 32],
                    selected_hash: [0; 32],
                    selected_len: 0,
                    payload_kind: SelectionPayloadKind::Raw,
                },
                value,
            ))
        }
    }
}

/// Whole-value resolve of a raster object: the root location, decoded.
pub(crate) fn resolve_whole_view<T: DeserializeOwned>(
    view: &RasterView,
) -> Result<(Vec<u8>, SelectionCommitment, T)> {
    let location = view.index.root_location()?;
    let bytes = view.bytes.read_subtree(0, view.bytes.len())?;
    let tree = tree_value_from_raster_location(&view.index, &view.bytes, &location)?;
    let value = typed_value_from_tree(&tree)?;
    Ok((
        SelectionCommitment {
            path: SelectorPath::default(),
            source_root_hash: view.root,
            selected_hash: raster_core::input::selection_payload_hash(&bytes),
            selected_len: bytes.len() as u64,
            payload_kind: SelectionPayloadKind::Raw,
        },
        bytes,
        value,
    ))
    .map(|(commitment, bytes, value)| (bytes, commitment, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry_arguments::postcard_bytes_to_tree;
    use crate::input::tree_value_from_serialize;
    use raster_core::input::{ExternalEncoding, SchemaField, Selectable};
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct EntryA {
        value: u64,
    }

    impl Selectable for EntryA {
        fn schema() -> SchemaNode {
            SchemaNode::Struct {
                type_name: "EntryA".into(),
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

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct EntryB {
        name: String,
    }

    impl Selectable for EntryB {
        fn schema() -> SchemaNode {
            SchemaNode::Struct {
                type_name: "EntryB".into(),
                fields: vec![SchemaField::new(
                    "name",
                    "name",
                    SchemaNode::Leaf {
                        type_name: "String".into(),
                    },
                )],
            }
        }
    }

    struct FakeResolver {
        files: BTreeMap<String, (Vec<u8>, String)>,
    }

    impl SourceResolver for FakeResolver {
        fn manifest_commitment_metadata(&self, name: &str) -> Result<(ExternalEncoding, String)> {
            let (_, commitment) = self
                .files
                .get(name)
                .cloned()
                .ok_or_else(|| Error::Other(format!("no fixture for '{}'", name)))?;
            Ok((ExternalEncoding::Postcard, commitment))
        }

        fn resolve(&self, name: &str) -> Result<ResolvedSourceData> {
            let (bytes, commitment) = self
                .files
                .get(name)
                .cloned()
                .ok_or_else(|| Error::Other(format!("no fixture for '{}'", name)))?;
            Ok(ResolvedSourceData::Postcard {
                commitment,
                file: crate::source::SourceFile::Memory(Arc::from(bytes.into_boxed_slice())),
            })
        }
    }

    fn referenced_object_fixture() -> (ReferencedObject, FakeResolver) {
        let a = EntryA { value: 42 };
        let b = EntryB {
            name: "hello".into(),
        };
        let a_bytes = raster_core::postcard::to_allocvec(&a).unwrap();
        let b_bytes = raster_core::postcard::to_allocvec(&b).unwrap();
        let (_, a_root) =
            subtree_payload_and_root(&tree_value_from_serialize(&a).unwrap()).unwrap();
        let (_, b_root) =
            subtree_payload_and_root(&tree_value_from_serialize(&b).unwrap()).unwrap();

        let sources = vec![
            ReferencedSource {
                name: "entry_a".into(),
                commitment: a_root.to_vec(),
                kind: ReferencedSourceKind::Postcard {
                    to_tree: postcard_bytes_to_tree::<EntryA>,
                    schema: EntryA::schema,
                },
            },
            ReferencedSource {
                name: "entry_b".into(),
                commitment: b_root.to_vec(),
                kind: ReferencedSourceKind::Postcard {
                    to_tree: postcard_bytes_to_tree::<EntryB>,
                    schema: EntryB::schema,
                },
            },
        ];
        let referenced = ReferencedObject { sources };

        let mut files = BTreeMap::new();
        files.insert("entry_a".to_string(), (a_bytes, hex_string(&a_root)));
        files.insert("entry_b".to_string(), (b_bytes, hex_string(&b_root)));
        (referenced, FakeResolver { files })
    }

    #[test]
    fn referenced_object_selects_named_source_field() {
        let (referenced, resolver) = referenced_object_fixture();
        let selector = SelectorPath::new(vec![
            SelectorSegment::Field("entry_a".into()),
            SelectorSegment::Field("value".into()),
        ]);

        let (tree, selected) = referenced.select(&selector, &resolver).unwrap();

        assert_eq!(typed_value_from_tree::<u64>(&tree).unwrap(), 42);
        assert_eq!(
            selected.commitment.source_root_hash.to_vec(),
            referenced.combined_root()
        );
    }

    #[test]
    fn referenced_object_selection_witness_verifies_against_combined_root() {
        let (referenced, resolver) = referenced_object_fixture();
        let selector = SelectorPath::new(vec![
            SelectorSegment::Field("entry_b".into()),
            SelectorSegment::Field("name".into()),
        ]);

        let (_, selected) = referenced.select(&selector, &resolver).unwrap();
        let witness = referenced
            .selection_witness(&selector, SelectionPayloadKind::Raw, &resolver)
            .unwrap();

        assert!(raster_core::input::verify_selection_witness(
            &selected.commitment,
            &witness
        ));
    }

    #[test]
    fn referenced_object_rejects_unknown_argument_name() {
        let (referenced, resolver) = referenced_object_fixture();
        let selector = SelectorPath::new(vec![SelectorSegment::Field("missing".into())]);

        let err = referenced.select(&selector, &resolver).unwrap_err();

        assert!(err.to_string().contains("Unknown entry argument"));
    }

    #[test]
    fn referenced_object_rejects_tampered_source_bytes() {
        let (referenced, mut resolver) = referenced_object_fixture();
        resolver.files.get_mut("entry_a").unwrap().0 =
            raster_core::postcard::to_allocvec(&EntryA { value: 999 }).unwrap();
        let selector = SelectorPath::new(vec![
            SelectorSegment::Field("entry_a".into()),
            SelectorSegment::Field("value".into()),
        ]);

        let err = referenced.select(&selector, &resolver).unwrap_err();

        assert!(err
            .to_string()
            .contains("failed structural integrity check"));
    }

    #[test]
    fn combined_root_uses_the_shared_struct_commitment_convention() {
        let (referenced, _resolver) = referenced_object_fixture();

        // Deliberately not a hand-rolled hash: the whole point of sharing
        // `struct_commitments_root` is that the guest recomputes this by
        // calling the same function, so a local re-implementation here would
        // only test that two copies agree, not that there is one.
        let expected = struct_commitments_root(
            referenced
                .sources
                .iter()
                .map(|source| (source.name.as_str(), source.commitment.as_slice())),
        )
        .to_vec();

        assert_eq!(referenced.combined_root(), expected);
    }

    #[test]
    fn combined_root_distinguishes_entry_arguments_by_name() {
        let (referenced, _resolver) = referenced_object_fixture();
        let renamed = ReferencedObject {
            sources: referenced
                .sources
                .iter()
                .enumerate()
                .map(|(index, source)| ReferencedSource {
                    name: format!("renamed_{}", index),
                    commitment: source.commitment.clone(),
                    kind: source.kind.clone(),
                })
                .collect(),
        };

        assert_ne!(referenced.combined_root(), renamed.combined_root());
    }

    fn tamper_u64_leaf(data: &mut [u8], index: &RasterIndex, field: &str, value: u64) {
        let node = index.get_node(index.root_node).unwrap();
        let crate::raster_index::RasterNodeKind::Struct { fields } = &node.kind else {
            panic!("expected Bytes struct root");
        };
        let child = fields
            .iter()
            .find(|f| f.name == field)
            .unwrap()
            .child;
        let position = index
            .child_position(index.root_position().unwrap(), child)
            .unwrap();
        let start = position as usize + 9;
        data[start..start + 8].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn load_rejects_page_size_leaf_that_disagrees_with_schema() {
        let region = raster_core::Bytes::<4>::paged(vec![1, 2, 3, 4, 5]).unwrap();
        let (mut data, index_bytes, _) = crate::encode_raster_value(&region).unwrap();
        let index = RasterIndex::from_bytes(&index_bytes).unwrap();
        tamper_u64_leaf(&mut data, &index, "page_size", 8);
        let err = check_schema_page_sizes(&raster_core::Bytes::<4>::schema(), &index, &data)
            .unwrap_err();
        assert!(matches!(
            err,
            Error::PageSizeMismatch {
                declared: 4,
                artifact: 8
            }
        ));
    }

    #[test]
    fn load_rejects_page_count_that_disagrees_with_ceil() {
        let region = raster_core::Bytes::<4>::paged(vec![1, 2, 3, 4, 5]).unwrap();
        let (mut data, index_bytes, _) = crate::encode_raster_value(&region).unwrap();
        let index = RasterIndex::from_bytes(&index_bytes).unwrap();
        // 5 bytes / 4 → 2 pages; claiming byte_len = 4 expects 1 page.
        tamper_u64_leaf(&mut data, &index, "byte_len", 4);
        let err = check_schema_page_sizes(&raster_core::Bytes::<4>::schema(), &index, &data)
            .unwrap_err();
        assert!(matches!(err, Error::PageShape { .. }));
    }
}
