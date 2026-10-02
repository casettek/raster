use raster_core::input::{
    Hash32, ListProofDirection, ListProofSibling, SelectionProofStep, SelectorDescent, SelectorPath,
};
use raster_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::format;
use std::string::String;
use std::sync::Arc;
use std::vec::Vec;

const RINDEX_MAGIC: &[u8; 8] = b"rindex04";
const RINDEX_VERSION: u32 = 4;
/// Formats this reader refuses with a re-import message rather than a parse
/// error. `rindex03` stored absolute offsets (§`RasterNode::offset`).
const RINDEX_LEGACY_MAGICS: [&[u8; 8]; 2] = [b"rindex02", b"rindex03"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RasterIndex {
    pub version: u32,
    pub root_node: u64,
    pub root_commitment: Hash32,
    pub nodes: Vec<RasterNode>,
    /// A derived object's index is layered over its base's: ids below the
    /// base's node count are the base's nodes, shared by reference; `nodes`
    /// holds only what derivation added or rewrote
    /// (`incremental-draft-materialization` §How a derived object maps onto
    /// buffers). Never serialized — [`Self::encode`] flattens.
    #[serde(skip)]
    pub base: Option<Arc<RasterIndex>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RasterNode {
    /// Where this node's payload starts, **relative to its parent's** payload
    /// start (`rindex04`); the root's is relative to the data file. A node's
    /// position is therefore computed on descent — `pos(parent) + offset` —
    /// and appending to a list moves no recorded offset anywhere: not the
    /// list's own elements (they precede the growth), and not a later sibling
    /// field's children (they are relative to that sibling). That is what lets
    /// a derived object share its base's nodes, and lets a draft's buffers be
    /// sealed without an offset fixup. See
    /// `docs/proposals/incremental-draft-materialization.md` §Two remedies.
    pub offset: u64,
    pub len: u64,
    pub root_hash: Hash32,
    pub kind: RasterNodeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum RasterNodeKind {
    Unit,
    Leaf {
        type_name: String,
    },
    Struct {
        fields: Vec<RasterStructField>,
    },
    List {
        len: u64,
        elements: Vec<u64>,
        merkle_levels: Vec<RasterMerkleLevel>,
    },
    /// A list grown by derivation: the base's list node `base_list`, plus the
    /// appended elements and, per Merkle level, the nodes from the first one
    /// the base did not complete — level `h` is
    /// `base.levels[h][..base_len >> h] ++ level_tails[h]`. `O(k + log N)`
    /// instead of copying the base's `O(N)` element ids and levels. In-memory
    /// only: [`RasterIndex::encode`] flattens it into a `List`.
    ListContinuation {
        base_list: u64,
        len: u64,
        extra_elements: Vec<u64>,
        level_tails: Vec<RasterMerkleLevel>,
    },
    Map {
        entries: Vec<RasterMapEntry>,
    },
    EnumUnit {
        variant: String,
    },
    EnumNewtype {
        variant: String,
        child: u64,
    },
    EnumTuple {
        variant: String,
        elements: Vec<u64>,
    },
    EnumStruct {
        variant: String,
        fields: Vec<RasterStructField>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RasterStructField {
    pub name: String,
    pub child: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RasterMapEntry {
    pub key: u64,
    pub value: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RasterMerkleLevel {
    pub hashes: Vec<Hash32>,
}

/// A contiguous slice of a list node's elements, as located in the data file.
///
/// A range is the one selection that names no node: the storage tree holds a
/// `List<T>`, not a list of slices. What makes it addressable anyway is the
/// element layout — a list node's payload is `0x02 ‖ len ‖ (len8 ‖ child)*`
/// and element node offsets point *past* their length prefix
/// (`prepare_raster_children` in `input.rs`), so elements `[start, end)` occupy
/// one contiguous region. The payload is that region behind a synthesized
/// `0x02 ‖ k` header, which is the only part that exists nowhere in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RasterRangeSlice {
    pub start: u64,
    pub end: u64,
}

impl RasterRangeSlice {
    pub fn count(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RasterSelectionLocation {
    /// For a range, the *list* node the slice was taken from.
    pub node_id: u64,
    /// `node_id`'s data-file position — equal to `offset` except for a range,
    /// whose `offset` is the slice's.
    pub node_position: u64,
    pub offset: u64,
    pub len: u64,
    pub root_hash: Hash32,
    /// `Some` when the selection is a slice of `node_id`'s elements rather
    /// than the node itself.
    pub range: Option<RasterRangeSlice>,
}

struct Descent {
    node_id: u64,
    node_position: u64,
    offset: u64,
    len: u64,
    root_hash: Hash32,
    steps: Vec<SelectionProofStep>,
    range: Option<RasterRangeSlice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RasterSelection {
    pub node_id: u64,
    pub offset: u64,
    pub len: u64,
    pub root_hash: Hash32,
    pub steps: Vec<SelectionProofStep>,
    pub range: Option<RasterRangeSlice>,
}

impl RasterIndex {
    #[allow(dead_code)]
    pub(crate) fn new(root_node: u64, root_commitment: Hash32, nodes: Vec<RasterNode>) -> Self {
        Self {
            version: RINDEX_VERSION,
            root_node,
            root_commitment,
            nodes,
            base: None,
        }
    }

    /// An index layered over `base`: `nodes` takes ids from the base's node
    /// count onward.
    pub(crate) fn layered(
        base: Arc<RasterIndex>,
        root_node: u64,
        root_commitment: Hash32,
        nodes: Vec<RasterNode>,
    ) -> Self {
        Self {
            version: RINDEX_VERSION,
            root_node,
            root_commitment,
            nodes,
            base: Some(base),
        }
    }

    /// Node ids that belong to the base.
    pub(crate) fn base_len(&self) -> u64 {
        self.base.as_ref().map_or(0, |base| base.total_len())
    }

    /// Every node id this index answers, base included.
    pub(crate) fn total_len(&self) -> u64 {
        self.base_len() + self.nodes.len() as u64
    }

    /// The list at `id`, `None` for any other node kind.
    pub(crate) fn list_len(&self, id: u64) -> Result<Option<u64>> {
        Ok(match &self.node(id)?.kind {
            RasterNodeKind::List { len, .. } | RasterNodeKind::ListContinuation { len, .. } => {
                Some(*len)
            }
            _ => None,
        })
    }

    /// Element `i` of the list at `id`.
    pub(crate) fn list_element(&self, id: u64, i: u64) -> Result<u64> {
        let missing = || {
            Error::Serialization(format!("Malformed raster index: missing list element {}", i))
        };
        match &self.node(id)?.kind {
            RasterNodeKind::List { elements, .. } => {
                elements.get(i as usize).copied().ok_or_else(missing)
            }
            RasterNodeKind::ListContinuation {
                base_list,
                extra_elements,
                ..
            } => {
                let base_len = self.list_len(*base_list)?.ok_or_else(missing)?;
                if i < base_len {
                    self.list_element(*base_list, i)
                } else {
                    extra_elements
                        .get((i - base_len) as usize)
                        .copied()
                        .ok_or_else(missing)
                }
            }
            _ => Err(Error::Other("Raster node is not a list".into())),
        }
    }

    /// Element ids `[start, end)` of the list at `id`.
    pub(crate) fn list_elements(&self, id: u64, start: u64, end: u64) -> Result<Vec<u64>> {
        (start..end).map(|i| self.list_element(id, i)).collect()
    }

    /// How many Merkle levels the list at `id` has (leaves first, up to the
    /// single top node; none for an empty list).
    pub(crate) fn list_level_count(&self, id: u64) -> Result<usize> {
        match &self.node(id)?.kind {
            RasterNodeKind::List { merkle_levels, .. } => Ok(merkle_levels.len()),
            RasterNodeKind::ListContinuation { level_tails, .. } => Ok(level_tails.len()),
            _ => Err(Error::Other("Raster node is not a list".into())),
        }
    }

    /// The width of level `height`: `len` halved, rounding up, `height` times.
    pub(crate) fn list_level_width(&self, id: u64, height: usize) -> Result<usize> {
        let len = self
            .list_len(id)?
            .ok_or_else(|| Error::Other("Raster node is not a list".into()))?;
        let mut width = len;
        for _ in 0..height {
            width = width / 2 + width % 2;
        }
        Ok(width as usize)
    }

    /// Node `j` of Merkle level `height` of the list at `id`.
    pub(crate) fn list_level_node(&self, id: u64, height: usize, j: usize) -> Result<Hash32> {
        let missing = || {
            Error::Serialization(format!(
                "Malformed raster index: missing list Merkle node {} at level {}",
                j, height
            ))
        };
        match &self.node(id)?.kind {
            RasterNodeKind::List { merkle_levels, .. } => merkle_levels
                .get(height)
                .and_then(|level| level.hashes.get(j))
                .copied()
                .ok_or_else(missing),
            RasterNodeKind::ListContinuation {
                base_list,
                level_tails,
                ..
            } => {
                let base_len = self.list_len(*base_list)?.ok_or_else(missing)?;
                let complete = (base_len >> height) as usize;
                if j < complete {
                    self.list_level_node(*base_list, height, j)
                } else {
                    level_tails
                        .get(height)
                        .and_then(|level| level.hashes.get(j - complete))
                        .copied()
                        .ok_or_else(missing)
                }
            }
            _ => Err(Error::Other("Raster node is not a list".into())),
        }
    }

    /// The root of a list's element tree: its top Merkle node, `None` empty.
    pub(crate) fn list_elements_root(&self, id: u64) -> Result<Option<Hash32>> {
        let count = self.list_level_count(id)?;
        if count == 0 {
            return Ok(None);
        }
        Ok(Some(self.list_level_node(id, count - 1, 0)?))
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self> {
        for legacy in RINDEX_LEGACY_MAGICS {
            if bytes.len() >= legacy.len() && &bytes[..legacy.len()] == legacy {
                return Err(Error::Serialization(format!(
                    "Failed to parse raster index: {} is no longer supported; re-import as rindex04",
                    String::from_utf8_lossy(legacy)
                )));
            }
        }
        if bytes.len() < RINDEX_MAGIC.len() || &bytes[..RINDEX_MAGIC.len()] != RINDEX_MAGIC {
            return Err(Error::Serialization(
                "Failed to parse raster index: missing rindex04 header".into(),
            ));
        }

        let index: Self =
            raster_core::postcard::from_bytes(&bytes[RINDEX_MAGIC.len()..]).map_err(|e| {
                Error::Serialization(format!("Failed to decode raster index payload: {}", e))
            })?;
        index.validate()?;
        Ok(index)
    }

    #[allow(dead_code)]
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let flat;
        let index = if self.is_flat() {
            self
        } else {
            flat = self.flatten()?;
            &flat
        };
        let mut out = RINDEX_MAGIC.to_vec();
        out.extend(raster_core::postcard::to_allocvec(index).map_err(|e| {
            Error::Serialization(format!("Failed to encode raster index payload: {}", e))
        })?);
        Ok(out)
    }

    /// Validate this index as `encode` would, without encoding it.
    pub(crate) fn encode_check(&self) -> Result<()> {
        self.validate()
    }

    fn is_flat(&self) -> bool {
        self.base.is_none()
            && !self
                .nodes
                .iter()
                .any(|node| matches!(node.kind, RasterNodeKind::ListContinuation { .. }))
    }

    /// A standalone index answering exactly as this one: the base's nodes at
    /// their ids, then this layer's, with every continuation written out as
    /// the plain list it stands for. Ids and offsets are unchanged — offsets
    /// are parent-relative — so this is the export a derived object needs.
    pub(crate) fn flatten(&self) -> Result<RasterIndex> {
        let mut nodes = Vec::with_capacity(self.total_len() as usize);
        for id in 0..self.total_len() {
            let node = self.node(id)?;
            let kind = match &node.kind {
                RasterNodeKind::ListContinuation { len, .. } => RasterNodeKind::List {
                    len: *len,
                    elements: self.list_elements(id, 0, *len)?,
                    merkle_levels: (0..self.list_level_count(id)?)
                        .map(|height| {
                            Ok(RasterMerkleLevel {
                                hashes: (0..self.list_level_width(id, height)?)
                                    .map(|j| self.list_level_node(id, height, j))
                                    .collect::<Result<_>>()?,
                            })
                        })
                        .collect::<Result<_>>()?,
                },
                kind => kind.clone(),
            };
            nodes.push(RasterNode {
                offset: node.offset,
                len: node.len,
                root_hash: node.root_hash,
                kind,
            });
        }
        Ok(RasterIndex::new(self.root_node, self.root_commitment, nodes))
    }

    pub(crate) fn root_commitment_hex(&self) -> String {
        hex_string(&self.root_commitment)
    }

    pub(crate) fn root_location(&self) -> Result<RasterSelectionLocation> {
        let node = self.node(self.root_node)?;
        Ok(RasterSelectionLocation {
            node_id: self.root_node,
            node_position: node.offset,
            offset: node.offset,
            len: node.len,
            root_hash: self.root_commitment.clone(),
            range: None,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn root_selection(&self) -> Result<RasterSelection> {
        let location = self.root_location()?;
        Ok(RasterSelection {
            node_id: location.node_id,
            offset: location.offset,
            len: location.len,
            root_hash: location.root_hash,
            steps: Vec::new(),
            range: None,
        })
    }

    /// Byte region of elements `[start, end)` inside a list node's payload,
    /// and the slice descriptor that turns it into a `0x02` payload.
    ///
    /// Rejects an empty or reversed range and one that overruns the list: the
    /// index refuses it here rather than emitting a proof for `fold_list_range`
    /// to reject later.
    fn list_range_region(
        &self,
        list_position: u64,
        list: u64,
        len: u64,
        start: u64,
        end: u64,
    ) -> Result<(u64, u64, RasterRangeSlice)> {
        if start >= end || end > len {
            return Err(Error::Other(format!(
                "Selector range '{}..{}' is out of bounds for a list of length {}",
                start, end, len
            )));
        }

        let first = self.node(self.list_element(list, start)?)?;
        let last = self.node(self.list_element(list, end - 1)?)?;

        // Back up over the first element's 8-byte length prefix; element
        // offsets point past it.
        let region_start = (list_position + first.offset).checked_sub(8).ok_or_else(|| {
            Error::Serialization("Malformed raster index: list element precedes its length prefix".into())
        })?;
        let region_end = (list_position + last.offset).checked_add(last.len).ok_or_else(|| {
            Error::Serialization("Malformed raster index: list element region overflows".into())
        })?;
        let region_len = region_end.checked_sub(region_start).ok_or_else(|| {
            Error::Serialization("Malformed raster index: list elements are not in order".into())
        })?;

        Ok((region_start, region_len, RasterRangeSlice { start, end }))
    }

    pub(crate) fn locate(&self, selector: &SelectorPath) -> Result<RasterSelectionLocation> {
        let selection = self.descend(selector, false)?;
        Ok(RasterSelectionLocation {
            node_id: selection.node_id,
            node_position: selection.node_position,
            offset: selection.offset,
            len: selection.len,
            root_hash: selection.root_hash,
            range: selection.range,
        })
    }

    pub(crate) fn select(&self, selector: &SelectorPath) -> Result<RasterSelection> {
        let selection = self.descend(selector, true)?;
        Ok(RasterSelection {
            node_id: selection.node_id,
            offset: selection.offset,
            len: selection.len,
            root_hash: selection.root_hash,
            steps: selection.steps,
            range: selection.range,
        })
    }

    /// Walk `selector` from the root, computing each node's position on the
    /// way down (offsets are parent-relative) and, when `prove`, the proof
    /// steps. Lists are read through the list accessors, so a plain list and
    /// a derived continuation answer identically.
    fn descend(&self, selector: &SelectorPath, prove: bool) -> Result<Descent> {
        let mut current_id = self.root_node;
        let mut current_position = self.node(self.root_node)?.offset;
        let mut steps = Vec::with_capacity(if prove { selector.segments.len() } else { 0 });
        let last_segment = selector.segments.len().saturating_sub(1);

        for (position, segment) in selector.segments.iter().enumerate() {
            let list_len = self.list_len(current_id)?;
            match (segment.descent(), list_len) {
                (SelectorDescent::Range { start, end }, Some(len)) => {
                    if position != last_segment {
                        return Err(Error::Other(
                            "Range selector segment must be the final segment".into(),
                        ));
                    }
                    let (offset, region_len, range) =
                        self.list_range_region(current_position, current_id, len, start, end)?;
                    if prove {
                        steps.push(SelectionProofStep::ListRange {
                            start,
                            len,
                            siblings: list_range_proof_siblings(
                                self,
                                current_id,
                                start as usize,
                                end as usize,
                            )?,
                        });
                    }
                    return Ok(Descent {
                        node_id: current_id,
                        node_position: current_position,
                        offset,
                        len: region_len,
                        root_hash: self.root_commitment,
                        steps,
                        range: Some(range),
                    });
                }
                (SelectorDescent::Index(index), Some(len)) => {
                    if index >= len {
                        return Err(Error::Other(format!(
                            "Selector index '{}' was not found in raster index",
                            index
                        )));
                    }
                    if prove {
                        steps.push(SelectionProofStep::List {
                            index,
                            len,
                            siblings: list_proof_siblings(self, current_id, index as usize)?,
                        });
                    }
                    current_id = self.list_element(current_id, index)?;
                }
                (SelectorDescent::Field(field_name), None) => {
                    let RasterNodeKind::Struct { fields } = &self.node(current_id)?.kind else {
                        return Err(Error::Other(format!(
                            "Selector field '{}' was not found in selected value",
                            field_name
                        )));
                    };
                    let target_index = fields
                        .iter()
                        .position(|field| field.name == field_name)
                        .ok_or_else(|| {
                            Error::Other(format!(
                                "Selector field '{}' was not found in raster index",
                                field_name
                            ))
                        })?;
                    if prove {
                        let mut siblings = Vec::with_capacity(fields.len().saturating_sub(1));
                        for (idx, field) in fields.iter().enumerate() {
                            if idx != target_index {
                                siblings.push(self.node(field.child)?.root_hash);
                            }
                        }
                        steps.push(SelectionProofStep::Struct {
                            field_index: target_index as u64,
                            field_names: fields.iter().map(|field| field.name.clone()).collect(),
                            siblings,
                        });
                    }
                    current_id = fields[target_index].child;
                }
                (SelectorDescent::Field(field_name), Some(_)) => {
                    return Err(Error::Other(format!(
                        "Selector field '{}' was not found in selected value",
                        field_name
                    )));
                }
                (SelectorDescent::Index(index), None) => {
                    return Err(Error::Other(format!(
                        "Selector index '{}' was not found in selected value",
                        index
                    )));
                }
                (SelectorDescent::Range { start, end }, None) => {
                    return Err(Error::Other(format!(
                        "Selector range '{}..{}' requires a list value",
                        start, end
                    )));
                }
            }
            current_position = self.child_position(current_position, current_id)?;
        }

        let node = self.node(current_id)?;
        Ok(Descent {
            node_id: current_id,
            node_position: current_position,
            offset: current_position,
            len: node.len,
            root_hash: self.root_commitment,
            steps,
            range: None,
        })
    }

    /// A list node's authenticated length and element root, without touching
    /// an element or the data file.
    ///
    /// Both come straight out of the index: `len` from the node, the elements
    /// root from `merkle_levels.last()`, which [`RasterIndex::validate`]
    /// already guarantees holds exactly one hash for a non-empty list and is
    /// empty for an empty one. That is what makes recur-source tracing O(1) —
    /// see `docs/proposals/lazy-list-recur.md` §1.
    ///
    /// The values are only *index-trusted* here. They become authenticated
    /// when encoded as a `0x0A` payload and folded: the root is recomputed
    /// from the pair, so a forged length cannot reach the committed root.
    pub(crate) fn list_metadata(&self, selector: &SelectorPath) -> Result<(u64, Option<Hash32>)> {
        let location = self.locate(selector)?;
        if location.range.is_some() {
            return Err(Error::Other(
                "List metadata is a view of a whole list, not of a range selection".into(),
            ));
        }
        let len = self.list_len(location.node_id)?.ok_or_else(|| {
            Error::Other("List metadata requires a list value at the selected path".into())
        })?;
        Ok((len, self.list_elements_root(location.node_id)?))
    }

    pub(crate) fn get_node(&self, id: u64) -> Result<&RasterNode> {
        self.node(id)
    }

    /// The data-file position of `child`, a child of a node at
    /// `parent_position` — the one step of the descent every reader repeats.
    pub(crate) fn child_position(&self, parent_position: u64, child: u64) -> Result<u64> {
        parent_position
            .checked_add(self.node(child)?.offset)
            .ok_or_else(|| Error::Serialization("Malformed raster index: offset overflows".into()))
    }

    /// The data-file position of the root node.
    pub(crate) fn root_position(&self) -> Result<u64> {
        Ok(self.node(self.root_node)?.offset)
    }

    fn validate(&self) -> Result<()> {
        if self.version != RINDEX_VERSION {
            return Err(Error::Serialization(format!(
                "Unsupported raster index version {}",
                self.version
            )));
        }

        let root = self.node(self.root_node)?;
        if root.root_hash != self.root_commitment {
            return Err(Error::Serialization(
                "Raster index root commitment does not match root node hash".into(),
            ));
        }

        for node in &self.nodes {
            match &node.kind {
                RasterNodeKind::Unit
                | RasterNodeKind::Leaf { .. }
                | RasterNodeKind::EnumUnit { .. } => {}
                RasterNodeKind::Struct { fields } => {
                    for field in fields {
                        let _ = self.node(field.child)?;
                    }
                }
                RasterNodeKind::List {
                    len,
                    elements,
                    merkle_levels,
                } => {
                    if *len as usize != elements.len() {
                        return Err(Error::Serialization(format!(
                            "Raster list node declares len {} but has {} elements",
                            len,
                            elements.len()
                        )));
                    }
                    for child in elements {
                        let _ = self.node(*child)?;
                    }
                    if *len == 0 {
                        if !merkle_levels.is_empty() {
                            return Err(Error::Serialization(
                                "Empty raster list node must not store Merkle levels".into(),
                            ));
                        }
                    } else {
                        let first_width = merkle_levels.first().map(|level| level.hashes.len());
                        if first_width != Some(elements.len()) {
                            return Err(Error::Serialization(
                                "Raster list node first Merkle level must match element count"
                                    .into(),
                            ));
                        }
                        if merkle_levels.last().map(|level| level.hashes.len()) != Some(1) {
                            return Err(Error::Serialization(
                                "Raster list node last Merkle level must contain one hash".into(),
                            ));
                        }
                    }
                }
                RasterNodeKind::ListContinuation {
                    base_list,
                    len,
                    extra_elements,
                    level_tails,
                } => {
                    let base_len = self.list_len(*base_list)?.ok_or_else(|| {
                        Error::Serialization(
                            "Raster list continuation does not continue a list".into(),
                        )
                    })?;
                    if base_len + extra_elements.len() as u64 != *len {
                        return Err(Error::Serialization(format!(
                            "Raster list continuation declares len {} but has {} + {} elements",
                            len,
                            base_len,
                            extra_elements.len()
                        )));
                    }
                    for child in extra_elements {
                        let _ = self.node(*child)?;
                    }
                    // Level `h` holds `width(h)` nodes, the first `base_len >> h`
                    // of them complete in the base; the tail holds the rest.
                    let mut width = *len;
                    let mut expected_levels = Vec::new();
                    while width > 0 {
                        let complete = (base_len >> expected_levels.len()).min(width);
                        expected_levels.push(width - complete);
                        if width == 1 {
                            break;
                        }
                        width = width / 2 + width % 2;
                    }
                    let actual: Vec<u64> =
                        level_tails.iter().map(|level| level.hashes.len() as u64).collect();
                    if actual != expected_levels {
                        return Err(Error::Serialization(format!(
                            "Raster list continuation level tails {:?} do not fit a list of {} over {}",
                            actual, len, base_len
                        )));
                    }
                }
                RasterNodeKind::Map { entries } => {
                    for entry in entries {
                        let _ = self.node(entry.key)?;
                        let _ = self.node(entry.value)?;
                    }
                }
                RasterNodeKind::EnumNewtype { child, .. } => {
                    let _ = self.node(*child)?;
                }
                RasterNodeKind::EnumTuple { elements, .. } => {
                    for child in elements {
                        let _ = self.node(*child)?;
                    }
                }
                RasterNodeKind::EnumStruct { fields, .. } => {
                    for field in fields {
                        let _ = self.node(field.child)?;
                    }
                }
            }
        }

        Ok(())
    }

    fn node(&self, id: u64) -> Result<&RasterNode> {
        let base_len = self.base_len();
        if id < base_len {
            return self
                .base
                .as_ref()
                .expect("a nonzero base length has a base")
                .node(id);
        }
        self.nodes.get((id - base_len) as usize).ok_or_else(|| {
            Error::Serialization(format!("Malformed raster index: missing node {}", id))
        })
    }
}

/// Boundary siblings for a slice `[start, end)`, consumed by `fold_list_range`
/// level by level (left boundary before right).
///
/// The same walk as `list_root_and_range_proof` in `input.rs`, reading the
/// index's **stored** levels instead of recomputing them — `merkle_levels` is
/// exactly that function's `level` sequence before padding, so the two agree
/// step for step. Only the two boundaries need a witness: everything strictly
/// inside the slice is derived from the payload's own element roots, and an
/// odd-width level's final node pairs with a duplicate of itself, which the
/// verifier reconstructs without help.
fn list_range_proof_siblings(
    index: &RasterIndex,
    list: u64,
    start: usize,
    end: usize,
) -> Result<Vec<ListProofSibling>> {
    let mut siblings = Vec::new();
    let mut lo = start;
    let mut hi = end;

    for height in 0..index.list_level_count(list)? {
        let width = index.list_level_width(list, height)?;
        if width <= 1 {
            break;
        }

        if lo % 2 == 1 {
            siblings.push(ListProofSibling {
                direction: ListProofDirection::Left,
                hash: index.list_level_node(list, height, lo - 1)?,
            });
            lo -= 1;
        }
        if hi % 2 == 1 {
            if hi < width {
                siblings.push(ListProofSibling {
                    direction: ListProofDirection::Right,
                    hash: index.list_level_node(list, height, hi)?,
                });
            }
            // `hi == width`: odd-width level, the last node pairs with a
            // duplicate of itself and the verifier derives it.
            hi += 1;
        }

        lo /= 2;
        hi /= 2;
    }

    Ok(siblings)
}

fn list_proof_siblings(index: &RasterIndex, list: u64, element: usize) -> Result<Vec<ListProofSibling>> {
    let mut siblings = Vec::new();
    let mut idx = element;
    for height in 0..index.list_level_count(list)? {
        let width = index.list_level_width(list, height)?;
        if width <= 1 {
            break;
        }

        let sibling_index = if idx % 2 == 0 { idx + 1 } else { idx - 1 };
        // The last node of an odd-width level pairs with itself.
        let sibling_hash = index.list_level_node(list, height, sibling_index.min(width - 1))?;

        siblings.push(ListProofSibling {
            direction: if idx % 2 == 0 {
                ListProofDirection::Right
            } else {
                ListProofDirection::Left
            },
            hash: sibling_hash,
        });
        idx /= 2;
    }

    Ok(siblings)
}

fn hex_string(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}
