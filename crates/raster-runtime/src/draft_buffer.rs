//! A recur site's object under construction, held in its encoded form.
//!
//! The `DraftBuffer` replaces the decoded `values` a draft used to keep and
//! re-encode at its close (`incremental-draft-materialization` §Mechanism, §The
//! draft buffer). Per append field it keeps what the stored object will hold:
//!
//! - the inline list's element region, `(len:u64 ‖ payload)*`, appended to;
//! - each element's index nodes, in one arena shared by the whole draft, so a
//!   node id is final the moment its element is pushed;
//! - the list's Merkle levels, updated along the right spine — `O(log N)`
//!   hashes per push, the same work the frontier did, and the frontier is read
//!   back off them.
//!
//! The seal then writes the struct and list headers around those buffers. No
//! element is re-encoded or re-hashed, and because index offsets are
//! parent-relative (`rindex04`) no node is rewritten either. The sealed
//! payload, root and every selection equal what `encode_raster_value` gives
//! for the same value — the invariant the tests below hold it to.
//!
//! Unauthenticated runs store nothing and prove nothing, so they keep the
//! decoded values instead and materialize the typed object from them.

use std::collections::BTreeMap;

use raster_core::draft::{
    draft_root_from_field_roots, draft_tree_from_fields, DraftFieldValue, DraftOp,
    DraftStateWitness, DraftValue, DraftWitnessField,
};
use raster_core::input::{
    list_node_hash, list_root_from_elements_root, struct_commitments_root, AppendFrontier, Hash32,
    SchemaFieldMode, SchemaNode,
};
use raster_core::tree::{subtree_payload_and_root, LIST_HANDLE_HEADER_LEN};
use raster_core::{Error, Result};

use std::sync::Arc;

use crate::backing::{DerivedObject, ObjectBytes, PieceSource, PieceTable};
use raster_core::trace::{DerivedPayload, DerivedSegment};
use crate::input::TreeValue;
use crate::raster_encode::{encode_indexed, place_child};
use crate::raster_index::{RasterIndex, RasterMerkleLevel, RasterNode, RasterNodeKind, RasterStructField};

/// Stand-in root for a draft in an unauthenticated run (see `storage.rs`).
pub(crate) const UNAUTHENTICATED_DRAFT_ROOT: [u8; 32] = [0u8; 32];

/// One append field's encoded list — the whole list for a creating site, or,
/// for a deriving one, what it adds to its base's list.
#[derive(Debug, Clone, Default)]
pub(crate) struct AppendBuffer {
    len: u64,
    /// The inline `0x02` list's element region — the part this buffer wrote:
    /// `(len:u64 ‖ payload)*`.
    body: Vec<u8>,
    /// Element node ids in the draft's arena — the elements this buffer wrote.
    elements: Vec<u64>,
    /// Merkle levels, leaves first, each pairing the one below with its last
    /// node duplicated when odd. For a continuation, level `h` holds only the
    /// nodes from index `base.len >> h` on: the ones before are complete in
    /// the base and read from it.
    levels: Vec<Vec<Hash32>>,
    /// The base list this one continues, for a deriving site.
    base: Option<BaseList>,
    /// Unauthenticated runs only: the decoded elements.
    values: Vec<DraftValue>,
}

/// A base object's list a deriving site appends to: read through the base's
/// index, never copied.
#[derive(Debug, Clone)]
struct BaseList {
    index: Arc<RasterIndex>,
    node: u64,
    len: u64,
    /// Bytes of the base's element region, behind its `0x02 ‖ count` header.
    body_len: u64,
}

impl AppendBuffer {
    fn continuing(base: BaseList) -> Result<Self> {
        let mut levels = Vec::new();
        for height in 0..base.index.list_level_count(base.node)? {
            let width = base.index.list_level_width(base.node, height)?;
            let complete = (base.len >> height) as usize;
            levels.push(
                (complete..width)
                    .map(|j| base.index.list_level_node(base.node, height, j))
                    .collect::<Result<Vec<_>>>()?,
            );
        }
        Ok(Self {
            len: base.len,
            levels,
            base: Some(base),
            ..Self::default()
        })
    }

    fn base_len(&self) -> u64 {
        self.base.as_ref().map_or(0, |base| base.len)
    }

    /// Nodes of level `height` that are complete in the base.
    fn complete(&self, height: usize) -> usize {
        (self.base_len() >> height) as usize
    }

    fn level_width(&self, height: usize) -> usize {
        let mut width = self.len;
        for _ in 0..height {
            width = width / 2 + width % 2;
        }
        width as usize
    }

    fn level_count(&self) -> usize {
        if self.len == 0 {
            return 0;
        }
        let mut count = 1;
        let mut width = self.len;
        while width > 1 {
            width = width / 2 + width % 2;
            count += 1;
        }
        count
    }

    fn level_node(&self, height: usize, j: usize) -> Hash32 {
        let complete = self.complete(height);
        match &self.base {
            Some(base) if j < complete => base
                .index
                .list_level_node(base.node, height, j)
                .expect("a base list's complete nodes are in its index"),
            _ => self.levels[height][j - complete],
        }
    }

    /// The list's raster root — the field's root.
    fn root(&self) -> Hash32 {
        let count = self.level_count();
        let top = (count > 0).then(|| self.level_node(count - 1, 0));
        list_root_from_elements_root(self.len, top.as_ref())
    }

    /// The right edge the witness carries, read off the levels in `O(log N)`:
    /// the last leaf, and the left sibling at every level where the path from
    /// it is a right child. A left sibling of the edge always covers a
    /// complete subtree, so no padding enters it — and for a continuation it
    /// is read from the base's index without touching an element.
    pub(crate) fn frontier(&self) -> AppendFrontier {
        if self.len == 0 {
            return AppendFrontier::empty();
        }
        let last = self.len - 1;
        let mut ommers = Vec::new();
        for height in 0..self.level_count() {
            if self.level_width(height) <= 1 {
                break;
            }
            let index = (last >> height) as usize;
            if index % 2 == 1 {
                ommers.push(self.level_node(height, index - 1));
            }
        }
        AppendFrontier {
            len: self.len,
            leaf: Some(self.level_node(0, last as usize)),
            ommers,
        }
    }

    /// Fold one element root into the levels along the right spine.
    fn push_leaf(&mut self, leaf: Hash32) {
        if self.levels.is_empty() {
            self.levels.push(Vec::new());
        }
        self.levels[0].push(leaf);
        self.len += 1;
        let mut height = 0;
        loop {
            let width = self.level_width(height);
            if width <= 1 {
                break;
            }
            let parent = (width - 1) / 2;
            let left = self.level_node(height, 2 * parent);
            let right = if 2 * parent + 1 < width {
                self.level_node(height, 2 * parent + 1)
            } else {
                left
            };
            let node = list_node_hash(&left, &right);
            if height + 1 == self.levels.len() {
                self.levels.push(Vec::new());
            }
            let slot = parent - self.complete(height + 1);
            let above = &mut self.levels[height + 1];
            if slot < above.len() {
                above[slot] = node;
            } else {
                above.push(node);
            }
            height += 1;
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum BufferField {
    Set { value: DraftValue, root: Hash32 },
    Append(AppendBuffer),
}

impl BufferField {
    fn root(&self) -> Hash32 {
        match self {
            Self::Set { root, .. } => *root,
            Self::Append(list) => list.root(),
        }
    }
}

/// The sealed object: what storage holds at the site's coordinate, and what
/// the site's close carries into the trace — one encoding for both.
pub(crate) struct SealedObject {
    pub payload: Vec<u8>,
    pub index: RasterIndex,
    pub root: Hash32,
}

#[derive(Debug, Clone)]
pub(crate) struct DraftBuffer {
    pub schema: SchemaNode,
    pub current_root: [u8; 32],
    pub fields: BTreeMap<String, BufferField>,
    pub ops: Vec<DraftOp>,
    authenticated: bool,
    /// Index nodes of every encoded element, in push order.
    nodes: Vec<RasterNode>,
    /// The object a deriving site continues; `None` for a creating site.
    base: Option<DerivationBase>,
}

/// The stored object a deriving site extends, as a raster read sees it.
#[derive(Debug, Clone)]
pub(crate) struct DerivationBase {
    pub coordinates: raster_core::cfs::CfsCoordinates,
    pub commitment: Vec<u8>,
    pub index: Arc<RasterIndex>,
    pub bytes: ObjectBytes,
}

impl DraftBuffer {
    pub(crate) fn new(schema: SchemaNode, authenticated: bool) -> Result<Self> {
        let current_root = if authenticated {
            draft_root_from_field_roots(&schema, &BTreeMap::new())?
        } else {
            UNAUTHENTICATED_DRAFT_ROOT
        };
        Ok(Self {
            schema,
            current_root,
            fields: BTreeMap::new(),
            ops: Vec::new(),
            authenticated,
            nodes: Vec::new(),
            base: None,
        })
    }

    /// A deriving site's draft, opened on its base object without decoding a
    /// list: set-once fields are decoded (a witness carries their values),
    /// each list continues from the base's stored Merkle levels in
    /// `O(log N)`. Its root is the base's commitment.
    pub(crate) fn derive(schema: SchemaNode, base: DerivationBase) -> Result<Self> {
        let mut buffer = Self::new(schema, true)?;
        let root_position = base.index.root_position()?;
        let RasterNodeKind::Struct { fields } = &base.index.get_node(base.index.root_node)?.kind
        else {
            return Err(Error::Other("A derived draft's base is not a struct".into()));
        };
        for field in fields {
            let position = base.index.child_position(root_position, field.child)?;
            match buffer.field_mode(&field.name)? {
                SchemaFieldMode::SetOnce => {
                    let value = crate::input::tree_value_from_raster_node(
                        &base.index,
                        &base.bytes,
                        field.child,
                        position,
                    )?;
                    let root = base.index.get_node(field.child)?.root_hash;
                    buffer
                        .fields
                        .insert(field.name.clone(), BufferField::Set { value, root });
                }
                SchemaFieldMode::AppendOnlyVec => {
                    let len = base.index.list_len(field.child)?.ok_or_else(|| {
                        Error::Other(format!(
                            "Derived draft base field '{}' is not a list",
                            field.name
                        ))
                    })?;
                    let body_len = base.index.get_node(field.child)?.len - 1 - 8;
                    let list = AppendBuffer::continuing(BaseList {
                        index: base.index.clone(),
                        node: field.child,
                        len,
                        body_len,
                    })?;
                    buffer
                        .fields
                        .insert(field.name.clone(), BufferField::Append(list));
                }
            }
        }
        buffer.recompose_root()?;
        if buffer.current_root.as_slice() != base.commitment.as_slice() {
            return Err(Error::Other(format!(
                "A derived draft opened at {:?}, not its base's commitment",
                buffer.current_root
            )));
        }
        buffer.base = Some(base);
        Ok(buffer)
    }

    pub(crate) fn is_derived(&self) -> bool {
        self.base.is_some()
    }

    pub(crate) fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// Recompose the draft root from the per-field roots — `O(#fields)`.
    pub(crate) fn recompose_root(&mut self) -> Result<[u8; 32]> {
        if !self.authenticated {
            return Ok(UNAUTHENTICATED_DRAFT_ROOT);
        }
        let roots = self
            .fields
            .iter()
            .map(|(name, field)| (name.clone(), field.root()))
            .collect();
        self.current_root = draft_root_from_field_roots(&self.schema, &roots)?;
        Ok(self.current_root)
    }

    fn field_mode(&self, field: &str) -> Result<SchemaFieldMode> {
        schema_fields(&self.schema)?
            .iter()
            .find(|candidate| candidate.name == field)
            .map(|candidate| candidate.mode)
            .ok_or_else(|| Error::Other(format!("Unknown draft field '{}'", field)))
    }

    /// Write a set-once field. Its value is encoded at the seal: set-once
    /// fields are small and written once, so nothing is gained earlier.
    pub(crate) fn set(&mut self, field: &str, value: DraftValue) -> Result<()> {
        self.write_set(field, value, true)
    }

    /// Append to a list field — see [`Self::write_push`].
    pub(crate) fn push(&mut self, field: &str, value: DraftValue) -> Result<()> {
        self.write_push(field, value, true)
    }

    /// Write a base object's field into a deriving site's draft. Not an op:
    /// the base was written by the step that produced it, and the draft's
    /// root after adoption is the base's commitment.
    pub(crate) fn adopt(&mut self, field: &str, value: DraftValue) -> Result<()> {
        match self.field_mode(field)? {
            SchemaFieldMode::SetOnce => self.write_set(field, value, false),
            SchemaFieldMode::AppendOnlyVec => match value {
                DraftValue::ListHandle(elements) | DraftValue::List(elements) => {
                    self.fields
                        .entry(field.to_string())
                        .or_insert_with(|| BufferField::Append(AppendBuffer::default()));
                    for element in elements {
                        self.write_push(field, element, false)?;
                    }
                    Ok(())
                }
                _ => Err(Error::Other(format!(
                    "Derived draft base field '{}' is not a list",
                    field
                ))),
            },
        }
    }

    fn write_set(&mut self, field: &str, value: DraftValue, record: bool) -> Result<()> {
        if self.field_mode(field)? != SchemaFieldMode::SetOnce {
            return Err(Error::Other(format!(
                "Draft field '{}' does not support set; use push",
                field
            )));
        }
        if self.fields.contains_key(field) {
            return Err(Error::Other(format!(
                "Draft field '{}' can only be written once",
                field
            )));
        }
        let root = if self.authenticated {
            subtree_payload_and_root(&value)?.1
        } else {
            UNAUTHENTICATED_DRAFT_ROOT
        };
        if self.authenticated && record {
            self.ops.push(DraftOp::Set {
                field: field.to_string(),
                value: value.clone(),
            });
        }
        self.fields
            .insert(field.to_string(), BufferField::Set { value, root });
        self.recompose_root()?;
        Ok(())
    }

    /// Append to a list field: encode the element once — payload, root and
    /// index nodes together — and fold its root into the levels.
    fn write_push(&mut self, field: &str, value: DraftValue, record: bool) -> Result<()> {
        if self.field_mode(field)? != SchemaFieldMode::AppendOnlyVec {
            return Err(Error::Other(format!(
                "Draft field '{}' does not support push; use set",
                field
            )));
        }
        let authenticated = self.authenticated;
        let list = match self
            .fields
            .entry(field.to_string())
            .or_insert_with(|| BufferField::Append(AppendBuffer::default()))
        {
            BufferField::Append(list) => list,
            BufferField::Set { .. } => {
                return Err(Error::Other(format!(
                    "Draft field '{}' is not appendable",
                    field
                )))
            }
        };
        if !authenticated {
            list.values.push(value);
            list.len += 1;
            return Ok(());
        }
        let encoded = encode_indexed(&value, &mut self.nodes)?;
        // The element's payload starts past the inline list's `0x02 ‖ count`,
        // the elements before it, and its own length prefix — relative to the
        // inline list node, which is where its index node is placed.
        let base_body = list.base.as_ref().map_or(0, |base| base.body_len);
        place_child(
            &mut self.nodes,
            encoded.node,
            1 + 8 + base_body + list.body.len() as u64 + 8,
            0,
        );
        list.body
            .extend_from_slice(&(encoded.payload.len() as u64).to_le_bytes());
        list.body.extend_from_slice(&encoded.payload);
        list.elements.push(encoded.node);
        list.push_leaf(encoded.root);
        if record {
            self.ops.push(DraftOp::Push {
                field: field.to_string(),
                value,
            });
        }
        self.recompose_root()?;
        Ok(())
    }

    /// What a tile step's witness carries: set-once values, list frontiers.
    pub(crate) fn witness(&self) -> DraftStateWitness {
        DraftStateWitness {
            schema: self.schema.clone(),
            fields: self
                .fields
                .iter()
                .map(|(name, field)| {
                    let witness = match field {
                        BufferField::Set { value, .. } => DraftWitnessField::Set(value.clone()),
                        BufferField::Append(list) => DraftWitnessField::Append(list.frontier()),
                    };
                    (name.clone(), witness)
                })
                .collect(),
        }
    }

    /// The first set-once field never written, for an empty sweep's error.
    pub(crate) fn first_unset_set_once_field(&self) -> Result<Option<String>> {
        Ok(schema_fields(&self.schema)?
            .iter()
            .find(|field| field.mode == SchemaFieldMode::SetOnce && !self.fields.contains_key(&field.name))
            .map(|field| field.name.clone()))
    }

    /// The object with every list emptied — enough to check that a partial
    /// object (an empty sweep's) deserializes as `S` at all, in
    /// `O(set-once fields)`, before the seal stores it.
    pub(crate) fn shape_without_lists(&self) -> Result<TreeValue> {
        Ok(TreeValue::Struct(
            schema_fields(&self.schema)?
                .iter()
                .map(|field| {
                    let value = match (field.mode, self.fields.get(&field.name)) {
                        (SchemaFieldMode::AppendOnlyVec, _) => TreeValue::ListHandle(Vec::new()),
                        (_, Some(BufferField::Set { value, .. })) => value.clone(),
                        _ => TreeValue::Unit,
                    };
                    (field.name.clone(), value)
                })
                .collect(),
        ))
    }

    /// The decoded object, for an unauthenticated run (which keeps values).
    pub(crate) fn materialize(&self, require_complete: bool) -> Result<TreeValue> {
        let fields: BTreeMap<String, DraftFieldValue> = self
            .fields
            .iter()
            .map(|(name, field)| {
                let value = match field {
                    BufferField::Set { value, .. } => DraftFieldValue::Set(value.clone()),
                    BufferField::Append(list) => DraftFieldValue::Append(list.values.clone()),
                };
                (name.clone(), value)
            })
            .collect();
        draft_tree_from_fields(&self.schema, &fields, require_complete)
    }

    /// Seal the object: the struct's payload around the field buffers, its
    /// index around the element arena, and its root from the field roots —
    /// with no element encoded or hashed again.
    ///
    /// Fields in schema (declaration) order, the order the struct's payload
    /// and root fold them in. An unwritten set-once field is `Unit` when
    /// `require_complete` is false, else an error; an unwritten list is empty.
    pub(crate) fn seal(self, require_complete: bool) -> Result<SealedObject> {
        if !self.authenticated {
            return Err(Error::Other(
                "An unauthenticated draft holds no encoded object to seal".into(),
            ));
        }
        if self.base.is_some() {
            return Err(Error::Other(
                "A derived draft seals as a derived object (seal_derived)".into(),
            ));
        }
        let DraftBuffer {
            schema,
            current_root,
            mut fields,
            mut nodes,
            ..
        } = self;
        let schema_fields = schema_fields(&schema)?;

        let mut payload = Vec::new();
        payload.push(0x01);
        payload.extend_from_slice(&(schema_fields.len() as u64).to_le_bytes());
        let mut index_fields = Vec::with_capacity(schema_fields.len());
        let mut field_roots = Vec::with_capacity(schema_fields.len());

        for schema_field in schema_fields {
            let name = &schema_field.name;
            payload.extend_from_slice(&(name.len() as u64).to_le_bytes());
            payload.extend_from_slice(name.as_bytes());
            match (schema_field.mode, fields.remove(name)) {
                (SchemaFieldMode::AppendOnlyVec, field) => {
                    let list = match field {
                        Some(BufferField::Append(list)) => list,
                        None => AppendBuffer::default(),
                        Some(BufferField::Set { .. }) => {
                            return Err(Error::Other(format!(
                                "Draft field '{}' holds a set value but is a list",
                                name
                            )))
                        }
                    };
                    let root = list.root();
                    let inner_len = 1 + 8 + list.body.len() as u64;
                    payload.extend_from_slice(&(LIST_HANDLE_HEADER_LEN + inner_len).to_le_bytes());
                    let handle_start = payload.len() as u64;
                    payload.push(0x09);
                    payload.extend_from_slice(&root);
                    payload.extend_from_slice(&list.len.to_le_bytes());
                    payload.extend_from_slice(&inner_len.to_le_bytes());
                    payload.push(0x02);
                    payload.extend_from_slice(&list.len.to_le_bytes());
                    payload.extend_from_slice(&list.body);

                    let node = nodes.len() as u64;
                    nodes.push(RasterNode {
                        offset: handle_start + LIST_HANDLE_HEADER_LEN,
                        len: inner_len,
                        root_hash: root,
                        kind: RasterNodeKind::List {
                            len: list.len,
                            elements: list.elements,
                            merkle_levels: list
                                .levels
                                .into_iter()
                                .map(|hashes| RasterMerkleLevel { hashes })
                                .collect(),
                        },
                    });
                    index_fields.push(RasterStructField {
                        name: name.clone(),
                        child: node,
                    });
                    field_roots.push(root);
                }
                (SchemaFieldMode::SetOnce, field) => {
                    let value = match field {
                        Some(BufferField::Set { value, .. }) => value,
                        None if !require_complete => DraftValue::Unit,
                        None => {
                            return Err(Error::Other(format!(
                                "Draft field '{}' must be written before finalize",
                                name
                            )))
                        }
                        Some(BufferField::Append(_)) => {
                            return Err(Error::Other(format!(
                                "Draft field '{}' holds a list but is set-once",
                                name
                            )))
                        }
                    };
                    let encoded = encode_indexed(&value, &mut nodes)?;
                    payload.extend_from_slice(&(encoded.payload.len() as u64).to_le_bytes());
                    place_child(&mut nodes, encoded.node, payload.len() as u64, 0);
                    payload.extend_from_slice(&encoded.payload);
                    index_fields.push(RasterStructField {
                        name: name.clone(),
                        child: encoded.node,
                    });
                    field_roots.push(encoded.root);
                }
            }
        }

        let root = struct_commitments_root(
            schema_fields
                .iter()
                .map(|field| field.name.as_str())
                .zip(field_roots.iter().map(|root| root.as_slice())),
        );
        if root != current_root {
            return Err(Error::Other(format!(
                "Sealed draft root {:?} does not match the draft's root {:?}",
                root, current_root
            )));
        }
        let root_node = nodes.len() as u64;
        nodes.push(RasterNode {
            offset: 0,
            len: payload.len() as u64,
            root_hash: root,
            kind: RasterNodeKind::Struct {
                fields: index_fields,
            },
        });
        Ok(SealedObject {
            payload,
            index: RasterIndex::new(root_node, root, nodes),
            root,
        })
    }
}

impl DraftBuffer {
    /// Seal a deriving site's object (`incremental-draft-materialization`
    /// §How a derived object maps onto buffers): the base's bytes and index
    /// nodes shared, plus the appended elements, the headers that changed, and
    /// new nodes only where an offset moved — a grown list's continuation, a
    /// field laid out after it, the struct root. Returns the object for the
    /// child's store and the delta for the site's close event.
    ///
    /// Fields are walked in the base's payload order, the declaration order
    /// its struct node lists them in. A field's offsets inside it are
    /// parent-relative, so only its own node is rewritten when it moves.
    pub(crate) fn seal_derived(self) -> Result<(DerivedObject, DerivedPayload)> {
        let DraftBuffer {
            schema,
            current_root,
            mut fields,
            nodes: local_nodes,
            base,
            ..
        } = self;
        let base = base.ok_or_else(|| Error::Other("Not a derived draft".into()))?;
        let base_count = base.index.total_len();
        let mut overlay: Vec<RasterNode> = local_nodes
            .into_iter()
            .map(|node| shift_node_ids(node, base_count))
            .collect();

        let base_root = base.index.get_node(base.index.root_node)?;
        if base_root.offset != 0 {
            return Err(Error::Other("A derived draft's base must start its payload".into()));
        }
        let RasterNodeKind::Struct {
            fields: base_fields,
        } = &base_root.kind
        else {
            return Err(Error::Other("A derived draft's base is not a struct".into()));
        };

        let mut segments = vec![DerivedSegment::Base { offset: 0, len: 1 + 8 }];
        let mut tail = Vec::new();
        let mut cursor: u64 = 1 + 8;
        let mut new_fields = Vec::with_capacity(base_fields.len());
        let mut roots = Vec::with_capacity(base_fields.len());

        for base_field in base_fields {
            let child = base.index.get_node(base_field.child)?;
            let mode = schema_fields(&schema)?
                .iter()
                .find(|field| field.name == base_field.name)
                .map(|field| field.mode)
                .ok_or_else(|| {
                    Error::Other(format!("Unknown draft field '{}'", base_field.name))
                })?;
            let shift = match mode {
                SchemaFieldMode::AppendOnlyVec => LIST_HANDLE_HEADER_LEN,
                SchemaFieldMode::SetOnce => 0,
            };
            let name_len = base_field.name.len() as u64;
            let base_start = child.offset - shift;
            let base_len = child.len + shift;
            segments.push(DerivedSegment::Base {
                offset: base_start - 8 - name_len - 8,
                len: 8 + name_len,
            });
            cursor += 8 + name_len;

            let grown = match fields.remove(&base_field.name) {
                Some(BufferField::Append(list)) if list.len > list.base_len() => Some(list),
                _ => None,
            };
            match grown {
                Some(list) => {
                    let base_list = list.base.as_ref().expect("a derived list continues its base");
                    let root = list.root();
                    let inner_len = 1 + 8 + base_list.body_len + list.body.len() as u64;
                    let header_start = tail.len() as u64;
                    tail.extend_from_slice(&(LIST_HANDLE_HEADER_LEN + inner_len).to_le_bytes());
                    tail.push(0x09);
                    tail.extend_from_slice(&root);
                    tail.extend_from_slice(&list.len.to_le_bytes());
                    tail.extend_from_slice(&inner_len.to_le_bytes());
                    tail.push(0x02);
                    tail.extend_from_slice(&list.len.to_le_bytes());
                    segments.push(DerivedSegment::Tail {
                        offset: header_start,
                        len: tail.len() as u64 - header_start,
                    });
                    cursor += 8;
                    let payload_start = cursor;
                    segments.push(DerivedSegment::Base {
                        offset: base_start + LIST_HANDLE_HEADER_LEN + 1 + 8,
                        len: base_list.body_len,
                    });
                    let body_start = tail.len() as u64;
                    tail.extend_from_slice(&list.body);
                    segments.push(DerivedSegment::Tail {
                        offset: body_start,
                        len: list.body.len() as u64,
                    });
                    cursor += LIST_HANDLE_HEADER_LEN + inner_len;

                    let id = base_count + overlay.len() as u64;
                    overlay.push(RasterNode {
                        offset: payload_start + LIST_HANDLE_HEADER_LEN,
                        len: inner_len,
                        root_hash: root,
                        kind: RasterNodeKind::ListContinuation {
                            base_list: base_field.child,
                            len: list.len,
                            extra_elements: list
                                .elements
                                .iter()
                                .map(|element| element + base_count)
                                .collect(),
                            level_tails: list
                                .levels
                                .into_iter()
                                .map(|hashes| RasterMerkleLevel { hashes })
                                .collect(),
                        },
                    });
                    new_fields.push(RasterStructField {
                        name: base_field.name.clone(),
                        child: id,
                    });
                    roots.push(root);
                }
                None => {
                    segments.push(DerivedSegment::Base {
                        offset: base_start - 8,
                        len: 8 + base_len,
                    });
                    cursor += 8;
                    let payload_start = cursor;
                    cursor += base_len;
                    let id = if payload_start == base_start {
                        base_field.child
                    } else {
                        // Moved by a grown list before it: a new node with the
                        // new offset; its children are relative to it and stay.
                        let id = base_count + overlay.len() as u64;
                        overlay.push(RasterNode {
                            offset: payload_start + shift,
                            ..child.clone()
                        });
                        id
                    };
                    new_fields.push(RasterStructField {
                        name: base_field.name.clone(),
                        child: id,
                    });
                    roots.push(child.root_hash);
                }
            }
        }

        let root = struct_commitments_root(
            new_fields
                .iter()
                .map(|field| field.name.as_str())
                .zip(roots.iter().map(|root| root.as_slice())),
        );
        if root != current_root {
            return Err(Error::Other(format!(
                "Sealed derived root {:?} does not match the draft's root {:?}",
                root, current_root
            )));
        }
        let root_node = base_count + overlay.len() as u64;
        overlay.push(RasterNode {
            offset: 0,
            len: cursor,
            root_hash: root,
            kind: RasterNodeKind::Struct { fields: new_fields },
        });

        let payload = DerivedPayload {
            base_coordinates: base.coordinates.clone(),
            base_commitment: base.commitment.clone(),
            base_node_count: base_count,
            tail,
            segments: merge_segments(segments),
            overlay_nodes: raster_core::postcard::to_allocvec(&overlay).map_err(|e| {
                Error::Serialization(format!("Failed to encode derived index nodes: {}", e))
            })?,
            root_node,
            root_hash: root,
        };
        let object = derived_object(&base.index, &base.bytes, &payload, overlay)?;
        Ok((object, payload))
    }
}

/// Rebuild a derived object from its base and its delta — the child's store
/// and the recorder both build it here, from their own copy of the base.
///
/// What is checked is what the delta could get wrong cheaply: the base it
/// names has the node count it was derived against, the index validates, the
/// pieces cover the root node exactly, and the root is the struct root of its
/// fields' roots. Element bytes are taken as given, as a contiguous object's
/// are (`internal_object_commitment`).
pub(crate) fn derived_object(
    base_index: &Arc<RasterIndex>,
    base_bytes: &ObjectBytes,
    payload: &DerivedPayload,
    overlay: Vec<RasterNode>,
) -> Result<DerivedObject> {
    if base_index.total_len() != payload.base_node_count {
        return Err(Error::Other(format!(
            "Derived object expects a base of {} index nodes, found {}",
            payload.base_node_count,
            base_index.total_len()
        )));
    }
    let index = RasterIndex::layered(
        base_index.clone(),
        payload.root_node,
        payload.root_hash,
        overlay,
    );
    index.encode_check()?;
    let root_node = index.get_node(payload.root_node)?;
    let RasterNodeKind::Struct { fields } = &root_node.kind else {
        return Err(Error::Other("A derived object's root is not a struct".into()));
    };
    let mut roots = Vec::with_capacity(fields.len());
    for field in fields {
        roots.push(index.get_node(field.child)?.root_hash);
    }
    let root = struct_commitments_root(
        fields
            .iter()
            .map(|field| field.name.as_str())
            .zip(roots.iter().map(|root| root.as_slice())),
    );
    if root != payload.root_hash || root_node.root_hash != root {
        return Err(Error::Other("A derived object's root does not fold from its fields".into()));
    }

    let tail = Arc::new(payload.tail.clone());
    let mut pieces = PieceTable::default();
    for segment in &payload.segments {
        match *segment {
            DerivedSegment::Base { offset, len } => match base_bytes {
                ObjectBytes::Contiguous(object) => {
                    pieces.push(PieceSource::Object(object.clone()), offset, len)
                }
                ObjectBytes::Pieces(base_pieces) => {
                    for (source, from, take) in base_pieces.ranges(offset, len)? {
                        pieces.push(source, from, take);
                    }
                }
            },
            DerivedSegment::Tail { offset, len } => {
                if offset + len > tail.len() as u64 {
                    return Err(Error::Other("A derived object's tail is short".into()));
                }
                pieces.push(PieceSource::Tail(tail.clone()), offset, len)
            }
        }
    }
    if pieces.len != root_node.len {
        return Err(Error::Other(format!(
            "A derived object's pieces cover {} bytes, its root {}",
            pieces.len, root_node.len
        )));
    }
    Ok(DerivedObject {
        pieces: Arc::new(pieces),
        index: Arc::new(index),
        root: payload.root_hash,
        delta: None,
    })
}

/// Decode a delta's overlay nodes.
pub(crate) fn overlay_nodes(payload: &DerivedPayload) -> Result<Vec<RasterNode>> {
    raster_core::postcard::from_bytes(&payload.overlay_nodes)
        .map_err(|e| Error::Serialization(format!("Failed to decode derived index nodes: {}", e)))
}

fn merge_segments(segments: Vec<DerivedSegment>) -> Vec<DerivedSegment> {
    let mut merged: Vec<DerivedSegment> = Vec::with_capacity(segments.len());
    for segment in segments {
        match (merged.last_mut(), segment) {
            (_, DerivedSegment::Base { len: 0, .. } | DerivedSegment::Tail { len: 0, .. }) => {}
            (
                Some(DerivedSegment::Base { offset, len }),
                DerivedSegment::Base { offset: next, len: more },
            ) if *offset + *len == next => *len += more,
            (
                Some(DerivedSegment::Tail { offset, len }),
                DerivedSegment::Tail { offset: next, len: more },
            ) if *offset + *len == next => *len += more,
            _ => merged.push(segment),
        }
    }
    merged
}

/// Move a node encoded in a local arena into an index whose ids start at
/// `delta`.
fn shift_node_ids(mut node: RasterNode, delta: u64) -> RasterNode {
    match &mut node.kind {
        RasterNodeKind::Struct { fields } | RasterNodeKind::EnumStruct { fields, .. } => {
            for field in fields {
                field.child += delta;
            }
        }
        RasterNodeKind::List { elements, .. } | RasterNodeKind::EnumTuple { elements, .. } => {
            for element in elements {
                *element += delta;
            }
        }
        RasterNodeKind::Map { entries } => {
            for entry in entries {
                entry.key += delta;
                entry.value += delta;
            }
        }
        RasterNodeKind::EnumNewtype { child, .. } => *child += delta,
        RasterNodeKind::ListContinuation {
            base_list,
            extra_elements,
            ..
        } => {
            *base_list += delta;
            for element in extra_elements {
                *element += delta;
            }
        }
        RasterNodeKind::Unit | RasterNodeKind::Leaf { .. } | RasterNodeKind::EnumUnit { .. } => {}
    }
    node
}

fn schema_fields(schema: &SchemaNode) -> Result<&[raster_core::input::SchemaField]> {
    match schema {
        SchemaNode::Struct { fields, .. } => Ok(fields.as_slice()),
        _ => Err(Error::Other(
            "Drafts currently support only struct schemas at the root".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{encode_raster_value, tree_value_from_serialize};
    use crate::raster_encode::tests::assert_same_object;
    use raster_core::collections::List;
    use raster_core::input::{SchemaField, Selectable};
    use serde::Serialize;

    #[derive(Serialize, Clone)]
    struct Row {
        text: String,
        n: u32,
    }

    #[derive(Serialize)]
    struct Report {
        lines: List<String>,
        title: String,
        rows: List<Row>,
        count: u64,
    }

    fn row_schema() -> SchemaNode {
        SchemaNode::Struct {
            type_name: "Row".into(),
            fields: vec![
                SchemaField::new("text", "text", <String as Selectable>::schema()),
                SchemaField::new("n", "n", <u32 as Selectable>::schema()),
            ],
        }
    }

    fn report_schema() -> SchemaNode {
        SchemaNode::Struct {
            type_name: "Report".into(),
            fields: vec![
                SchemaField::new("lines", "lines", <List<String> as Selectable>::schema()),
                SchemaField::new("title", "title", <String as Selectable>::schema()),
                SchemaField::with_mode(
                    "rows",
                    "rows",
                    SchemaFieldMode::AppendOnlyVec,
                    SchemaNode::List {
                        type_name: "List<Row>".into(),
                        element: Box::new(row_schema()),
                    },
                ),
                SchemaField::new("count", "count", <u64 as Selectable>::schema()),
            ],
        }
    }

    fn tree<T: Serialize>(value: &T) -> DraftValue {
        tree_value_from_serialize(value).unwrap()
    }

    fn rows(n: usize) -> Vec<Row> {
        (0..n)
            .map(|i| Row {
                text: "r".repeat(i % 5),
                n: i as u32,
            })
            .collect()
    }

    /// Writes interleaved across fields, in any order: the seal must still lay
    /// the object out exactly as encoding it whole does.
    #[test]
    fn a_sealed_draft_is_the_object_encoded_whole() {
        for (n_lines, n_rows) in [(0, 0), (1, 0), (3, 7), (16, 1), (33, 9)] {
            let lines: Vec<String> = (0..n_lines).map(|i| format!("line {i}")).collect();
            let mut buffer = DraftBuffer::new(report_schema(), true).unwrap();
            buffer.set("count", tree(&42u64)).unwrap();
            for (i, row) in rows(n_rows).iter().enumerate() {
                buffer.push("rows", tree(row)).unwrap();
                if let Some(line) = lines.get(i) {
                    buffer.push("lines", tree(line)).unwrap();
                }
            }
            for line in lines.iter().skip(n_rows) {
                buffer.push("lines", tree(line)).unwrap();
            }
            buffer.set("title", tree(&String::from("T"))).unwrap();
            let draft_root = buffer.current_root;
            let sealed = buffer.seal(true).unwrap();

            let value = Report {
                lines: List::from(lines),
                title: "T".into(),
                rows: List::from(rows(n_rows)),
                count: 42,
            };
            let (payload, index_bytes, _) = encode_raster_value(&value).unwrap();
            let index = RasterIndex::from_bytes(&index_bytes).unwrap();
            assert_eq!(sealed.root, draft_root);
            assert_eq!(sealed.root, index.root_commitment);
            assert_eq!(sealed.payload, payload, "{n_lines} lines, {n_rows} rows");
            let sealed_index =
                RasterIndex::from_bytes(&sealed.index.encode().unwrap()).unwrap();
            assert_same_object(&sealed.payload, &sealed_index, &payload, &index);
        }
    }

    /// An empty sweep's partial object: unwritten set-once fields are `Unit`.
    #[test]
    fn an_incomplete_draft_seals_with_unit_fields_unless_completion_is_required() {
        let buffer = DraftBuffer::new(report_schema(), true).unwrap();
        assert!(buffer
            .clone()
            .seal(true)
            .err()
            .unwrap()
            .to_string()
            .contains("must be written before"));
        let sealed = buffer.seal(false).unwrap();
        let tree = DraftValue::Struct(vec![
            ("lines".into(), DraftValue::ListHandle(vec![])),
            ("title".into(), DraftValue::Unit),
            ("rows".into(), DraftValue::ListHandle(vec![])),
            ("count".into(), DraftValue::Unit),
        ]);
        let (payload, root) = subtree_payload_and_root(&tree).unwrap();
        assert_eq!((sealed.payload, sealed.root), (payload, root));
    }

    /// The frontier read off the stored levels is the frontier pushing builds
    /// — including the duplicate-last padding at every odd width.
    #[test]
    fn the_frontier_read_from_levels_is_the_pushed_frontier() {
        let mut list = AppendBuffer::default();
        let mut frontier = AppendFrontier::empty();
        for i in 0..40u8 {
            assert_eq!(list.frontier(), frontier, "after {i} elements");
            assert_eq!(Some(list.root()), frontier.root());
            let leaf = [i; 32];
            list.push_leaf(leaf);
            frontier.push(leaf);
        }
    }

    // ---- Derivation: a derived object is its base plus appends ----

    use crate::backing::RasterObject;
    use crate::input::RasterData;

    fn report(n_lines: usize, n_rows: usize) -> Report {
        Report {
            lines: List::from((0..n_lines).map(|i| format!("line {i}")).collect::<Vec<_>>()),
            title: "T".into(),
            rows: List::from(rows(n_rows)),
            count: 7,
        }
    }

    /// A contiguous stored object, as a base.
    fn stored(value: &Report) -> DerivationBase {
        let (payload, index_bytes, _) = encode_raster_value(value).unwrap();
        let index = RasterIndex::from_bytes(&index_bytes).unwrap();
        let root = index.root_commitment;
        let object = Arc::new(RasterObject::new(raster_core::trace::RasterPayload {
            bytes: payload,
            index_bytes,
            root_hash: root,
        }));
        DerivationBase {
            coordinates: raster_core::cfs::CfsCoordinates(vec![3]),
            commitment: root.to_vec(),
            index: object.index().unwrap(),
            bytes: ObjectBytes::Contiguous(object),
        }
    }

    /// A derived object as the next derivation's base.
    fn as_base(object: &DerivedObject) -> DerivationBase {
        DerivationBase {
            coordinates: raster_core::cfs::CfsCoordinates(vec![4]),
            commitment: object.root.to_vec(),
            index: object.index.clone(),
            bytes: ObjectBytes::Pieces(object.pieces.clone()),
        }
    }

    /// Derive from `base`, append `(k lines, j rows)` continuing after
    /// `(n_lines, n_rows)`, and hold the result to the contiguous encoding of
    /// the same value — through the child's object and the recorder's rebuild.
    fn derive_and_check(
        base: DerivationBase,
        (n_lines, n_rows): (usize, usize),
        (k, j): (usize, usize),
    ) -> DerivedObject {
        let mut buffer = DraftBuffer::derive(report_schema(), base.clone()).unwrap();
        assert_eq!(buffer.current_root.as_slice(), base.commitment.as_slice());
        let all_rows = rows(n_rows + j);
        for i in 0..k.max(j) {
            if i < k {
                buffer.push("lines", tree(&format!("line {}", n_lines + i))).unwrap();
            }
            if i < j {
                buffer.push("rows", tree(&all_rows[n_rows + i])).unwrap();
            }
        }
        let draft_root = buffer.current_root;
        let (object, payload) = buffer.seal_derived().unwrap();

        let expected = report(n_lines + k, n_rows + j);
        let (expected_payload, expected_index_bytes, _) = encode_raster_value(&expected).unwrap();
        let expected_index = RasterIndex::from_bytes(&expected_index_bytes).unwrap();
        assert_eq!(object.root, draft_root);
        assert_eq!(object.root, expected_index.root_commitment);

        let bytes = object.pieces.read_subtree(0, object.pieces.len).unwrap();
        assert_eq!(bytes, expected_payload, "derived bytes, flattened");
        assert_same_object(&bytes, &object.index, &expected_payload, &expected_index);
        // Exported: a flattened index answers the same, standalone.
        let exported = RasterIndex::from_bytes(&object.index.encode().unwrap()).unwrap();
        assert_same_object(&bytes, &exported, &expected_payload, &expected_index);

        // The recorder's rebuild from the delta is the same object.
        let rebuilt =
            derived_object(&base.index, &base.bytes, &payload, overlay_nodes(&payload).unwrap())
                .unwrap();
        assert_eq!(rebuilt.pieces.read_subtree(0, rebuilt.pieces.len).unwrap(), bytes);
        // The delta carries the appends and headers, not the base.
        let appended = encode_raster_value(&expected).unwrap().0.len()
            - encode_raster_value(&report(n_lines, n_rows)).unwrap().0.len();
        assert!(payload.tail.len() <= appended + 2 * (8 + LIST_HANDLE_HEADER_LEN as usize + 9));
        object
    }

    #[test]
    fn a_derived_object_is_its_base_plus_appends_encoded_whole() {
        // Grow the first list: every later field moves.
        derive_and_check(stored(&report(5, 3)), (5, 3), (4, 0));
        // Grow both lists, across odd and power-of-two widths.
        derive_and_check(stored(&report(8, 1)), (8, 1), (9, 2));
        derive_and_check(stored(&report(1, 0)), (1, 0), (1, 16));
        // From empty lists, and with nothing appended.
        derive_and_check(stored(&report(0, 0)), (0, 0), (3, 1));
        derive_and_check(stored(&report(6, 2)), (6, 2), (0, 0));
    }

    /// A derivation of a derivation: pieces point at the original buffers and
    /// the overlay stacks, and the object still equals its contiguous encoding.
    #[test]
    fn a_chain_of_derivations_is_the_whole_object() {
        let first = derive_and_check(stored(&report(3, 2)), (3, 2), (5, 1));
        let second = derive_and_check(as_base(&first), (8, 3), (2, 4));
        let third = derive_and_check(as_base(&second), (10, 7), (7, 0));
        // Flattened at derivation: no piece refers to an intermediate object.
        assert!(third.pieces.pieces.len() < 16, "{:?}", third.pieces.pieces.len());
    }

    /// Push-only: every set-once field is already written by the base.
    #[test]
    fn a_derived_draft_refuses_a_set() {
        let mut buffer = DraftBuffer::derive(report_schema(), stored(&report(2, 1))).unwrap();
        assert!(buffer
            .set("title", tree(&String::from("again")))
            .unwrap_err()
            .to_string()
            .contains("can only be written once"));
    }

    /// The witness frontier of a derived list is the frontier of the whole
    /// list — read from the base's levels, not rebuilt from its elements.
    #[test]
    fn a_derived_list_continues_its_base_frontier() {
        let base = stored(&report(13, 0));
        let mut buffer = DraftBuffer::derive(report_schema(), base).unwrap();
        let mut frontier = AppendFrontier::empty();
        for i in 0..13 {
            frontier.push(subtree_payload_and_root(&tree(&format!("line {i}"))).unwrap().1);
        }
        let witness_frontier = |buffer: &DraftBuffer| match buffer.fields.get("lines") {
            Some(BufferField::Append(list)) => list.frontier(),
            _ => unreachable!(),
        };
        assert_eq!(witness_frontier(&buffer), frontier);
        for i in 13..20 {
            let value = format!("line {i}");
            buffer.push("lines", tree(&value)).unwrap();
            frontier.push(subtree_payload_and_root(&tree(&value)).unwrap().1);
            assert_eq!(witness_frontier(&buffer), frontier);
        }
    }
}
