//! External input marker and resolved value types.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cfs::CfsCoordinates;

#[cfg(feature = "std")]
use std::collections::BTreeMap;

pub type Hash32 = [u8; 32];

/// A lightweight reference to a named external input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ExternalRef {
    pub name: String,
    #[serde(default)]
    pub selector: SelectorPath,
}

impl ExternalRef {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            selector: SelectorPath::default(),
        }
    }

    pub fn with_selector(name: impl Into<String>, selector: SelectorPath) -> Self {
        Self {
            name: name.into(),
            selector,
        }
    }
}

/// A lightweight reference to an immutable internal store object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct InternalRef {
    pub coordinates: CfsCoordinates,
    pub commitment: Vec<u8>,
}

impl InternalRef {
    pub fn new(coordinates: CfsCoordinates, commitment: Vec<u8>) -> Self {
        Self {
            coordinates,
            commitment,
        }
    }
}

/// A structured path describing a selected sub-value inside an external input.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SelectorPath {
    pub segments: Vec<SelectorSegment>,
}

impl SelectorPath {
    pub fn new(segments: Vec<SelectorSegment>) -> Self {
        Self { segments }
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SelectorSegment {
    Field(String),
    Index(u64),
    /// Contiguous list slice `[start, end)`. Only valid as the final segment
    /// of a selector path, and only against a list node.
    Range { start: u64, end: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SchemaNode {
    Leaf {
        type_name: String,
    },
    Struct {
        type_name: String,
        fields: Vec<SchemaField>,
    },
    List {
        type_name: String,
        element: Box<SchemaNode>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SchemaField {
    pub name: String,
    pub label: String,
    pub mode: SchemaFieldMode,
    pub schema: Box<SchemaNode>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SchemaFieldMode {
    SetOnce,
    AppendOnlyVec,
}

impl SchemaField {
    pub fn new(name: impl Into<String>, label: impl Into<String>, schema: SchemaNode) -> Self {
        let mode = match &schema {
            SchemaNode::List { .. } => SchemaFieldMode::AppendOnlyVec,
            _ => SchemaFieldMode::SetOnce,
        };
        Self::with_mode(name, label, mode, schema)
    }

    pub fn with_mode(
        name: impl Into<String>,
        label: impl Into<String>,
        mode: SchemaFieldMode,
        schema: SchemaNode,
    ) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            mode,
            schema: Box::new(schema),
        }
    }
}

pub trait Selectable {
    fn schema() -> SchemaNode;
}

pub trait Schema: Selectable {
    fn schema_hash() -> [u8; 32] {
        let schema = postcard::to_allocvec(&Self::schema()).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(schema);
        hasher.finalize().into()
    }
}

impl<T> Schema for T where T: Selectable {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Op {
    Set { field: String, value_bytes: Vec<u8> },
    Push { field: String, value_bytes: Vec<u8> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ListProofDirection {
    Left,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ListProofSibling {
    pub direction: ListProofDirection,
    pub hash: Hash32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SelectionProofStep {
    Struct {
        field_index: u64,
        field_count: u64,
        siblings: Vec<Hash32>,
    },
    List {
        index: u64,
        len: u64,
        siblings: Vec<ListProofSibling>,
    },
    /// Proof that the selected payload is the contiguous slice `[start, start + k)`
    /// of a list of `len` elements, where `k` is the payload's own element count.
    /// `siblings` are boundary hashes consumed level by level (left boundary
    /// before right boundary). Only valid as the final (deepest) proof step.
    ListRange {
        start: u64,
        len: u64,
        siblings: Vec<ListProofSibling>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SelectionProof {
    pub path: SelectorPath,
    pub root_hash: Hash32,
    pub steps: Vec<SelectionProofStep>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SelectionCommitment {
    pub path: SelectorPath,
    pub source_root_hash: Hash32,
    pub selected_hash: Hash32,
    pub selected_len: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SelectionWitness {
    pub bytes: Vec<u8>,
    pub proof: SelectionProof,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SelectedPayload {
    pub bytes: Vec<u8>,
    pub commitment: SelectionCommitment,
}

impl SelectedPayload {
    pub fn new(bytes: Vec<u8>, commitment: SelectionCommitment) -> Self {
        Self { bytes, commitment }
    }
}

fn selection_hash(parts: &[&[u8]]) -> Hash32 {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

fn parse_u64(bytes: &[u8], offset: &mut usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    let slice = bytes.get(*offset..end)?;
    let value = u64::from_le_bytes(slice.try_into().ok()?);
    *offset = end;
    Some(value)
}

fn parse_utf8(bytes: &[u8], offset: &mut usize) -> Option<Vec<u8>> {
    let len = parse_u64(bytes, offset)? as usize;
    let end = offset.checked_add(len)?;
    let slice = bytes.get(*offset..end)?;
    *offset = end;
    Some(slice.to_vec())
}

fn list_root_from_hashes(hashes: &[Hash32], len: u64) -> Hash32 {
    if hashes.is_empty() {
        return selection_hash(&[b"list-root", &len.to_le_bytes(), b"empty"]);
    }

    let mut level = hashes.to_vec();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            let last = level.last().cloned().unwrap();
            level.push(last);
        }

        let mut next = Vec::with_capacity(level.len() / 2);
        for pair in level.chunks(2) {
            next.push(selection_hash(&[
                b"list-node",
                pair[0].as_slice(),
                pair[1].as_slice(),
            ]));
        }
        level = next;
    }

    selection_hash(&[b"list-root", &len.to_le_bytes(), level[0].as_slice()])
}

fn parse_subtree_root(bytes: &[u8], offset: &mut usize) -> Option<Hash32> {
    let kind = *bytes.get(*offset)?;
    *offset += 1;

    match kind {
        0x00 => {
            let len = parse_u64(bytes, offset)? as usize;
            let end = offset.checked_add(len)?;
            let leaf_bytes = bytes.get(*offset..end)?;
            *offset = end;
            Some(selection_hash(&[b"leaf", leaf_bytes]))
        }
        0x01 => {
            let field_count = parse_u64(bytes, offset)? as usize;
            let mut child_roots = Vec::with_capacity(field_count);
            for _ in 0..field_count {
                let child_len = parse_u64(bytes, offset)? as usize;
                let end = offset.checked_add(child_len)?;
                let child_bytes = bytes.get(*offset..end)?;
                let mut child_offset = 0;
                let child_root = parse_subtree_root(child_bytes, &mut child_offset)?;
                if child_offset != child_bytes.len() {
                    return None;
                }
                *offset = end;
                child_roots.push(child_root);
            }

            let mut parts: Vec<&[u8]> = Vec::with_capacity(child_roots.len() + 1);
            parts.push(b"struct");
            for root in &child_roots {
                parts.push(root.as_slice());
            }
            Some(selection_hash(&parts))
        }
        0x02 => {
            let len = parse_u64(bytes, offset)?;
            let element_count = len as usize;
            let mut child_roots = Vec::with_capacity(element_count);
            for _ in 0..element_count {
                let child_len = parse_u64(bytes, offset)? as usize;
                let end = offset.checked_add(child_len)?;
                let child_bytes = bytes.get(*offset..end)?;
                let mut child_offset = 0;
                let child_root = parse_subtree_root(child_bytes, &mut child_offset)?;
                if child_offset != child_bytes.len() {
                    return None;
                }
                *offset = end;
                child_roots.push(child_root);
            }

            Some(list_root_from_hashes(&child_roots, len))
        }
        0x03 => Some(selection_hash(&[b"unit"])),
        0x04 => {
            let len = parse_u64(bytes, offset)?;
            let entry_count = len as usize;
            let len_bytes = len.to_le_bytes();
            let mut entry_roots = Vec::with_capacity(entry_count * 2);
            for _ in 0..entry_count {
                let key_len = parse_u64(bytes, offset)? as usize;
                let key_end = offset.checked_add(key_len)?;
                let key_bytes = bytes.get(*offset..key_end)?;
                let mut key_offset = 0;
                let key_root = parse_subtree_root(key_bytes, &mut key_offset)?;
                if key_offset != key_bytes.len() {
                    return None;
                }
                *offset = key_end;

                let value_len = parse_u64(bytes, offset)? as usize;
                let value_end = offset.checked_add(value_len)?;
                let value_bytes = bytes.get(*offset..value_end)?;
                let mut value_offset = 0;
                let value_root = parse_subtree_root(value_bytes, &mut value_offset)?;
                if value_offset != value_bytes.len() {
                    return None;
                }
                *offset = value_end;

                entry_roots.push(key_root);
                entry_roots.push(value_root);
            }

            let mut parts: Vec<&[u8]> = Vec::with_capacity(entry_roots.len() + 2);
            parts.push(b"map");
            parts.push(&len_bytes);
            for root in &entry_roots {
                parts.push(root.as_slice());
            }
            Some(selection_hash(&parts))
        }
        0x05 => {
            let variant = parse_utf8(bytes, offset)?;
            Some(selection_hash(&[b"enum-unit", variant.as_slice()]))
        }
        0x06 => {
            let variant = parse_utf8(bytes, offset)?;
            let child_len = parse_u64(bytes, offset)? as usize;
            let end = offset.checked_add(child_len)?;
            let child_bytes = bytes.get(*offset..end)?;
            let mut child_offset = 0;
            let child_root = parse_subtree_root(child_bytes, &mut child_offset)?;
            if child_offset != child_bytes.len() {
                return None;
            }
            *offset = end;
            Some(selection_hash(&[
                b"enum-newtype",
                variant.as_slice(),
                child_root.as_slice(),
            ]))
        }
        0x07 => {
            let variant = parse_utf8(bytes, offset)?;
            let len = parse_u64(bytes, offset)? as usize;
            let mut child_roots = Vec::with_capacity(len);
            for _ in 0..len {
                let child_len = parse_u64(bytes, offset)? as usize;
                let end = offset.checked_add(child_len)?;
                let child_bytes = bytes.get(*offset..end)?;
                let mut child_offset = 0;
                let child_root = parse_subtree_root(child_bytes, &mut child_offset)?;
                if child_offset != child_bytes.len() {
                    return None;
                }
                *offset = end;
                child_roots.push(child_root);
            }

            let mut parts: Vec<&[u8]> = Vec::with_capacity(child_roots.len() + 2);
            parts.push(b"enum-tuple");
            parts.push(variant.as_slice());
            for root in &child_roots {
                parts.push(root.as_slice());
            }
            Some(selection_hash(&parts))
        }
        0x08 => {
            let variant = parse_utf8(bytes, offset)?;
            let len = parse_u64(bytes, offset)? as usize;
            let mut child_roots = Vec::with_capacity(len);
            for _ in 0..len {
                let child_len = parse_u64(bytes, offset)? as usize;
                let end = offset.checked_add(child_len)?;
                let child_bytes = bytes.get(*offset..end)?;
                let mut child_offset = 0;
                let child_root = parse_subtree_root(child_bytes, &mut child_offset)?;
                if child_offset != child_bytes.len() {
                    return None;
                }
                *offset = end;
                child_roots.push(child_root);
            }

            let mut parts: Vec<&[u8]> = Vec::with_capacity(child_roots.len() + 2);
            parts.push(b"enum-struct");
            parts.push(variant.as_slice());
            for root in &child_roots {
                parts.push(root.as_slice());
            }
            Some(selection_hash(&parts))
        }
        _ => None,
    }
}

/// Parse a list-encoded payload (kind 0x02) into its element subtree roots.
/// Returns `None` when the payload is not a list or its length field does not
/// match the number of encoded children.
fn parse_list_child_roots(bytes: &[u8]) -> Option<Vec<Hash32>> {
    let mut offset = 0;
    if *bytes.first()? != 0x02 {
        return None;
    }
    offset += 1;

    let len = parse_u64(bytes, &mut offset)? as usize;
    let mut child_roots = Vec::with_capacity(len);
    for _ in 0..len {
        let child_len = parse_u64(bytes, &mut offset)? as usize;
        let end = offset.checked_add(child_len)?;
        let child_bytes = bytes.get(offset..end)?;
        let mut child_offset = 0;
        let child_root = parse_subtree_root(child_bytes, &mut child_offset)?;
        if child_offset != child_bytes.len() {
            return None;
        }
        offset = end;
        child_roots.push(child_root);
    }
    if offset != bytes.len() {
        return None;
    }
    Some(child_roots)
}

/// Fold the element roots of slice `[start, start + roots.len())` up to the
/// root of a list of `len` elements, consuming boundary `siblings` level by
/// level. Mirrors `list_root_from_hashes` exactly, including the duplication
/// of the last node at odd-width levels.
fn fold_list_range(
    roots: &[Hash32],
    start: u64,
    len: u64,
    siblings: &[ListProofSibling],
) -> Option<Hash32> {
    let count = roots.len() as u64;
    if count == 0 || start.checked_add(count)? > len {
        return None;
    }

    let mut nodes: Vec<Hash32> = roots.to_vec();
    let mut lo = start as usize;
    let mut hi = (start + count) as usize;
    let mut width = len as usize;
    let mut sibling_iter = siblings.iter();

    while width > 1 {
        if lo % 2 == 1 {
            let sibling = sibling_iter.next()?;
            if sibling.direction != ListProofDirection::Left {
                return None;
            }
            nodes.insert(0, sibling.hash);
            lo -= 1;
        }
        if hi % 2 == 1 {
            if hi == width {
                // Odd-width level: the last node pairs with a duplicate of itself.
                let last = *nodes.last()?;
                nodes.push(last);
            } else {
                let sibling = sibling_iter.next()?;
                if sibling.direction != ListProofDirection::Right {
                    return None;
                }
                nodes.push(sibling.hash);
            }
            hi += 1;
        }

        let mut next = Vec::with_capacity(nodes.len() / 2);
        for pair in nodes.chunks(2) {
            next.push(selection_hash(&[
                b"list-node",
                pair[0].as_slice(),
                pair[1].as_slice(),
            ]));
        }
        nodes = next;
        lo /= 2;
        hi /= 2;
        width = width / 2 + width % 2;
    }

    if sibling_iter.next().is_some() || nodes.len() != 1 {
        return None;
    }
    Some(selection_hash(&[
        b"list-root",
        &len.to_le_bytes(),
        nodes[0].as_slice(),
    ]))
}

pub fn verify_selection_proof(selected_bytes: &[u8], proof: &SelectionProof) -> bool {
    // A ListRange step may only appear as the final (deepest) proof step; it
    // derives the starting hash from the payload's element roots instead of
    // the payload's own subtree root.
    let mut steps = proof.steps.as_slice();
    let mut current_hash = if let Some(SelectionProofStep::ListRange {
        start,
        len,
        siblings,
    }) = steps.last()
    {
        steps = &steps[..steps.len() - 1];
        let Some(child_roots) = parse_list_child_roots(selected_bytes) else {
            return false;
        };
        let Some(hash) = fold_list_range(&child_roots, *start, *len, siblings) else {
            return false;
        };
        hash
    } else {
        let mut offset = 0;
        let Some(hash) = parse_subtree_root(selected_bytes, &mut offset) else {
            return false;
        };
        if offset != selected_bytes.len() {
            return false;
        }
        hash
    };

    for step in steps.iter().rev() {
        current_hash = match step {
            SelectionProofStep::Struct {
                field_index,
                field_count,
                siblings,
            } => {
                let field_index = *field_index as usize;
                let field_count = *field_count as usize;
                if field_index >= field_count || siblings.len() + 1 != field_count {
                    return false;
                }

                let mut child_roots = Vec::with_capacity(field_count);
                let mut sibling_iter = siblings.iter();
                for idx in 0..field_count {
                    if idx == field_index {
                        child_roots.push(current_hash.clone());
                    } else if let Some(sibling) = sibling_iter.next() {
                        child_roots.push(sibling.clone());
                    } else {
                        return false;
                    }
                }

                let mut parts: Vec<&[u8]> = Vec::with_capacity(child_roots.len() + 1);
                parts.push(b"struct");
                for root in &child_roots {
                    parts.push(root.as_slice());
                }
                selection_hash(&parts)
            }
            SelectionProofStep::List {
                index,
                len,
                siblings,
            } => {
                if *len == 0 || *index >= *len {
                    return false;
                }

                let mut hash = current_hash;
                for sibling in siblings {
                    hash = match sibling.direction {
                        ListProofDirection::Left => selection_hash(&[
                            b"list-node",
                            sibling.hash.as_slice(),
                            hash.as_slice(),
                        ]),
                        ListProofDirection::Right => selection_hash(&[
                            b"list-node",
                            hash.as_slice(),
                            sibling.hash.as_slice(),
                        ]),
                    };
                }
                selection_hash(&[b"list-root", &len.to_le_bytes(), hash.as_slice()])
            }
            // A range step is only valid as the final step, which was consumed
            // before this loop.
            SelectionProofStep::ListRange { .. } => return false,
        };
    }
    current_hash == proof.root_hash
}

pub fn selection_payload_hash(selected_bytes: &[u8]) -> Hash32 {
    Sha256::digest(selected_bytes).into()
}

pub fn verify_selection_witness(
    commitment: &SelectionCommitment,
    witness: &SelectionWitness,
) -> bool {
    if witness.proof.path != commitment.path
        || witness.proof.root_hash != commitment.source_root_hash
        || witness.bytes.len() as u64 != commitment.selected_len
        || selection_payload_hash(&witness.bytes) != commitment.selected_hash
    {
        return false;
    }

    verify_selection_proof(&witness.bytes, &witness.proof)
}

impl From<&str> for SelectorSegment {
    fn from(value: &str) -> Self {
        Self::Field(value.into())
    }
}

impl From<String> for SelectorSegment {
    fn from(value: String) -> Self {
        Self::Field(value)
    }
}

impl From<usize> for SelectorSegment {
    fn from(value: usize) -> Self {
        Self::Index(value as u64)
    }
}

impl From<u64> for SelectorSegment {
    fn from(value: u64) -> Self {
        Self::Index(value)
    }
}

impl From<u32> for SelectorSegment {
    fn from(value: u32) -> Self {
        Self::Index(value as u64)
    }
}

impl From<u16> for SelectorSegment {
    fn from(value: u16) -> Self {
        Self::Index(value as u64)
    }
}

impl From<u8> for SelectorSegment {
    fn from(value: u8) -> Self {
        Self::Index(value as u64)
    }
}

impl From<i32> for SelectorSegment {
    fn from(value: i32) -> Self {
        Self::Index(value as u64)
    }
}

/// A caller-owned external selection passed through `external!(...)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExternalSelection {
    pub reference: ExternalRef,
}

impl ExternalSelection {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            reference: ExternalRef::new(name),
        }
    }

    pub fn with_selector(name: impl Into<String>, selector: SelectorPath) -> Self {
        Self {
            reference: ExternalRef::with_selector(name, selector),
        }
    }

    pub fn name(&self) -> &str {
        &self.reference.name
    }

    pub fn selector(&self) -> &SelectorPath {
        &self.reference.selector
    }

    pub fn into_ref(self) -> ExternalRef {
        self.reference
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum AuthValue<T> {
    External(ExternalValue<T>),
    Internal(InternalValue<T>),
    Inline(T),
}

impl<T> AuthValue<T> {
    pub fn inline(value: T) -> Self {
        Self::Inline(value)
    }

    pub fn external(value: ExternalValue<T>) -> Self {
        Self::External(value)
    }

    pub fn internal(value: InternalValue<T>) -> Self {
        Self::Internal(value)
    }

    pub fn into_inner(self) -> T {
        match self {
            Self::External(external) => external.value,
            Self::Internal(internal) => internal.value,
            Self::Inline(value) => value,
        }
    }

    pub fn as_external(&self) -> Option<&ExternalValue<T>> {
        match self {
            Self::External(external) => Some(external),
            Self::Inline(_) => None,
            Self::Internal(_) => None,
        }
    }

    pub fn as_internal(&self) -> Option<&InternalValue<T>> {
        match self {
            Self::Internal(internal) => Some(internal),
            Self::External(_) | Self::Inline(_) => None,
        }
    }
}

/// A resolved external value carrying both identity metadata and the typed value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ExternalValue<T> {
    pub name: String,
    pub selector: SelectorPath,
    pub commitment: Option<String>,
    pub selected: SelectedPayload,
    pub value: T,
}

impl<T> ExternalValue<T> {
    pub fn new(
        name: impl Into<String>,
        selector: SelectorPath,
        commitment: Option<String>,
        selected: SelectedPayload,
        value: T,
    ) -> Self {
        Self {
            name: name.into(),
            selector,
            commitment,
            selected,
            value,
        }
    }

    pub fn into_inner(self) -> T {
        self.value
    }

    pub fn bytes(&self) -> &[u8] {
        &self.selected.bytes
    }

    pub fn selected(&self) -> &SelectedPayload {
        &self.selected
    }
}

/// A resolved internal value carrying both identity metadata and the typed value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct InternalValue<T> {
    pub reference: InternalRef,
    pub bytes: Vec<u8>,
    pub selector: SelectorPath,
    pub selection: SelectionCommitment,
    pub value: T,
}

impl<T> InternalValue<T> {
    pub fn new(reference: InternalRef, bytes: Vec<u8>, value: T) -> Self {
        Self::new_with_selection(
            reference,
            bytes,
            SelectorPath::default(),
            SelectionCommitment::default(),
            value,
        )
    }

    pub fn new_with_selection(
        reference: InternalRef,
        bytes: Vec<u8>,
        selector: SelectorPath,
        selection: SelectionCommitment,
        value: T,
    ) -> Self {
        Self {
            reference,
            bytes,
            selector,
            selection,
            value,
        }
    }

    pub fn into_inner(self) -> T {
        self.value
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

macro_rules! impl_leaf_schema {
    ($($ty:ty => $name:expr),+ $(,)?) => {
        $(
            impl Selectable for $ty {
                fn schema() -> SchemaNode {
                    SchemaNode::Leaf {
                        type_name: $name.into(),
                    }
                }
            }
        )+
    };
}

impl_leaf_schema!(
    bool => "bool",
    String => "String",
    usize => "usize",
    u64 => "u64",
    u32 => "u32",
    u16 => "u16",
    u8 => "u8",
    i64 => "i64",
    i32 => "i32",
    i16 => "i16",
    i8 => "i8"
);

impl<T> Selectable for Vec<T>
where
    T: Selectable,
{
    fn schema() -> SchemaNode {
        SchemaNode::List {
            type_name: "Vec".into(),
            element: Box::new(T::schema()),
        }
    }
}

/// A private file-backed external input declared inside `input.json`.
pub type ExternalInputPathEntry = String;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ExternalEncoding {
    #[default]
    Postcard,
    Raster,
}

/// How the runtime should back a resolved external file in memory.
#[cfg(feature = "std")]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ExternalLoadPreference {
    Read,
    Mmap,
}

/// A public external input commitment declared inside `input_manifest.json`.
pub type ExternalInputManifestEntry = String;

/// A private input document that binds external names to serialized files.
///
/// Each top-level field must be an external path entry encoded as
/// `{ "path": "...", "load_preference": "read|mmap" }` for postcard-backed
/// inputs or `{ "path": "...", "index_path": "...", "load_preference":
/// "read|mmap" }` for `raster`-encoded inputs.
#[cfg(feature = "std")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InputDocumentEntry {
    pub path: ExternalInputPathEntry,
    #[serde(default)]
    pub index_path: Option<ExternalInputPathEntry>,
    pub load_preference: ExternalLoadPreference,
}

#[cfg(feature = "std")]
impl InputDocumentEntry {
    pub fn path(&self) -> &str {
        self.path.as_str()
    }

    pub fn index_path(&self) -> Option<&str> {
        self.index_path.as_deref()
    }

    pub fn load_preference(&self) -> ExternalLoadPreference {
        self.load_preference
    }
}

#[cfg(feature = "std")]
pub type InputDocument = BTreeMap<String, InputDocumentEntry>;

/// A public JSON manifest document that describes the commitments for externals.
///
/// Each top-level field is a structured commitment entry encoded as:
/// `{ "type": "sha256", "encoding": "postcard|raster", "commitment": "..." }`
#[cfg(feature = "std")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputCommitmentType {
    Sha256,
}

#[cfg(feature = "std")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InputManifestEntry {
    #[serde(rename = "type")]
    pub commitment_type: InputCommitmentType,
    #[serde(default)]
    pub encoding: ExternalEncoding,
    pub commitment: ExternalInputManifestEntry,
}

#[cfg(feature = "std")]
impl InputManifestEntry {
    pub fn as_sha256_commitment(&self) -> Option<&str> {
        match self.commitment_type {
            InputCommitmentType::Sha256 => Some(self.commitment.as_str()),
        }
    }

    pub fn encoding(&self) -> ExternalEncoding {
        self.encoding
    }
}

#[cfg(feature = "std")]
pub type InputManifestDocument = BTreeMap<String, InputManifestEntry>;

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn parses_external_input_entries_with_per_file_load_preference() {
        let document: InputDocument = serde_json::from_str(
            r#"{
                "payload": {
                    "path": "payload.bin",
                    "load_preference": "mmap"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            document.get("payload").map(InputDocumentEntry::path),
            Some("payload.bin")
        );
        assert_eq!(
            document
                .get("payload")
                .map(InputDocumentEntry::load_preference),
            Some(ExternalLoadPreference::Mmap)
        );
        assert_eq!(
            document
                .get("payload")
                .and_then(InputDocumentEntry::index_path),
            None
        );
    }

    #[test]
    fn parses_raster_input_entries_with_index_path() {
        let document: InputDocument = serde_json::from_str(
            r#"{
                "payload": {
                    "path": "payload.rastered",
                    "index_path": "payload.rindex",
                    "load_preference": "read"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            document.get("payload").map(InputDocumentEntry::path),
            Some("payload.rastered")
        );
        assert_eq!(
            document
                .get("payload")
                .and_then(InputDocumentEntry::index_path),
            Some("payload.rindex")
        );
    }

    #[test]
    fn rejects_input_document_entries_without_load_preference() {
        let err = serde_json::from_str::<InputDocument>(
            r#"{
                "payload": { "path": "payload.bin" }
            }"#,
        )
        .expect_err("load preference is required");

        assert!(err.to_string().contains("missing field `load_preference`"));
    }

    #[test]
    fn rejects_input_document_entries_with_unknown_fields() {
        let err = serde_json::from_str::<InputDocument>(
            r#"{
                "payload": {
                    "path": "payload.bin",
                    "load_preference": "read",
                    "unexpected": true
                }
            }"#,
        )
        .expect_err("unknown input fields should be rejected");

        assert!(err.to_string().contains("unknown field `unexpected`"));
    }

    #[test]
    fn parses_structured_manifest_entries() {
        let document: InputManifestDocument = serde_json::from_str(
            r#"{
                "payload": {
                    "type": "sha256",
                    "commitment": "abc123"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            document
                .get("payload")
                .and_then(InputManifestEntry::as_sha256_commitment),
            Some("abc123")
        );
        assert_eq!(
            document.get("payload").map(InputManifestEntry::encoding),
            Some(ExternalEncoding::Postcard)
        );
    }

    #[test]
    fn parses_raster_manifest_entries() {
        let document: InputManifestDocument = serde_json::from_str(
            r#"{
                "payload": {
                    "type": "sha256",
                    "encoding": "raster",
                    "commitment": "abc123"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            document.get("payload").map(InputManifestEntry::encoding),
            Some(ExternalEncoding::Raster)
        );
    }

    #[test]
    fn auth_value_helpers_preserve_inline_values() {
        let arg = AuthValue::inline(7u64);

        assert!(arg.as_external().is_none());
        assert_eq!(arg.into_inner(), 7);
    }

    #[test]
    fn auth_value_helpers_preserve_external_metadata() {
        let selected = SelectedPayload {
            bytes: alloc::vec![1, 2, 3],
            commitment: SelectionCommitment {
                path: SelectorPath::default(),
                source_root_hash: [4; 32],
                selected_hash: [7; 32],
                selected_len: 3,
            },
        };
        let arg = AuthValue::external(ExternalValue::new(
            "payload",
            SelectorPath::default(),
            Some("abc123".to_string()),
            selected.clone(),
            9u64,
        ));

        let external = arg.as_external().expect("expected external metadata");
        assert_eq!(external.name, "payload");
        assert_eq!(external.commitment.as_deref(), Some("abc123"));
        assert_eq!(external.selected, selected);
        assert_eq!(arg.into_inner(), 9);
    }

    fn encode_leaf(bytes: &[u8]) -> Vec<u8> {
        let mut out = alloc::vec![0x00u8];
        out.extend((bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(bytes);
        out
    }

    fn encode_list(children: &[Vec<u8>]) -> Vec<u8> {
        let mut out = alloc::vec![0x02u8];
        out.extend((children.len() as u64).to_le_bytes());
        for child in children {
            out.extend((child.len() as u64).to_le_bytes());
            out.extend_from_slice(child);
        }
        out
    }

    fn leaf_root(bytes: &[u8]) -> Hash32 {
        selection_hash(&[b"leaf", bytes])
    }

    /// Reference sibling builder mirroring `fold_list_range` with full tree
    /// knowledge; the runtime prover follows the same consumption order.
    fn build_range_siblings(
        element_roots: &[Hash32],
        start: usize,
        end: usize,
    ) -> Vec<ListProofSibling> {
        let mut level: Vec<Hash32> = element_roots.to_vec();
        let mut lo = start;
        let mut hi = end;
        let mut siblings = Vec::new();

        while level.len() > 1 {
            let width = level.len();
            if lo % 2 == 1 {
                siblings.push(ListProofSibling {
                    direction: ListProofDirection::Left,
                    hash: level[lo - 1],
                });
                lo -= 1;
            }
            if hi % 2 == 1 {
                if hi < width {
                    siblings.push(ListProofSibling {
                        direction: ListProofDirection::Right,
                        hash: level[hi],
                    });
                }
                hi += 1;
            }

            let mut padded = level.clone();
            if padded.len() % 2 == 1 {
                padded.push(*padded.last().unwrap());
            }
            level = padded
                .chunks(2)
                .map(|pair| selection_hash(&[b"list-node", &pair[0], &pair[1]]))
                .collect();
            lo /= 2;
            hi /= 2;
        }
        siblings
    }

    fn range_fixture(len: usize, start: usize, end: usize) -> (Vec<u8>, SelectionProof) {
        let element_bytes: Vec<Vec<u8>> = (0..len)
            .map(|value| alloc::vec![value as u8, 0xAB])
            .collect();
        let element_roots: Vec<Hash32> =
            element_bytes.iter().map(|bytes| leaf_root(bytes)).collect();
        let encoded_children: Vec<Vec<u8>> = element_bytes[start..end]
            .iter()
            .map(|bytes| encode_leaf(bytes))
            .collect();
        let payload = encode_list(&encoded_children);
        let proof = SelectionProof {
            path: SelectorPath::new(alloc::vec![SelectorSegment::Range {
                start: start as u64,
                end: end as u64,
            }]),
            root_hash: list_root_from_hashes(&element_roots, len as u64),
            steps: alloc::vec![SelectionProofStep::ListRange {
                start: start as u64,
                len: len as u64,
                siblings: build_range_siblings(&element_roots, start, end),
            }],
        };
        (payload, proof)
    }

    #[test]
    fn range_proofs_roundtrip_for_all_ranges_and_odd_widths() {
        for len in 1..=12usize {
            for start in 0..len {
                for end in (start + 1)..=len {
                    let (payload, proof) = range_fixture(len, start, end);
                    assert!(
                        verify_selection_proof(&payload, &proof),
                        "range [{start}, {end}) of len {len} should verify",
                    );
                }
            }
        }
        // A larger case crossing several duplicated (odd-width) levels.
        let (payload, proof) = range_fixture(33, 30, 33);
        assert!(verify_selection_proof(&payload, &proof));
    }

    #[test]
    fn range_proof_rejects_shifted_start() {
        let (payload, mut proof) = range_fixture(9, 2, 5);
        let SelectionProofStep::ListRange { start, .. } = &mut proof.steps[0] else {
            unreachable!();
        };
        *start += 1;
        assert!(!verify_selection_proof(&payload, &proof));
    }

    #[test]
    fn range_proof_rejects_tampered_element() {
        let (mut payload, proof) = range_fixture(9, 2, 5);
        // Flip a byte inside the first element's leaf payload.
        let last = payload.len() - 1;
        payload[last] ^= 0x01;
        assert!(!verify_selection_proof(&payload, &proof));
    }

    #[test]
    fn range_proof_rejects_wrong_total_len() {
        let (payload, mut proof) = range_fixture(9, 2, 5);
        let SelectionProofStep::ListRange { len, .. } = &mut proof.steps[0] else {
            unreachable!();
        };
        *len += 1;
        assert!(!verify_selection_proof(&payload, &proof));
    }

    #[test]
    fn range_proof_rejects_non_terminal_range_step() {
        let (payload, mut proof) = range_fixture(9, 2, 5);
        // Root→leaf step order: appending a List step after ListRange makes
        // the range step non-terminal.
        proof.steps.push(SelectionProofStep::List {
            index: 0,
            len: 1,
            siblings: alloc::vec![],
        });
        assert!(!verify_selection_proof(&payload, &proof));
    }

    #[test]
    fn range_proof_composes_under_struct_step() {
        let (payload, mut proof) = range_fixture(9, 2, 5);
        let list_root = proof.root_hash;
        let sibling_root = leaf_root(b"other-field");
        let struct_root = selection_hash(&[b"struct", &list_root, &sibling_root]);
        proof.steps.insert(
            0,
            SelectionProofStep::Struct {
                field_index: 0,
                field_count: 2,
                siblings: alloc::vec![sibling_root],
            },
        );
        proof.root_hash = struct_root;
        assert!(verify_selection_proof(&payload, &proof));
    }
}
