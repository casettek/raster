use raster_core::input::{
    Hash32, ListProofDirection, ListProofSibling, SelectionProofStep, SelectorDescent, SelectorPath,
};
use raster_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::format;
use std::string::String;
use std::vec::Vec;

const RINDEX_MAGIC: &[u8; 8] = b"rindex02";
const RINDEX_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RasterIndex {
    pub version: u32,
    pub root_node: u64,
    pub root_commitment: Hash32,
    pub nodes: Vec<RasterNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RasterNode {
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
    pub offset: u64,
    pub len: u64,
    pub root_hash: Hash32,
    /// `Some` when the selection is a slice of `node_id`'s elements rather
    /// than the node itself.
    pub range: Option<RasterRangeSlice>,
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
        }
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < RINDEX_MAGIC.len() || &bytes[..RINDEX_MAGIC.len()] != RINDEX_MAGIC {
            return Err(Error::Serialization(
                "Failed to parse raster index: missing rindex02 header".into(),
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
        let mut out = RINDEX_MAGIC.to_vec();
        out.extend(raster_core::postcard::to_allocvec(self).map_err(|e| {
            Error::Serialization(format!("Failed to encode raster index payload: {}", e))
        })?);
        Ok(out)
    }

    pub(crate) fn root_commitment_hex(&self) -> String {
        hex_string(&self.root_commitment)
    }

    pub(crate) fn root_location(&self) -> Result<RasterSelectionLocation> {
        let node = self.node(self.root_node)?;
        Ok(RasterSelectionLocation {
            node_id: self.root_node,
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
        len: u64,
        elements: &[u64],
        start: u64,
        end: u64,
    ) -> Result<(u64, u64, RasterRangeSlice)> {
        if start >= end || end > len {
            return Err(Error::Other(format!(
                "Selector range '{}..{}' is out of bounds for a list of length {}",
                start, end, len
            )));
        }

        let first = self.node(*elements.get(start as usize).ok_or_else(|| {
            Error::Serialization(format!("Malformed raster index: missing list element {}", start))
        })?)?;
        let last = self.node(*elements.get((end - 1) as usize).ok_or_else(|| {
            Error::Serialization(format!(
                "Malformed raster index: missing list element {}",
                end - 1
            ))
        })?)?;

        // Back up over the first element's 8-byte length prefix; element
        // offsets point past it.
        let region_start = first.offset.checked_sub(8).ok_or_else(|| {
            Error::Serialization("Malformed raster index: list element precedes its length prefix".into())
        })?;
        let region_end = last.offset.checked_add(last.len).ok_or_else(|| {
            Error::Serialization("Malformed raster index: list element region overflows".into())
        })?;
        let region_len = region_end.checked_sub(region_start).ok_or_else(|| {
            Error::Serialization("Malformed raster index: list elements are not in order".into())
        })?;

        Ok((region_start, region_len, RasterRangeSlice { start, end }))
    }

    pub(crate) fn locate(&self, selector: &SelectorPath) -> Result<RasterSelectionLocation> {
        if selector.is_empty() {
            return self.root_location();
        }

        let mut current_id = self.root_node;
        let last_segment = selector.segments.len() - 1;

        for (position, segment) in selector.segments.iter().enumerate() {
            let node = self.node(current_id)?;
            if let (
                SelectorDescent::Range { start, end },
                RasterNodeKind::List { len, elements, .. },
            ) = (segment.descent(), &node.kind)
            {
                if position != last_segment {
                    return Err(Error::Other(
                        "Range selector segment must be the final segment".into(),
                    ));
                }
                let (offset, region_len, range) =
                    self.list_range_region(*len, elements, start, end)?;
                return Ok(RasterSelectionLocation {
                    node_id: current_id,
                    offset,
                    len: region_len,
                    root_hash: self.root_commitment.clone(),
                    range: Some(range),
                });
            }
            match (segment.descent(), &node.kind) {
                (SelectorDescent::Field(field_name), RasterNodeKind::Struct { fields }) => {
                    let target = fields
                        .iter()
                        .find(|field| field.name == field_name)
                        .ok_or_else(|| {
                            Error::Other(format!(
                                "Selector field '{}' was not found in raster index",
                                field_name
                            ))
                        })?;
                    current_id = target.child;
                }
                (SelectorDescent::Index(index), RasterNodeKind::List { len, elements, .. }) => {
                    if index >= *len {
                        return Err(Error::Other(format!(
                            "Selector index '{}' was not found in raster index",
                            index
                        )));
                    }
                    current_id = *elements.get(index as usize).ok_or_else(|| {
                        Error::Serialization(format!(
                            "Malformed raster index: missing list element {}",
                            index
                        ))
                    })?;
                }
                (SelectorDescent::Field(field_name), _) => {
                    return Err(Error::Other(format!(
                        "Selector field '{}' was not found in selected value",
                        field_name
                    )));
                }
                (SelectorDescent::Index(index), _) => {
                    return Err(Error::Other(format!(
                        "Selector index '{}' was not found in selected value",
                        index
                    )));
                }
                // A list range is handled before this match; reaching here
                // means the node it was applied to is not a list.
                (SelectorDescent::Range { start, end }, _) => {
                    return Err(Error::Other(format!(
                        "Selector range '{}..{}' requires a list value",
                        start, end
                    )));
                }
            }
        }

        let node = self.node(current_id)?;
        Ok(RasterSelectionLocation {
            node_id: current_id,
            offset: node.offset,
            len: node.len,
            root_hash: self.root_commitment.clone(),
            range: None,
        })
    }

    pub(crate) fn select(&self, selector: &SelectorPath) -> Result<RasterSelection> {
        if selector.is_empty() {
            return self.root_selection();
        }

        let mut current_id = self.root_node;
        let mut steps = Vec::with_capacity(selector.segments.len());
        let last_segment = selector.segments.len() - 1;

        for (position, segment) in selector.segments.iter().enumerate() {
            let node = self.node(current_id)?;
            if let (
                SelectorDescent::Range { start, end },
                RasterNodeKind::List {
                    len,
                    elements,
                    merkle_levels,
                },
            ) = (segment.descent(), &node.kind)
            {
                if position != last_segment {
                    return Err(Error::Other(
                        "Range selector segment must be the final segment".into(),
                    ));
                }
                let (offset, region_len, range) =
                    self.list_range_region(*len, elements, start, end)?;
                steps.push(SelectionProofStep::ListRange {
                    start,
                    len: *len,
                    siblings: list_range_proof_siblings(
                        merkle_levels,
                        start as usize,
                        end as usize,
                    )?,
                });
                return Ok(RasterSelection {
                    node_id: current_id,
                    offset,
                    len: region_len,
                    root_hash: self.root_commitment.clone(),
                    steps,
                    range: Some(range),
                });
            }
            match (segment.descent(), &node.kind) {
                (SelectorDescent::Field(field_name), RasterNodeKind::Struct { fields }) => {
                    let target_index = fields
                        .iter()
                        .position(|field| field.name == field_name)
                        .ok_or_else(|| {
                            Error::Other(format!(
                                "Selector field '{}' was not found in raster index",
                                field_name
                            ))
                        })?;
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
                    current_id = fields[target_index].child;
                }
                (
                    SelectorDescent::Index(index),
                    RasterNodeKind::List {
                        len,
                        elements,
                        merkle_levels,
                    },
                ) => {
                    if index >= *len {
                        return Err(Error::Other(format!(
                            "Selector index '{}' was not found in raster index",
                            index
                        )));
                    }
                    let idx = index as usize;
                    let child = *elements.get(idx).ok_or_else(|| {
                        Error::Serialization(format!(
                            "Malformed raster index: missing list element {}",
                            idx
                        ))
                    })?;
                    steps.push(SelectionProofStep::List {
                        index,
                        len: *len,
                        siblings: list_proof_siblings(merkle_levels, idx)?,
                    });
                    current_id = child;
                }
                (SelectorDescent::Field(field_name), _) => {
                    return Err(Error::Other(format!(
                        "Selector field '{}' was not found in selected value",
                        field_name
                    )));
                }
                (SelectorDescent::Index(index), _) => {
                    return Err(Error::Other(format!(
                        "Selector index '{}' was not found in selected value",
                        index
                    )));
                }
                // A list range is handled before this match; reaching here
                // means the node it was applied to is not a list.
                (SelectorDescent::Range { start, end }, _) => {
                    return Err(Error::Other(format!(
                        "Selector range '{}..{}' requires a list value",
                        start, end
                    )));
                }
            }
        }

        let node = self.node(current_id)?;
        Ok(RasterSelection {
            node_id: current_id,
            offset: node.offset,
            len: node.len,
            root_hash: self.root_commitment.clone(),
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
        let node = self.node(location.node_id)?;
        let RasterNodeKind::List {
            len, merkle_levels, ..
        } = &node.kind
        else {
            return Err(Error::Other(
                "List metadata requires a list value at the selected path".into(),
            ));
        };

        let elements_root = match merkle_levels.last() {
            None => None,
            Some(level) => Some(*level.hashes.first().ok_or_else(|| {
                Error::Serialization(
                    "Malformed raster index: list node top Merkle level is empty".into(),
                )
            })?),
        };
        Ok((*len, elements_root))
    }

    pub(crate) fn get_node(&self, id: u64) -> Result<&RasterNode> {
        self.node(id)
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
        self.nodes.get(id as usize).ok_or_else(|| {
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
    levels: &[RasterMerkleLevel],
    start: usize,
    end: usize,
) -> Result<Vec<ListProofSibling>> {
    let mut siblings = Vec::new();
    let mut lo = start;
    let mut hi = end;

    for level in levels {
        let width = level.hashes.len();
        if width <= 1 {
            break;
        }

        if lo % 2 == 1 {
            siblings.push(ListProofSibling {
                direction: ListProofDirection::Left,
                hash: *level.hashes.get(lo - 1).ok_or_else(|| {
                    Error::Serialization(
                        "Malformed raster index: missing left range boundary sibling".into(),
                    )
                })?,
            });
            lo -= 1;
        }
        if hi % 2 == 1 {
            if hi < width {
                siblings.push(ListProofSibling {
                    direction: ListProofDirection::Right,
                    hash: *level.hashes.get(hi).ok_or_else(|| {
                        Error::Serialization(
                            "Malformed raster index: missing right range boundary sibling".into(),
                        )
                    })?,
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

fn list_proof_siblings(
    levels: &[RasterMerkleLevel],
    index: usize,
) -> Result<Vec<ListProofSibling>> {
    if levels.is_empty() {
        return Ok(Vec::new());
    }

    let mut siblings = Vec::new();
    let mut idx = index;
    for level in levels {
        if level.hashes.len() <= 1 {
            break;
        }

        let sibling_index = if idx % 2 == 0 { idx + 1 } else { idx - 1 };
        let sibling_hash = level
            .hashes
            .get(sibling_index)
            .cloned()
            .or_else(|| level.hashes.last().cloned())
            .ok_or_else(|| {
                Error::Serialization("Malformed raster index: missing list Merkle sibling".into())
            })?;

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
