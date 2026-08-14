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

/// A lightweight reference to an immutable storage object.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct StorageRef {
    pub coordinates: CfsCoordinates,
    pub commitment: Vec<u8>,
}

impl StorageRef {
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

/// Committed width of an integer that supplies a [`SelectorSegment::BoundIndex`].
///
/// The width is not decoration: a leaf's bytes are fixed-width little-endian
/// (see `encode_leaf_bytes` in the runtime and `parse_subtree_root`'s `0x00`
/// arm), so `7u32` and `7u64` are *different* payloads with different hashes.
/// [`encode_index_leaf_payload`] needs the width to re-derive the exact bytes
/// the index binding committed to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum IndexWidth {
    U8,
    U16,
    U32,
    U64,
}

impl IndexWidth {
    /// Encode `index` as this width's leaf bytes, or `None` if it does not fit.
    ///
    /// The `None` case is load-bearing, not defensive. A truncating cast here
    /// would be a forgery: a prover could record `index: 300` with width `U8`,
    /// truncate to `44`, and match a binding that actually committed `44` —
    /// proving element 300 while the authorized value said 44.
    pub fn encode(self, index: u64) -> Option<Vec<u8>> {
        let bytes = match self {
            Self::U8 => Vec::from(&u8::try_from(index).ok()?.to_le_bytes()[..]),
            Self::U16 => Vec::from(&u16::try_from(index).ok()?.to_le_bytes()[..]),
            Self::U32 => Vec::from(&u32::try_from(index).ok()?.to_le_bytes()[..]),
            Self::U64 => Vec::from(&index.to_le_bytes()[..]),
        };
        Some(bytes)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SelectorSegment {
    Field(String),
    Index(u64),
    /// Contiguous list slice `[start, end)`. Only valid as the final segment
    /// of a selector path, and only against a list node.
    Range {
        start: u64,
        end: u64,
    },
    /// List index taken from an authorized value rather than a literal.
    ///
    /// `index` is the value used on this run — per-run data, exactly as it
    /// already is for a recur iteration's [`SelectorSegment::Index`]. `source`
    /// names the storage binding *on the same step* that supplied it, and
    /// `width` fixes how that binding's value is encoded.
    ///
    /// Naming a sibling binding is what makes the reference sound: the step's
    /// storage map is already proved entry-by-entry (append-log path plus
    /// coordinate-index membership plus selection witness), so a `BoundIndex`
    /// can only point at a value this execution already authorized. Carrying
    /// the index's own `StorageRef`/`SelectorPath` inline instead would put the
    /// verifier back in the business of establishing provenance from scratch,
    /// and would make `SelectorPath` recursive. See
    /// `docs/proposals/dynamic-index-selection.md` §1-§3.
    ///
    /// `source` is a [`crate::trace::StorageBindingName`]; it is spelled
    /// `String` here because `trace` depends on this module, not the reverse.
    BoundIndex {
        index: u64,
        source: String,
        width: IndexWidth,
    },
}

/// What a [`SelectorSegment`] does to the tree, with provenance projected away.
///
/// A `BoundIndex` descends a list exactly as an `Index` does — same element,
/// same proof step, same bytes. The *only* thing that differs is where the index
/// came from, and that is checked in one place
/// ([`crate::trace::verify_bound_index_bindings`]) rather than in every walker.
/// Tree walkers and proof builders match on this, so "a dynamic index selects
/// like a literal one" is enforced by the type rather than repeated by
/// convention — and a future segment kind cannot be silently mishandled by a
/// walker that forgot about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorDescent<'a> {
    Field(&'a str),
    Index(u64),
    Range { start: u64, end: u64 },
}

impl SelectorSegment {
    /// Project this segment onto the descent it performs.
    pub fn descent(&self) -> SelectorDescent<'_> {
        match self {
            Self::Field(name) => SelectorDescent::Field(name.as_str()),
            Self::Index(index) | Self::BoundIndex { index, .. } => SelectorDescent::Index(*index),
            Self::Range { start, end } => SelectorDescent::Range {
                start: *start,
                end: *end,
            },
        }
    }
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
        /// Which of `field_names` this step descends into. The selected
        /// child's root is the hash carried in from the step below; the
        /// other fields' roots are `siblings`, in ascending position order
        /// with `field_index` skipped.
        field_index: u64,
        /// Every field of the struct, in declaration order — names
        /// included, because they are part of the struct commitment (see
        /// [`struct_commitments_root`]) and are what binds this step to a
        /// selector path segment.
        field_names: Vec<String>,
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

/// Which *view* of the selected node a commitment's payload carries.
///
/// A selector names a descent, so it cannot distinguish these: selecting a
/// `List<T>` and selecting that list's metadata share a path, a source root and
/// a set of proof steps, and both fold to the same root. What separates them is
/// the payload — a `0x02` list against a `0x0A` metadata record — and the rule
/// (`lazy-list-recur.md` §1) is that **every consumer states the kind it
/// expects** rather than accepting whichever arrives.
///
/// It is recorded rather than derived from byte 0 because the payload is not
/// always at hand: `raster-cli`'s commit pipeline rebuilds a witness from
/// `(coordinates, commitment, selector)` alone
/// (`raster-cli/src/commands/run.rs`, `build_storage_selection_witnesses`) and
/// has to know which form to regenerate. A *reader* holding the bytes can still
/// cross-check byte 0, which is what [`verify_selection_witness`] does.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SelectionPayloadKind {
    /// The selected node's own payload, whatever its kind — the only form that
    /// existed before `lazy-list-recur`, and still the form for every selection
    /// that is not a recur source.
    #[default]
    Raw,
    /// The `0x0A` authenticated list metadata record: `(len, elements_root)`,
    /// 41 bytes, or 9 for an empty list. There is deliberately no separate
    /// "raw whole list" spelling here — naming the kind `List` is what encodes
    /// that a list is *always* carried as metadata once it is named at all.
    List,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SelectionCommitment {
    pub path: SelectorPath,
    pub source_root_hash: Hash32,
    pub selected_hash: Hash32,
    pub selected_len: u64,
    /// The view of the node `selected_hash` is taken over. See
    /// [`SelectionPayloadKind`].
    pub payload_kind: SelectionPayloadKind,
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

/// A list's authenticated shape, together with the selection that proves it.
///
/// `len` is the value a recur loop is held to. It is *authenticated* rather
/// than index-trusted precisely because `selected` commits to the `0x0A`
/// payload carrying it: the committed root is recomputed from
/// `(len, elements_root)`, so no other length reaches the same root. Reading
/// the same integer straight off a `RasterIndex` node would not be — a nested
/// list node's `len` is never checked against its children's hashes
/// (`RasterIndex::validate`), which is the forged-`len = 0` sweep
/// `docs/proposals/lazy-list-recur.md` exists to close.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthenticatedListMetadata {
    pub len: u64,
    /// `None` for an empty list, which has no Merkle levels at all.
    pub elements_root: Option<Hash32>,
    pub selected: SelectedPayload,
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

/// The one definition of a struct node's commitment: the `b"struct"` domain
/// tag followed by each field's `(name, child_root)`, in declaration order.
///
/// Every producer and verifier of a selection tree must agree on this
/// byte-for-byte — the selection-tree assembler, the draft value hasher, the
/// selection prover, the proof verifier, and `main`'s entry-argument
/// binding all commit to structs, and a divergence between any two of them
/// is a silent verification failure. It lives here, in the crate every side
/// already depends on, so there is nothing to keep in sync.
///
/// Field names are part of the hash (length-prefixed, so `("ab", "c")` and
/// `("a", "bc")` cannot collide). That is what makes a selection path
/// structural rather than advisory: a proof recombining a child through a
/// *different* field's position yields a different root, so
/// `verify_selection_proof` can hold a claimed path to the positions its
/// steps actually prove.
pub fn struct_commitments_root<'a>(
    fields: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Hash32 {
    let mut hasher = Sha256::new();
    hasher.update(b"struct");
    for (name, child_root) in fields {
        hasher.update((name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update(child_root);
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

/// The final step of a list's root: `H(b"list-root" ‖ len ‖ elements_root)`,
/// with the `b"empty"` sentinel standing in for an empty list's absent root.
///
/// Factored out because the `0x0A` metadata payload *is* exactly this step —
/// metadata carries `(len, elements_root)` and nothing else, so recomputing its
/// root must reuse this function rather than restate it. A divergence between
/// the two spellings would be a metadata record that verifies against a root no
/// list can produce.
fn list_root_from_elements_root(len: u64, elements_root: Option<&Hash32>) -> Hash32 {
    match elements_root {
        Some(root) => selection_hash(&[b"list-root", &len.to_le_bytes(), root.as_slice()]),
        None => selection_hash(&[b"list-root", &len.to_le_bytes(), b"empty"]),
    }
}

fn list_root_from_hashes(hashes: &[Hash32], len: u64) -> Hash32 {
    if hashes.is_empty() {
        return list_root_from_elements_root(len, None);
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

    list_root_from_elements_root(len, Some(&level[0]))
}

/// Encode the `0x0A` authenticated list metadata payload.
///
/// ```text
/// 0x0A ‖ len:u64 ‖ elements_root:32     (len > 0)   41 bytes
/// 0x0A ‖ 0u64                           (empty)      9 bytes
/// ```
///
/// The encode half of the `0x0A` arm of [`parse_subtree_root`], in the same
/// encode-and-compare spirit as [`encode_index_leaf_payload`]: producers build
/// the bytes here so there is exactly one spelling of a metadata record, and
/// the round-trip is covered by `list_metadata_payload_folds_to_the_list_root`.
///
/// `elements_root` is the top of a list node's Merkle levels — for a raster
/// index, `merkle_levels.last().hashes[0]`, which `RasterIndex::validate`
/// already guarantees is exactly one hash. `None` encodes the empty list.
pub fn encode_list_metadata_payload(len: u64, elements_root: Option<Hash32>) -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + 8 + 32);
    payload.push(0x0A);
    payload.extend_from_slice(&len.to_le_bytes());
    if let Some(root) = elements_root {
        payload.extend_from_slice(&root);
    }
    payload
}

/// Encode the `0x0B` bytes-page payload.
///
/// ```text
/// 0x0B ‖ index:u64 ‖ offset:u64 ‖ len:u64 ‖ bytes
/// root = H(b"bytes-page" ‖ index ‖ offset ‖ len ‖ bytes)
/// ```
pub fn encode_bytes_page_payload(index: u64, offset: u64, len: u64, bytes: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(1 + 8 + 8 + 8 + bytes.len());
    payload.push(0x0B);
    payload.extend_from_slice(&index.to_le_bytes());
    payload.extend_from_slice(&offset.to_le_bytes());
    payload.extend_from_slice(&len.to_le_bytes());
    payload.extend_from_slice(bytes);
    payload
}

/// Domain-separated root of a bytes page. Distinct from the leaf domain so a
/// page cannot collide with a `String` holding the same bytes.
pub fn bytes_page_root(index: u64, offset: u64, len: u64, bytes: &[u8]) -> Hash32 {
    selection_hash(&[
        b"bytes-page",
        &index.to_le_bytes(),
        &offset.to_le_bytes(),
        &len.to_le_bytes(),
        bytes,
    ])
}

/// Recompute a selection payload's structural root from its bytes.
///
/// # Payload tags
///
/// The tag space is contiguous and small, so it is allocated **here**, centrally,
/// rather than negotiated per proposal — two documents each reserving a byte is
/// how collisions happen. Add a row before adding an arm.
///
/// | tag | payload | defined by |
/// | --- | --- | --- |
/// | `0x00` | leaf | — |
/// | `0x01` | struct | — |
/// | `0x02` | list | — |
/// | `0x03` | unit | — |
/// | `0x04` | map | — |
/// | `0x05`–`0x08` | enum unit / newtype / tuple / struct | — |
/// | `0x09` | list handle (stored root, skipped body) | `bounded-collections.md` |
/// | `0x0A` | list metadata (`len`, `elements_root`) | `lazy-list-recur.md` |
/// | `0x0B` | bytes page | `paged-bytes.md` |
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
            let mut fields = Vec::with_capacity(field_count);
            for _ in 0..field_count {
                let name = parse_utf8(bytes, offset)?;
                let child_len = parse_u64(bytes, offset)? as usize;
                let end = offset.checked_add(child_len)?;
                let child_bytes = bytes.get(*offset..end)?;
                let mut child_offset = 0;
                let child_root = parse_subtree_root(child_bytes, &mut child_offset)?;
                if child_offset != child_bytes.len() {
                    return None;
                }
                *offset = end;
                fields.push((String::from_utf8(name).ok()?, child_root));
            }

            Some(struct_commitments_root(
                fields
                    .iter()
                    .map(|(name, root)| (name.as_str(), root.as_slice())),
            ))
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
        // list-handle: `[root:32][len:8][inner_len:8][inner: a 0x02 list
        // payload]`. The stored root is the list's Merkle root, so a parent
        // struct's structural root is recomputed in O(1) — the inline elements
        // are skipped, never re-Merkleized (see
        // `docs/proposals/bounded-collections.md`, phase 2). The elements stay
        // present so the list remains selectable; a selection *into* the list
        // still proves each element against this same root, which is where a
        // handle whose stored root disagrees with its elements is caught.
        0x09 => {
            let root_end = offset.checked_add(32)?;
            let root: Hash32 = bytes.get(*offset..root_end)?.try_into().ok()?;
            *offset = root_end;
            let _len = parse_u64(bytes, offset)?;
            let inner_len = parse_u64(bytes, offset)? as usize;
            let inner_end = offset.checked_add(inner_len)?;
            // Bounds-check the inline region, then skip it without parsing.
            let _ = bytes.get(*offset..inner_end)?;
            *offset = inner_end;
            Some(root)
        }
        // list-metadata: `[len:8]`, plus `[elements_root:32]` when `len > 0`.
        // Proves a list's length and element root without carrying an element,
        // which is what lets a recur source be traced in O(1) instead of
        // O(list) (see `docs/proposals/lazy-list-recur.md` §1).
        //
        // The root is *recomputed* from `(len, elements_root)`, so a forged
        // length yields a different root and fails to fold. That makes this
        // strictly stronger than the `0x09` handle above, which returns its
        // stored root and ignores its stored `len` — a handle's length field is
        // not authenticated and cannot be used as a loop bound.
        0x0A => {
            let len = parse_u64(bytes, offset)?;
            let elements_root = if len == 0 {
                None
            } else {
                let root_end = offset.checked_add(32)?;
                let root: Hash32 = bytes.get(*offset..root_end)?.try_into().ok()?;
                *offset = root_end;
                Some(root)
            };
            Some(list_root_from_elements_root(len, elements_root.as_ref()))
        }
        0x0B => {
            let index = parse_u64(bytes, offset)?;
            let page_offset = parse_u64(bytes, offset)?;
            let len = parse_u64(bytes, offset)?;
            let end = offset.checked_add(len as usize)?;
            let page_bytes = bytes.get(*offset..end)?;
            *offset = end;
            Some(bytes_page_root(index, page_offset, len, page_bytes))
        }
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

/// Recompute the raster **structural root** of a canonical payload — the same
/// hash a manifest entry commits for a value (`encode_raster_value`'s root) and
/// the one `verify_selection_proof` walks a selection against. It is a pure,
/// public function of the payload bytes alone (no index needed), so a chain
/// verifier holding a program's `output.bin` can recompute the manifest-side
/// link hash without any runtime-crate internals. Returns `None` if `bytes` is
/// not a well-formed, fully-consumed payload. See `docs/proposals/program-chain.md`.
pub fn payload_structural_root(bytes: &[u8]) -> Option<Hash32> {
    let mut offset = 0;
    let root = parse_subtree_root(bytes, &mut offset)?;
    // A valid payload is consumed exactly; trailing bytes mean a malformed or
    // truncated artifact, which must not silently produce a root.
    if offset != bytes.len() {
        return None;
    }
    Some(root)
}

/// Check that `step` proves exactly the descent `segment` names.
///
/// Recombining hashes only proves that *some* child sits under the root; it
/// says nothing about which one the caller claimed. Since a
/// `SelectionCommitment` is trusted elsewhere by its `path` (that is how a
/// consumer knows which value it is looking at), each step must be pinned
/// to its path segment, or a witness could prove the bytes at one field
/// while claiming the path of another.
fn step_proves_segment(step: &SelectionProofStep, segment: &SelectorSegment) -> bool {
    match (step, segment) {
        (
            SelectionProofStep::Struct {
                field_index,
                field_names,
                ..
            },
            SelectorSegment::Field(name),
        ) => field_names
            .get(*field_index as usize)
            .is_some_and(|proven| proven == name),
        // Both index forms are pinned the same way: the proof must prove the
        // index the segment claims. For `BoundIndex` this is necessary but not
        // sufficient — `verify_bound_index_bindings` is what ties that index to
        // the authorized value it claims to come from.
        (SelectionProofStep::List { index, .. }, segment) => {
            segment.descent() == SelectorDescent::Index(*index)
        }
        // A range step is pinned to a range segment by its start; the slice
        // width is checked against the payload in `fold_list_range`.
        (
            SelectionProofStep::ListRange { start, .. },
            SelectorSegment::Range { start: claimed, .. },
        ) => start == claimed,
        _ => false,
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
    // One step per path segment, outermost first in both — anything else is
    // a proof that does not describe the path it claims.
    if proof.steps.len() != proof.path.segments.len() {
        return false;
    }
    if !proof
        .steps
        .iter()
        .zip(proof.path.segments.iter())
        .all(|(step, segment)| step_proves_segment(step, segment))
    {
        return false;
    }

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
                field_names,
                siblings,
            } => {
                let field_index = *field_index as usize;
                let field_count = field_names.len();
                if field_index >= field_count || siblings.len() + 1 != field_count {
                    return false;
                }

                let mut child_roots = Vec::with_capacity(field_count);
                let mut sibling_iter = siblings.iter();
                for idx in 0..field_count {
                    if idx == field_index {
                        child_roots.push(current_hash);
                    } else if let Some(sibling) = sibling_iter.next() {
                        child_roots.push(*sibling);
                    } else {
                        return false;
                    }
                }

                struct_commitments_root(
                    field_names
                        .iter()
                        .map(String::as_str)
                        .zip(child_roots.iter().map(Hash32::as_slice)),
                )
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

/// Re-derive the canonical selection payload a leaf holding `index` at `width`
/// commits to: `[0x00][len: u64 LE][fixed-width LE bytes]`.
///
/// This is the *encode* half of the encode-and-compare check in
/// [`crate::trace::verify_bound_index_bindings`]. Comparing an encoding rather
/// than decoding the committed bytes keeps every integer decoder out of the
/// guest and leaves no room for two spellings of the same value: the leaf
/// encoding is fixed-width, so one `(index, width)` pair has exactly one
/// payload. Mirrors the `0x00` arm of `parse_subtree_root` and the runtime's
/// `encode_leaf_bytes`; a divergence between them is a verification failure, so
/// the round-trip is covered by `bound_index_payload_matches_encoded_leaf`.
///
/// Returns `None` when `index` does not fit `width` (see [`IndexWidth::encode`]).
pub fn encode_index_leaf_payload(index: u64, width: IndexWidth) -> Option<Vec<u8>> {
    let leaf_bytes = width.encode(index)?;
    let mut payload = Vec::with_capacity(1 + 8 + leaf_bytes.len());
    payload.push(0x00);
    payload.extend_from_slice(&(leaf_bytes.len() as u64).to_le_bytes());
    payload.extend_from_slice(&leaf_bytes);
    Some(payload)
}

/// Whether a payload's leading tag is the view `kind` claims.
///
/// This is the general form of the rule `lazy-list-recur.md` §1 states: a
/// consumer pins the payload kind it expects. Two payload forms can fold to the
/// same root — a `0x02` list and its `0x0A` metadata both do — so folding alone
/// does not establish *which* was supplied. Accepting a `0x02` where metadata
/// was recorded would not forge the length (a full-list fold authenticates its
/// own length too), but it would put the whole-list payload back in the witness,
/// which is the `O(list)` cost metadata exists to remove.
fn payload_matches_kind(bytes: &[u8], kind: SelectionPayloadKind) -> bool {
    match (bytes.first(), kind) {
        (Some(0x0A), SelectionPayloadKind::List) => true,
        (Some(0x0A), SelectionPayloadKind::Raw) => false,
        (Some(_), SelectionPayloadKind::Raw) => true,
        // A `List` commitment accepts nothing but metadata — in particular not
        // the `0x02` list it was derived from.
        (Some(_), SelectionPayloadKind::List) => false,
        (None, _) => false,
    }
}

/// Decode a `0x0B` page payload into `(index, offset, len, bytes)`.
pub fn parse_bytes_page_payload(bytes: &[u8]) -> Option<(u64, u64, u64, &[u8])> {
    if bytes.first().copied() != Some(0x0B) {
        return None;
    }
    let mut offset = 1usize;
    let index = parse_u64(bytes, &mut offset)?;
    let page_offset = parse_u64(bytes, &mut offset)?;
    let len = parse_u64(bytes, &mut offset)?;
    let end = offset.checked_add(len as usize)?;
    let page_bytes = bytes.get(offset..end)?;
    if end != bytes.len() {
        return None;
    }
    Some((index, page_offset, len, page_bytes))
}

fn u64_leaf_root(value: u64) -> Hash32 {
    selection_hash(&[b"leaf", &value.to_le_bytes()])
}

/// Geometry rules 2–3 from `paged-bytes.md` §3.1, run where the witness is
/// in scope. A non-page payload is accepted (nothing to check).
pub fn verify_bytes_page_geometry(witness: &SelectionWitness) -> bool {
    let Some((index, page_offset, len, page_bytes)) = parse_bytes_page_payload(&witness.bytes)
    else {
        return true;
    };
    if len != page_bytes.len() as u64 {
        return false;
    }

    let mut bytes_step = None;
    let mut list_len = None;
    for step in &witness.proof.steps {
        match step {
            SelectionProofStep::Struct { field_names, .. }
                if field_names.as_slice() == ["byte_len", "page_size", "pages"] =>
            {
                bytes_step = Some(step);
            }
            SelectionProofStep::List {
                index: step_index,
                len: step_len,
                ..
            } if *step_index == index => {
                list_len = Some(*step_len);
            }
            _ => {}
        }
    }

    let Some(SelectionProofStep::Struct {
        field_index,
        field_names,
        siblings,
    }) = bytes_step
    else {
        return true;
    };
    if field_names.get(*field_index as usize).map(String::as_str) != Some("pages") {
        return false;
    }
    if siblings.len() != 2 {
        return false;
    }
    let Some(page_count) = list_len else {
        return false;
    };
    if index >= page_count {
        return false;
    }

    let last = index + 1 == page_count;
    let page_size = if index > 0 {
        if page_offset % index != 0 {
            return false;
        }
        page_offset / index
    } else if !last {
        if page_offset != 0 {
            return false;
        }
        len
    } else {
        if page_offset != 0 {
            return false;
        }
        let byte_len = len;
        if u64_leaf_root(byte_len) != siblings[0] {
            return false;
        }
        return page_count == 1;
    };

    if page_size == 0 || u64_leaf_root(page_size) != siblings[1] {
        return false;
    }
    if page_offset != index.saturating_mul(page_size) {
        return false;
    }

    if last {
        let byte_len = page_offset + len;
        if u64_leaf_root(byte_len) != siblings[0] {
            return false;
        }
        let expected_len = core::cmp::min(page_size, byte_len.saturating_sub(page_offset));
        if len != expected_len {
            return false;
        }
        page_count == byte_len.div_ceil(page_size)
    } else {
        len == page_size
    }
}

pub fn verify_selection_witness(
    commitment: &SelectionCommitment,
    witness: &SelectionWitness,
) -> bool {
    if witness.proof.path != commitment.path
        || witness.proof.root_hash != commitment.source_root_hash
        || witness.bytes.len() as u64 != commitment.selected_len
        || selection_payload_hash(&witness.bytes) != commitment.selected_hash
        || !payload_matches_kind(&witness.bytes, commitment.payload_kind)
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum AuthValue<T> {
    Storage(StorageValue<T>),
    Inline(T),
}

impl<T> AuthValue<T> {
    pub fn inline(value: T) -> Self {
        Self::Inline(value)
    }

    pub fn storage(value: StorageValue<T>) -> Self {
        Self::Storage(value)
    }

    pub fn into_inner(self) -> T {
        match self {
            Self::Storage(storage) => storage.value,
            Self::Inline(value) => value,
        }
    }

    pub fn as_storage(&self) -> Option<&StorageValue<T>> {
        match self {
            Self::Storage(storage) => Some(storage),
            Self::Inline(_) => None,
        }
    }
}

/// A resolved storage value carrying both identity metadata and the typed value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct StorageValue<T> {
    pub reference: StorageRef,
    pub bytes: Vec<u8>,
    pub selector: SelectorPath,
    pub selection: SelectionCommitment,
    pub value: T,
}

impl<T> StorageValue<T> {
    pub fn new(reference: StorageRef, bytes: Vec<u8>, value: T) -> Self {
        Self::new_with_selection(
            reference,
            bytes,
            SelectorPath::default(),
            SelectionCommitment::default(),
            value,
        )
    }

    pub fn new_with_selection(
        reference: StorageRef,
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

// `Vec<T>` is deliberately not `Selectable`: collections in Rastered data are
// `List<T>` (unbounded, referenced) and `Block<T>` (bounded, materialized). Both
// live in `crate::collections` and carry the `SchemaNode::List` schema.

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

        assert!(arg.as_storage().is_none());
        assert_eq!(arg.into_inner(), 7);
    }

    #[test]
    fn auth_value_helpers_preserve_storage_metadata() {
        let reference = StorageRef::new(CfsCoordinates(alloc::vec![0]), alloc::vec![4; 32]);
        let arg = AuthValue::storage(StorageValue::new(
            reference.clone(),
            alloc::vec![1, 2, 3],
            9u64,
        ));

        let storage = arg.as_storage().expect("expected storage metadata");
        assert_eq!(storage.reference, reference);
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
        let field_names = alloc::vec!["slice".to_string(), "other".to_string()];
        let struct_root = struct_commitments_root(
            field_names
                .iter()
                .map(String::as_str)
                .zip([list_root.as_slice(), sibling_root.as_slice()]),
        );
        proof.steps.insert(
            0,
            SelectionProofStep::Struct {
                field_index: 0,
                field_names: field_names.clone(),
                siblings: alloc::vec![sibling_root],
            },
        );
        // The struct step must be pinned to a matching path segment; the range
        // slice lives under field 0.
        proof
            .path
            .segments
            .insert(0, SelectorSegment::Field("slice".to_string()));
        proof.root_hash = struct_root;
        assert!(verify_selection_proof(&payload, &proof));
    }

    // ---- `0x0A` authenticated list metadata (`lazy-list-recur.md` §1) ----

    /// The top of a list's Merkle levels — what a `.rindex` stores as
    /// `merkle_levels.last().hashes[0]`, and what metadata carries.
    fn elements_root(element_roots: &[Hash32]) -> Option<Hash32> {
        if element_roots.is_empty() {
            return None;
        }
        let mut level = element_roots.to_vec();
        while level.len() > 1 {
            if level.len() % 2 == 1 {
                level.push(*level.last().unwrap());
            }
            level = level
                .chunks(2)
                .map(|pair| selection_hash(&[b"list-node", &pair[0], &pair[1]]))
                .collect();
        }
        Some(level[0])
    }

    fn metadata_fixture(len: usize) -> (Vec<u8>, Hash32) {
        let element_roots: Vec<Hash32> = (0..len)
            .map(|value| leaf_root(&[value as u8, 0xAB]))
            .collect();
        let payload = encode_list_metadata_payload(len as u64, elements_root(&element_roots));
        (payload, list_root_from_hashes(&element_roots, len as u64))
    }

    fn parsed_root(payload: &[u8]) -> Option<Hash32> {
        let mut offset = 0;
        let root = parse_subtree_root(payload, &mut offset)?;
        // The arm must consume exactly its own bytes: every struct/list parent
        // asserts `child_offset == child_bytes.len()`.
        (offset == payload.len()).then_some(root)
    }

    #[test]
    fn list_metadata_payload_folds_to_the_list_root() {
        // 0 and 1 are the boundary shapes; 2/3/4/5 cover even, odd (the Merkle
        // duplication path), and a second level.
        for len in [0usize, 1, 2, 3, 4, 5, 8, 9] {
            let (payload, expected) = metadata_fixture(len);
            assert_eq!(
                payload.len(),
                if len == 0 { 9 } else { 41 },
                "metadata is 41 bytes, 9 when empty (len {})",
                len
            );
            assert_eq!(
                parsed_root(&payload),
                Some(expected),
                "metadata must recompute the same root the full list folds to (len {})",
                len
            );
        }
    }

    #[test]
    fn empty_list_metadata_uses_the_empty_sentinel() {
        let (payload, expected) = metadata_fixture(0);
        assert_eq!(payload, alloc::vec![0x0A, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(
            expected,
            selection_hash(&[b"list-root", &0u64.to_le_bytes(), b"empty"])
        );
    }

    /// The point of the whole payload form: the length is *recomputed into* the
    /// root, so claiming a different one yields a different root. This is what
    /// the `0x09` handle cannot do — it returns its stored root and ignores its
    /// stored `len`.
    #[test]
    fn forged_list_metadata_length_fails_to_fold() {
        let (payload, honest) = metadata_fixture(5);
        for forged in [0u64, 1, 4, 6, 999] {
            let mut tampered = payload.clone();
            tampered[1..9].copy_from_slice(&forged.to_le_bytes());
            assert_ne!(
                parsed_root(&tampered),
                Some(honest),
                "a metadata record claiming len {} must not fold to the honest root",
                forged
            );
        }
    }

    #[test]
    fn forged_list_metadata_elements_root_fails_to_fold() {
        let (payload, honest) = metadata_fixture(5);
        let mut tampered = payload.clone();
        tampered[9] ^= 0xFF;
        assert_ne!(parsed_root(&tampered), Some(honest));
    }

    #[test]
    fn bytes_page_payload_folds_to_domain_separated_root() {
        let bytes = b"hello-page";
        let payload = encode_bytes_page_payload(1, 4, bytes.len() as u64, bytes);
        assert_eq!(payload[0], 0x0B);
        assert_eq!(
            parsed_root(&payload),
            Some(bytes_page_root(1, 4, bytes.len() as u64, bytes))
        );
        let leaf = encode_leaf(bytes);
        assert_ne!(parsed_root(&payload), parsed_root(&leaf));
    }

    #[test]
    fn parse_bytes_page_payload_reads_coordinates() {
        let bytes = b"abcd";
        let payload = encode_bytes_page_payload(2, 8, 4, bytes);
        let (index, offset, len, page) = parse_bytes_page_payload(&payload).unwrap();
        assert_eq!((index, offset, len, page), (2, 8, 4, bytes.as_slice()));
    }

    #[test]
    fn verify_bytes_page_geometry_accepts_last_page() {
        let bytes = b"x";
        let payload = encode_bytes_page_payload(1, 4, 1, bytes);
        let witness = SelectionWitness {
            bytes: payload,
            proof: SelectionProof {
                path: SelectorPath::default(),
                root_hash: [0; 32],
                steps: alloc::vec![
                    SelectionProofStep::Struct {
                        field_index: 2,
                        field_names: alloc::vec![
                            "byte_len".into(),
                            "page_size".into(),
                            "pages".into(),
                        ],
                        siblings: alloc::vec![u64_leaf_root(5), u64_leaf_root(4)],
                    },
                    SelectionProofStep::List {
                        index: 1,
                        len: 2,
                        siblings: alloc::vec![],
                    },
                ],
            },
        };
        assert!(verify_bytes_page_geometry(&witness));
    }

    #[test]
    fn verify_bytes_page_geometry_rejects_bad_offset() {
        let bytes = b"xxxx";
        let payload = encode_bytes_page_payload(1, 3, 4, bytes);
        let witness = SelectionWitness {
            bytes: payload,
            proof: SelectionProof {
                path: SelectorPath::default(),
                root_hash: [0; 32],
                steps: alloc::vec![
                    SelectionProofStep::Struct {
                        field_index: 2,
                        field_names: alloc::vec![
                            "byte_len".into(),
                            "page_size".into(),
                            "pages".into(),
                        ],
                        siblings: alloc::vec![u64_leaf_root(8), u64_leaf_root(4)],
                    },
                    SelectionProofStep::List {
                        index: 1,
                        len: 2,
                        siblings: alloc::vec![],
                    },
                ],
            },
        };
        assert!(!verify_bytes_page_geometry(&witness));
    }

    #[test]
    fn verify_bytes_page_geometry_rejects_short_non_final_page() {
        let bytes = b"xxx";
        let payload = encode_bytes_page_payload(1, 4, 3, bytes);
        let witness = SelectionWitness {
            bytes: payload,
            proof: SelectionProof {
                path: SelectorPath::default(),
                root_hash: [0; 32],
                steps: alloc::vec![
                    SelectionProofStep::Struct {
                        field_index: 2,
                        field_names: alloc::vec![
                            "byte_len".into(),
                            "page_size".into(),
                            "pages".into(),
                        ],
                        siblings: alloc::vec![u64_leaf_root(12), u64_leaf_root(4)],
                    },
                    SelectionProofStep::List {
                        index: 1,
                        len: 3,
                        siblings: alloc::vec![],
                    },
                ],
            },
        };
        assert!(!verify_bytes_page_geometry(&witness));
    }

    #[test]
    fn verify_bytes_page_geometry_rejects_count_disagreeing_with_ceil() {
        let bytes = b"x";
        let payload = encode_bytes_page_payload(1, 4, 1, bytes);
        let witness = SelectionWitness {
            bytes: payload,
            proof: SelectionProof {
                path: SelectorPath::default(),
                root_hash: [0; 32],
                steps: alloc::vec![
                    SelectionProofStep::Struct {
                        field_index: 2,
                        field_names: alloc::vec![
                            "byte_len".into(),
                            "page_size".into(),
                            "pages".into(),
                        ],
                        siblings: alloc::vec![u64_leaf_root(5), u64_leaf_root(4)],
                    },
                    SelectionProofStep::List {
                        index: 1,
                        len: 3,
                        siblings: alloc::vec![],
                    },
                ],
            },
        };
        assert!(!verify_bytes_page_geometry(&witness));
    }

    #[test]
    fn truncated_list_metadata_is_rejected() {
        let (payload, _) = metadata_fixture(5);
        // Missing the elements root entirely, and a partial one.
        assert_eq!(parsed_root(&payload[..9]), None);
        assert_eq!(parsed_root(&payload[..30]), None);
        // A non-empty length must not accept trailing slack either.
        let mut padded = payload.clone();
        padded.push(0);
        assert_eq!(parsed_root(&padded), None);
    }

    /// Each consumer pins the kind it expects. A range proof reaches for the
    /// element roots and must not silently accept a metadata record instead.
    #[test]
    fn parse_list_child_roots_rejects_metadata() {
        let (payload, _) = metadata_fixture(4);
        assert!(parse_list_child_roots(&payload).is_none());
    }

    fn metadata_witness(len: usize) -> (SelectionCommitment, SelectionWitness) {
        let (payload, root) = metadata_fixture(len);
        let commitment = SelectionCommitment {
            path: SelectorPath::default(),
            source_root_hash: root,
            selected_hash: selection_payload_hash(&payload),
            selected_len: payload.len() as u64,
            payload_kind: SelectionPayloadKind::List,
        };
        let witness = SelectionWitness {
            bytes: payload,
            proof: SelectionProof {
                path: SelectorPath::default(),
                root_hash: root,
                steps: Vec::new(),
            },
        };
        (commitment, witness)
    }

    #[test]
    fn metadata_witness_verifies_against_a_list_kind_commitment() {
        let (commitment, witness) = metadata_witness(5);
        assert!(verify_selection_witness(&commitment, &witness));
    }

    /// A whole-list payload folds to the same root as its metadata and
    /// authenticates its own length just as well — so folding cannot be what
    /// separates them. The recorded kind is, and a `List` commitment refuses
    /// the `0x02` form outright rather than parsing it. That refusal is the
    /// `O(list)` cost this proposal exists to remove.
    #[test]
    fn a_list_kind_commitment_refuses_a_whole_list_payload() {
        let len = 5usize;
        let element_bytes: Vec<Vec<u8>> = (0..len)
            .map(|value| alloc::vec![value as u8, 0xAB])
            .collect();
        let element_roots: Vec<Hash32> =
            element_bytes.iter().map(|bytes| leaf_root(bytes)).collect();
        let encoded: Vec<Vec<u8>> = element_bytes.iter().map(|b| encode_leaf(b)).collect();
        let whole = encode_list(&encoded);
        let root = list_root_from_hashes(&element_roots, len as u64);

        // It really does fold to the same root…
        assert_eq!(parsed_root(&whole), Some(root));
        assert_eq!(parsed_root(&metadata_fixture(len).0), Some(root));

        // …and is still refused where metadata was recorded.
        let commitment = SelectionCommitment {
            path: SelectorPath::default(),
            source_root_hash: root,
            selected_hash: selection_payload_hash(&whole),
            selected_len: whole.len() as u64,
            payload_kind: SelectionPayloadKind::List,
        };
        let witness = SelectionWitness {
            bytes: whole,
            proof: SelectionProof {
                path: SelectorPath::default(),
                root_hash: root,
                steps: Vec::new(),
            },
        };
        assert!(!verify_selection_witness(&commitment, &witness));
    }

    #[test]
    fn a_raw_kind_commitment_refuses_a_metadata_payload() {
        let (mut commitment, witness) = metadata_witness(5);
        commitment.payload_kind = SelectionPayloadKind::Raw;
        assert!(!verify_selection_witness(&commitment, &witness));
    }
}
