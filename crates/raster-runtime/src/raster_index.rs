use raster_core::input::{
    Hash32, ListProofDirection, ListProofSibling, SelectionProofStep, SelectorPath, SelectorSegment,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RasterSelectionLocation {
    pub node_id: u64,
    pub offset: u64,
    pub len: u64,
    pub root_hash: Hash32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RasterSelection {
    pub node_id: u64,
    pub offset: u64,
    pub len: u64,
    pub root_hash: Hash32,
    pub steps: Vec<SelectionProofStep>,
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
        })
    }

    pub(crate) fn locate(&self, selector: &SelectorPath) -> Result<RasterSelectionLocation> {
        if selector.is_empty() {
            return self.root_location();
        }

        let mut current_id = self.root_node;

        for segment in &selector.segments {
            let node = self.node(current_id)?;
            match (segment, &node.kind) {
                (SelectorSegment::Field(field_name), RasterNodeKind::Struct { fields }) => {
                    let target = fields
                        .iter()
                        .find(|field| field.name == *field_name)
                        .ok_or_else(|| {
                            Error::Other(format!(
                                "Selector field '{}' was not found in raster index",
                                field_name
                            ))
                        })?;
                    current_id = target.child;
                }
                (SelectorSegment::Index(index), RasterNodeKind::List { len, elements, .. }) => {
                    if *index >= *len {
                        return Err(Error::Other(format!(
                            "Selector index '{}' was not found in raster index",
                            index
                        )));
                    }
                    current_id = *elements.get(*index as usize).ok_or_else(|| {
                        Error::Serialization(format!(
                            "Malformed raster index: missing list element {}",
                            index
                        ))
                    })?;
                }
                (SelectorSegment::Field(field_name), _) => {
                    return Err(Error::Other(format!(
                        "Selector field '{}' was not found in selected value",
                        field_name
                    )));
                }
                (SelectorSegment::Index(index), _) => {
                    return Err(Error::Other(format!(
                        "Selector index '{}' was not found in selected value",
                        index
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
        })
    }

    pub(crate) fn select(&self, selector: &SelectorPath) -> Result<RasterSelection> {
        if selector.is_empty() {
            return self.root_selection();
        }

        let mut current_id = self.root_node;
        let mut steps = Vec::with_capacity(selector.segments.len());

        for segment in &selector.segments {
            let node = self.node(current_id)?;
            match (segment, &node.kind) {
                (SelectorSegment::Field(field_name), RasterNodeKind::Struct { fields }) => {
                    let target_index = fields
                        .iter()
                        .position(|field| field.name == *field_name)
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
                    SelectorSegment::Index(index),
                    RasterNodeKind::List {
                        len,
                        elements,
                        merkle_levels,
                    },
                ) => {
                    if *index >= *len {
                        return Err(Error::Other(format!(
                            "Selector index '{}' was not found in raster index",
                            index
                        )));
                    }
                    let idx = *index as usize;
                    let child = *elements.get(idx).ok_or_else(|| {
                        Error::Serialization(format!(
                            "Malformed raster index: missing list element {}",
                            idx
                        ))
                    })?;
                    steps.push(SelectionProofStep::List {
                        index: *index,
                        len: *len,
                        siblings: list_proof_siblings(merkle_levels, idx)?,
                    });
                    current_id = child;
                }
                (SelectorSegment::Field(field_name), _) => {
                    return Err(Error::Other(format!(
                        "Selector field '{}' was not found in selected value",
                        field_name
                    )));
                }
                (SelectorSegment::Index(index), _) => {
                    return Err(Error::Other(format!(
                        "Selector index '{}' was not found in selected value",
                        index
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
        })
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
