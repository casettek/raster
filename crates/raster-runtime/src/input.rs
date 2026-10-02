use raster_core::input::{
    encode_list_metadata_payload, selection_payload_hash, struct_commitments_root,
    AuthenticatedListMetadata, Hash32, ListProofDirection, ListProofSibling, SchemaNode,
    Selectable, SelectedPayload, SelectionCommitment, SelectionPayloadKind, SelectionProof,
    SelectionProofStep, SelectionWitness, SelectorDescent, SelectorPath, SelectorSegment,
    StorageValue,
};
use raster_core::{Error, Result as CoreResult};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::format;
use std::fs;
use std::path::Path;
use std::string::{String, ToString};
use std::vec::Vec;

use crate::raster_index::{
    RasterIndex, RasterNodeKind, RasterRangeSlice, RasterSelection, RasterSelectionLocation,
    RasterStructField,
};
use crate::reader::ReadLimits;
use crate::source::SourceFile;

pub(crate) use raster_core::tree::{
    subtree_payload_and_root, tree_value_from_serialize, typed_value_from_tree, TreeValue,
};
#[cfg(test)]
use raster_core::tree::LIST_HANDLE_HEADER_LEN;


fn parse_leaf_value(type_name: &str, subtree_bytes: &[u8]) -> CoreResult<TreeValue> {
    if type_name == "BytesPage" || subtree_bytes.first().copied() == Some(0x0B) {
        return parse_bytes_page(subtree_bytes);
    }
    if subtree_bytes.first().copied() != Some(0x00) {
        return Err(Error::Serialization(
            "Expected leaf subtree while decoding raster selection".into(),
        ));
    }

    let mut offset = 1usize;
    let len = parse_u64(subtree_bytes, &mut offset)
        .ok_or_else(|| Error::Serialization("Malformed raster leaf payload".into()))?
        as usize;
    let end = offset
        .checked_add(len)
        .ok_or_else(|| Error::Serialization("Malformed raster leaf payload length".into()))?;
    let leaf_bytes = subtree_bytes
        .get(offset..end)
        .ok_or_else(|| Error::Serialization("Malformed raster leaf payload".into()))?;
    if end != subtree_bytes.len() {
        return Err(Error::Serialization(
            "Malformed raster leaf payload trailing bytes".into(),
        ));
    }

    match type_name {
        "bool" => match leaf_bytes {
            [0] => Ok(TreeValue::Bool(false)),
            [1] => Ok(TreeValue::Bool(true)),
            _ => Err(Error::Serialization(
                "Malformed raster bool leaf payload".into(),
            )),
        },
        "u8" => leaf_bytes
            .first()
            .copied()
            .map(TreeValue::U8)
            .ok_or_else(|| Error::Serialization("Malformed raster u8 leaf payload".into())),
        "u16" => Ok(TreeValue::U16(read_fixed_u16(leaf_bytes, "u16")?)),
        "u32" => Ok(TreeValue::U32(read_fixed_u32(leaf_bytes, "u32")?)),
        "u64" | "usize" => Ok(TreeValue::U64(read_fixed_u64(leaf_bytes, type_name)?)),
        "i8" => leaf_bytes
            .first()
            .copied()
            .map(|value| TreeValue::I8(value as i8))
            .ok_or_else(|| Error::Serialization("Malformed raster i8 leaf payload".into())),
        "i16" => Ok(TreeValue::I16(read_fixed_i16(leaf_bytes, "i16")?)),
        "i32" => Ok(TreeValue::I32(read_fixed_i32(leaf_bytes, "i32")?)),
        "i64" => Ok(TreeValue::I64(read_fixed_i64(leaf_bytes, "i64")?)),
        "String" => {
            let mut string_offset = 0usize;
            let string_len = parse_u64(leaf_bytes, &mut string_offset).ok_or_else(|| {
                Error::Serialization("Malformed raster string leaf payload".into())
            })? as usize;
            let string_end = string_offset.checked_add(string_len).ok_or_else(|| {
                Error::Serialization("Malformed raster string leaf payload length".into())
            })?;
            let value = leaf_bytes.get(string_offset..string_end).ok_or_else(|| {
                Error::Serialization("Malformed raster string leaf payload".into())
            })?;
            if string_end != leaf_bytes.len() {
                return Err(Error::Serialization(
                    "Malformed raster string leaf payload trailing bytes".into(),
                ));
            }
            Ok(TreeValue::String(
                std::str::from_utf8(value)
                    .map_err(|e| {
                        Error::Serialization(format!(
                            "Malformed raster string leaf payload UTF-8: {}",
                            e
                        ))
                    })?
                    .to_string(),
            ))
        }
        _ => Err(Error::Serialization(format!(
            "Unsupported raster leaf type '{}'",
            type_name
        ))),
    }
}

fn parse_bytes_page(bytes: &[u8]) -> CoreResult<TreeValue> {
    if bytes.first().copied() != Some(0x0B) {
        return Err(Error::Serialization(
            "Expected 0x0B bytes-page payload".into(),
        ));
    }
    let mut offset = 1usize;
    let index = parse_u64(bytes, &mut offset).ok_or_else(|| {
        Error::Serialization("Malformed bytes-page index".into())
    })?;
    let page_offset = parse_u64(bytes, &mut offset).ok_or_else(|| {
        Error::Serialization("Malformed bytes-page offset".into())
    })?;
    let len = parse_u64(bytes, &mut offset).ok_or_else(|| {
        Error::Serialization("Malformed bytes-page len".into())
    })?;
    let end = offset.checked_add(len as usize).ok_or_else(|| {
        Error::Serialization("Malformed bytes-page payload length".into())
    })?;
    let page_bytes = bytes.get(offset..end).ok_or_else(|| {
        Error::Serialization("Malformed bytes-page payload".into())
    })?;
    if end != bytes.len() {
        return Err(Error::Serialization(
            "Malformed bytes-page payload trailing bytes".into(),
        ));
    }
    Ok(TreeValue::BytesPage {
        index,
        offset: page_offset,
        len,
        bytes: page_bytes.to_vec(),
    })
}

/// Byte source for a `.rastered` file. Memory/mmap slices and retained
/// `Read` handles both implement this so a page select does not pull the
/// whole file into RAM.
pub(crate) trait RasterData {
    fn read_subtree(&self, offset: u64, len: u64) -> CoreResult<Vec<u8>>;
}

impl RasterData for [u8] {
    fn read_subtree(&self, offset: u64, len: u64) -> CoreResult<Vec<u8>> {
        raster_subtree_bytes(self, offset, len).map(|bytes| bytes.to_vec())
    }
}

impl RasterData for Vec<u8> {
    fn read_subtree(&self, offset: u64, len: u64) -> CoreResult<Vec<u8>> {
        raster_subtree_bytes(self, offset, len).map(|bytes| bytes.to_vec())
    }
}

impl RasterData for SourceFile {
    fn read_subtree(&self, offset: u64, len: u64) -> CoreResult<Vec<u8>> {
        self.read_range(offset, len)
    }
}

pub(crate) fn tree_value_from_raster_location(
    index: &RasterIndex,
    data: &impl RasterData,
    selection: &RasterSelectionLocation,
) -> CoreResult<TreeValue> {
    let Some(range) = selection.range else {
        return tree_value_from_raster_node(index, data, selection.node_id, selection.offset);
    };

    // A range selects a slice of a list node's elements, which is a `List`
    // value of its own — never a `ListHandle`, since the slice is not the
    // committed collection and carries no stored root.
    if index.list_len(selection.node_id)?.is_none() {
        return Err(Error::Other(
            "Range selection resolved to a non-list raster node".into(),
        ));
    }
    let slice = index.list_elements(selection.node_id, range.start, range.end)?;

    let mut values = Vec::with_capacity(slice.len());
    for child in slice {
        values.push(tree_value_from_raster_node(
            index,
            data,
            child,
            index.child_position(selection.node_position, child)?,
        )?);
    }
    Ok(TreeValue::List(values))
}

/// The selection payload for a located region: the region itself, or — for a
/// range — that region behind a synthesized `0x02 ‖ k` list header.
///
/// The header is the only synthesized part. The element bytes are copied
/// straight through, contiguous and in order, because that is exactly how a
/// list node lays them out (see [`RasterRangeSlice`]).
fn raster_selection_payload(
    data: &impl RasterData,
    offset: u64,
    len: u64,
    range: Option<RasterRangeSlice>,
) -> CoreResult<Vec<u8>> {
    let region = data.read_subtree(offset, len)?;
    let Some(range) = range else {
        return Ok(region);
    };
    let mut payload = Vec::with_capacity(1 + 8 + region.len());
    payload.push(0x02);
    push_u64(&mut payload, range.count());
    payload.extend_from_slice(&region);
    Ok(payload)
}

/// `position` is the node's data-file position, computed on descent: index
/// offsets are parent-relative (`rindex04`).
pub(crate) fn tree_value_from_raster_node<D: RasterData + ?Sized>(
    index: &RasterIndex,
    data: &D,
    node_id: u64,
    position: u64,
) -> CoreResult<TreeValue> {
    let node = index.get_node(node_id)?;
    match &node.kind {
        RasterNodeKind::Unit => Ok(TreeValue::Unit),
        RasterNodeKind::Leaf { type_name } => {
            let subtree = data.read_subtree(position, node.len)?;
            parse_leaf_value(type_name, &subtree)
        }
        RasterNodeKind::Struct { fields } => {
            let mut values = Vec::with_capacity(fields.len());
            for field in fields {
                values.push((
                    field.name.clone(),
                    tree_value_from_raster_node(index, data, field.child, index.child_position(position, field.child)?)?,
                ));
            }
            Ok(TreeValue::Struct(values))
        }
        RasterNodeKind::List { len, .. } | RasterNodeKind::ListContinuation { len, .. } => {
            let mut values = Vec::with_capacity(*len as usize);
            for child in index.list_elements(node_id, 0, *len)? {
                values.push(tree_value_from_raster_node(
                    index,
                    data,
                    child,
                    index.child_position(position, child)?,
                )?);
            }
            Ok(TreeValue::List(values))
        }
        RasterNodeKind::Map { entries } => {
            let mut values = Vec::with_capacity(entries.len());
            for entry in entries {
                values.push((
                    tree_value_from_raster_node(index, data, entry.key, index.child_position(position, entry.key)?)?,
                    tree_value_from_raster_node(index, data, entry.value, index.child_position(position, entry.value)?)?,
                ));
            }
            Ok(TreeValue::Map(values))
        }
        RasterNodeKind::EnumUnit { variant } => Ok(TreeValue::EnumUnit(variant.clone())),
        RasterNodeKind::EnumNewtype { variant, child } => Ok(TreeValue::EnumNewtype(
            variant.clone(),
            Box::new(tree_value_from_raster_node(index, data, *child, index.child_position(position, *child)?)?),
        )),
        RasterNodeKind::EnumTuple { variant, elements } => {
            let mut values = Vec::with_capacity(elements.len());
            for child in elements {
                values.push(tree_value_from_raster_node(index, data, *child, index.child_position(position, *child)?)?);
            }
            Ok(TreeValue::EnumTuple(variant.clone(), values))
        }
        RasterNodeKind::EnumStruct { variant, fields } => {
            let mut values = Vec::with_capacity(fields.len());
            for field in fields {
                values.push((
                    field.name.clone(),
                    tree_value_from_raster_node(index, data, field.child, index.child_position(position, field.child)?)?,
                ));
            }
            Ok(TreeValue::EnumStruct(variant.clone(), values))
        }
    }
}

/// A decoded raster value — the rendering-facing view of an artifact.
///
/// Deliberately separate from [`TreeValue`], which is the encoder's internal
/// representation and load-bearing for commitments; making that public would
/// freeze an internal on a rendering use case.
///
/// Two absences are properties of the format, not omissions:
///
/// - **No struct name.** [`RasterNodeKind::Struct`] records field names and no
///   type name, so a struct renders anonymously. Enum variants *are* named.
/// - **No float.** [`parse_leaf_value`] has no float arm because [`TreeValue`]
///   has no float variant; the encoder cannot produce one.
///
/// See `docs/proposals/artifact-inspection.md` §4.1.
#[derive(Debug, Clone, PartialEq)]
pub enum RasterValue {
    Unit,
    Bool(bool),
    /// Every signed and unsigned width, widened. `ty` is the Rust type name the
    /// index recorded, so a renderer can print `353u64` rather than `353`.
    Int { value: i128, ty: &'static str },
    Str { value: String, truncated: bool },
    Bytes {
        index: u64,
        offset: u64,
        len: u64,
        data: Vec<u8>,
        truncated: bool,
    },
    /// `len` is the field count the index declares; `fields` may be shorter.
    ///
    /// A struct is bounded for the same reason a list is: the field table comes
    /// out of the `.rindex`, so its width is data, not a property of any Rust
    /// type that was compiled. A corrupt or hostile index can declare as many
    /// fields as it likes.
    Struct {
        len: u64,
        fields: Vec<(String, RasterValue)>,
        truncated: bool,
    },
    List {
        len: u64,
        elements: Vec<RasterValue>,
        truncated: bool,
    },
    Map {
        len: u64,
        entries: Vec<(RasterValue, RasterValue)>,
        truncated: bool,
    },
    Enum {
        variant: String,
        payload: Option<Box<RasterValue>>,
    },
    /// `ReadLimits::max_depth` reached — the subtree exists and was not walked.
    Elided,
}

/// Walk index + payload into a [`RasterValue`], bounded by `limits`.
///
/// Mirrors [`tree_value_from_raster_node`] node for node, and reuses
/// [`parse_leaf_value`] verbatim for leaves — the reader and the encoder agree
/// on what a leaf is because it is literally the same function. It is a
/// separate walk rather than a conversion from [`TreeValue`] because limits
/// have to bind *during* the descent: converting afterwards would materialize
/// the 100k-element list first, which is the case the limits exist for.
pub(crate) fn raster_value_from_node<D: RasterData + ?Sized>(
    index: &RasterIndex,
    data: &D,
    node_id: u64,
    limits: &ReadLimits,
) -> CoreResult<RasterValue> {
    raster_value_at_depth(index, data, node_id, index.root_position()?, limits, 0)
}

fn raster_value_at_depth<D: RasterData + ?Sized>(
    index: &RasterIndex,
    data: &D,
    node_id: u64,
    position: u64,
    limits: &ReadLimits,
    depth: usize,
) -> CoreResult<RasterValue> {
    let node = index.get_node(node_id)?;

    // A leaf is cheap and self-terminating, so the depth cut applies only to
    // the composites — eliding a scalar would lose information for nothing.
    if depth >= limits.max_depth && !matches!(node.kind, RasterNodeKind::Leaf { .. }) {
        return Ok(RasterValue::Elided);
    }
    let child_depth = depth + 1;

    match &node.kind {
        RasterNodeKind::Unit => Ok(RasterValue::Unit),
        RasterNodeKind::Leaf { type_name } => {
            let subtree = data.read_subtree(position, node.len)?;
            raster_value_from_leaf(parse_leaf_value(type_name, &subtree)?, limits)
        }
        RasterNodeKind::Struct { fields } => {
            raster_value_fields(index, data, fields, position, limits, child_depth)
        }
        RasterNodeKind::List { len, .. } | RasterNodeKind::ListContinuation { len, .. } => {
            let kept = (*len as usize).min(limits.max_list_elements);
            let mut values = Vec::with_capacity(kept);
            for child in index.list_elements(node_id, 0, kept as u64)? {
                values.push(raster_value_at_depth(
                    index,
                    data,
                    child,
                    index.child_position(position, child)?,
                    limits,
                    child_depth,
                )?);
            }
            Ok(RasterValue::List {
                len: *len,
                elements: values,
                truncated: kept < *len as usize,
            })
        }
        RasterNodeKind::Map { entries } => {
            let kept = entries.len().min(limits.max_list_elements);
            let mut values = Vec::with_capacity(kept);
            for entry in &entries[..kept] {
                values.push((
                    raster_value_at_depth(index, data, entry.key, index.child_position(position, entry.key)?, limits, child_depth)?,
                    raster_value_at_depth(index, data, entry.value, index.child_position(position, entry.value)?, limits, child_depth)?,
                ));
            }
            Ok(RasterValue::Map {
                len: entries.len() as u64,
                entries: values,
                truncated: kept < entries.len(),
            })
        }
        RasterNodeKind::EnumUnit { variant } => Ok(RasterValue::Enum {
            variant: variant.clone(),
            payload: None,
        }),
        RasterNodeKind::EnumNewtype { variant, child } => Ok(RasterValue::Enum {
            variant: variant.clone(),
            payload: Some(Box::new(raster_value_at_depth(index, data, *child, index.child_position(position, *child)?, limits, child_depth)?)),
        }),
        RasterNodeKind::EnumTuple { variant, elements } => {
            let kept = elements.len().min(limits.max_list_elements);
            let mut values = Vec::with_capacity(kept);
            for child in &elements[..kept] {
                values.push(raster_value_at_depth(index, data, *child, index.child_position(position, *child)?, limits, child_depth)?);
            }
            Ok(RasterValue::Enum {
                variant: variant.clone(),
                payload: Some(Box::new(RasterValue::List {
                    len: elements.len() as u64,
                    elements: values,
                    truncated: kept < elements.len(),
                })),
            })
        }
        RasterNodeKind::EnumStruct { variant, fields } => Ok(RasterValue::Enum {
            variant: variant.clone(),
            payload: Some(Box::new(raster_value_fields(index, data, fields, position, limits, child_depth)?)),
        }),
    }
}

fn raster_value_fields<D: RasterData + ?Sized>(
    index: &RasterIndex,
    data: &D,
    fields: &[RasterStructField],
    position: u64,
    limits: &ReadLimits,
    depth: usize,
) -> CoreResult<RasterValue> {
    let kept = fields.len().min(limits.max_struct_fields);
    let mut values = Vec::with_capacity(kept);
    for field in &fields[..kept] {
        values.push((
            field.name.clone(),
            raster_value_at_depth(index, data, field.child, index.child_position(position, field.child)?, limits, depth)?,
        ));
    }
    Ok(RasterValue::Struct {
        len: fields.len() as u64,
        fields: values,
        truncated: kept < fields.len(),
    })
}

/// Narrow a leaf [`TreeValue`] to its rendering view, applying the byte limit.
///
/// Only leaf-shaped variants can arrive here — [`parse_leaf_value`] produces
/// nothing else — so the composite arms are unreachable in practice and are
/// mapped rather than panicking.
fn raster_value_from_leaf(value: TreeValue, limits: &ReadLimits) -> CoreResult<RasterValue> {
    let int = |value: i128, ty: &'static str| Ok(RasterValue::Int { value, ty });
    match value {
        TreeValue::Unit => Ok(RasterValue::Unit),
        TreeValue::Bool(v) => Ok(RasterValue::Bool(v)),
        TreeValue::U8(v) => int(v as i128, "u8"),
        TreeValue::U16(v) => int(v as i128, "u16"),
        TreeValue::U32(v) => int(v as i128, "u32"),
        TreeValue::U64(v) => int(v as i128, "u64"),
        TreeValue::I8(v) => int(v as i128, "i8"),
        TreeValue::I16(v) => int(v as i128, "i16"),
        TreeValue::I32(v) => int(v as i128, "i32"),
        TreeValue::I64(v) => int(v as i128, "i64"),
        TreeValue::String(text) => {
            // Truncate on a char boundary: `max_bytes_per_leaf` bounds bytes,
            // and cutting mid-codepoint would produce a string that is not a
            // string.
            let keep = text
                .char_indices()
                .map(|(idx, _)| idx)
                .chain(std::iter::once(text.len()))
                .take_while(|idx| *idx <= limits.max_bytes_per_leaf)
                .last()
                .unwrap_or(0);
            let truncated = keep < text.len();
            let mut text = text;
            text.truncate(keep);
            Ok(RasterValue::Str {
                value: text,
                truncated,
            })
        }
        TreeValue::BytesPage {
            index,
            offset,
            len,
            bytes,
        } => {
            let keep = bytes.len().min(limits.max_bytes_per_leaf);
            let truncated = keep < bytes.len();
            let mut bytes = bytes;
            bytes.truncate(keep);
            Ok(RasterValue::Bytes {
                index,
                offset,
                len,
                data: bytes,
                truncated,
            })
        }
        other => Err(Error::Serialization(format!(
            "Raster leaf decoded to a non-leaf value: {:?}",
            other
        ))),
    }
}

pub(crate) fn raster_subtree_bytes(data_bytes: &[u8], offset: u64, len: u64) -> CoreResult<&[u8]> {
    let start = usize::try_from(offset)
        .map_err(|_| Error::Serialization("Raster subtree offset does not fit in usize".into()))?;
    let len = usize::try_from(len)
        .map_err(|_| Error::Serialization("Raster subtree length does not fit in usize".into()))?;
    let end = start.checked_add(len).ok_or_else(|| {
        Error::Serialization("Raster subtree offset overflowed available address space".into())
    })?;
    data_bytes
        .get(start..end)
        .ok_or_else(|| Error::Serialization("Raster subtree points outside .rastered data".into()))
}

fn read_fixed_u16(bytes: &[u8], type_name: &str) -> CoreResult<u16> {
    let array: [u8; 2] = bytes.try_into().map_err(|_| {
        Error::Serialization(format!("Malformed raster {} leaf payload", type_name))
    })?;
    Ok(u16::from_le_bytes(array))
}

fn read_fixed_u32(bytes: &[u8], type_name: &str) -> CoreResult<u32> {
    let array: [u8; 4] = bytes.try_into().map_err(|_| {
        Error::Serialization(format!("Malformed raster {} leaf payload", type_name))
    })?;
    Ok(u32::from_le_bytes(array))
}

fn read_fixed_u64(bytes: &[u8], type_name: &str) -> CoreResult<u64> {
    let array: [u8; 8] = bytes.try_into().map_err(|_| {
        Error::Serialization(format!("Malformed raster {} leaf payload", type_name))
    })?;
    Ok(u64::from_le_bytes(array))
}

fn read_fixed_i16(bytes: &[u8], type_name: &str) -> CoreResult<i16> {
    let array: [u8; 2] = bytes.try_into().map_err(|_| {
        Error::Serialization(format!("Malformed raster {} leaf payload", type_name))
    })?;
    Ok(i16::from_le_bytes(array))
}

fn read_fixed_i32(bytes: &[u8], type_name: &str) -> CoreResult<i32> {
    let array: [u8; 4] = bytes.try_into().map_err(|_| {
        Error::Serialization(format!("Malformed raster {} leaf payload", type_name))
    })?;
    Ok(i32::from_le_bytes(array))
}

fn read_fixed_i64(bytes: &[u8], type_name: &str) -> CoreResult<i64> {
    let array: [u8; 8] = bytes.try_into().map_err(|_| {
        Error::Serialization(format!("Malformed raster {} leaf payload", type_name))
    })?;
    Ok(i64::from_le_bytes(array))
}

fn selection_hash(parts: &[&[u8]]) -> Hash32 {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn parse_u64(bytes: &[u8], offset: &mut usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    let slice = bytes.get(*offset..end)?;
    let value = u64::from_le_bytes(slice.try_into().ok()?);
    *offset = end;
    Some(value)
}

fn list_root_and_proof(
    hashes: &[Hash32],
    index: usize,
) -> CoreResult<(Hash32, Vec<ListProofSibling>)> {
    if index >= hashes.len() {
        return Err(Error::Other(format!(
            "Selector index '{}' was not found in external input",
            index
        )));
    }

    let len = hashes.len() as u64;
    if hashes.is_empty() {
        return Ok((
            selection_hash(&[b"list-root", &len.to_le_bytes(), b"empty"]),
            Vec::new(),
        ));
    }

    let mut siblings = Vec::new();
    let mut idx = index;
    let mut level = hashes.to_vec();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            let last = level.last().cloned().unwrap();
            level.push(last);
        }

        let sibling_index = if idx % 2 == 0 { idx + 1 } else { idx - 1 };
        siblings.push(ListProofSibling {
            direction: if idx % 2 == 0 {
                ListProofDirection::Right
            } else {
                ListProofDirection::Left
            },
            hash: level[sibling_index],
        });

        let mut next = Vec::with_capacity(level.len() / 2);
        for pair in level.chunks(2) {
            next.push(selection_hash(&[
                b"list-node",
                pair[0].as_slice(),
                pair[1].as_slice(),
            ]));
        }
        idx /= 2;
        level = next;
    }

    Ok((
        selection_hash(&[b"list-root", &len.to_le_bytes(), level[0].as_slice()]),
        siblings,
    ))
}

/// Root and boundary siblings for the contiguous slice `[start, end)`.
/// Sibling consumption order (left boundary before right boundary, level by
/// level) must match `fold_list_range` in raster-core.
fn list_root_and_range_proof(
    hashes: &[Hash32],
    start: usize,
    end: usize,
) -> CoreResult<(Hash32, Vec<ListProofSibling>)> {
    if start >= end || end > hashes.len() {
        return Err(Error::Other(format!(
            "Selector range '{}..{}' is out of bounds for list of length {}",
            start,
            end,
            hashes.len()
        )));
    }

    let len = hashes.len() as u64;
    let mut siblings = Vec::new();
    let mut lo = start;
    let mut hi = end;
    let mut level = hashes.to_vec();
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
            // hi == width: odd-width duplication, the verifier derives the
            // partner from its own last node — no witness data.
            hi += 1;
        }

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
        lo /= 2;
        hi /= 2;
    }

    Ok((
        selection_hash(&[b"list-root", &len.to_le_bytes(), level[0].as_slice()]),
        siblings,
    ))
}

fn find_struct_field<'a>(entries: &'a [(String, TreeValue)], name: &str) -> Option<&'a TreeValue> {
    entries
        .iter()
        .find(|(field_name, _)| field_name == name)
        .map(|(_, value)| value)
}

pub(crate) struct ProvenSelection {
    pub(crate) selected_value: TreeValue,
    pub(crate) selected_bytes: Vec<u8>,
    pub(crate) root_hash: Hash32,
    pub(crate) steps: Vec<SelectionProofStep>,
}

pub(crate) fn prove_selection(
    schema: &SchemaNode,
    value: &TreeValue,
    segments: &[SelectorSegment],
) -> CoreResult<ProvenSelection> {
    if segments.is_empty() {
        // A whole `List<T>` selection yields the plain inline list bytes (not the
        // `0x09` handle wrapper): its recomputed root equals the handle root, and
        // downstream consumers select/iterate it like any other list.
        let normalized = match value {
            TreeValue::ListHandle(values) => TreeValue::List(values.clone()),
            other => other.clone(),
        };
        let (selected_bytes, root_hash) = subtree_payload_and_root(&normalized)?;
        return Ok(ProvenSelection {
            selected_value: normalized,
            selected_bytes,
            root_hash,
            steps: Vec::new(),
        });
    }

    // Matched by descent, so a data-sourced index builds the same `List` proof
    // step a literal one does. Its provenance is discharged separately, against
    // the step's storage map (`verify_bound_index_bindings`).
    match (segments[0].descent(), schema, value) {
        (
            SelectorDescent::Field(field_name),
            SchemaNode::Struct { fields, .. },
            TreeValue::Struct(entries),
        ) => {
            let target_index = fields
                .iter()
                .position(|field| field.name == field_name)
                .ok_or_else(|| {
                    Error::Other(format!("Selector field '{}' was not found", field_name))
                })?;
            let target_field = &fields[target_index];
            let child_value = find_struct_field(entries, field_name).ok_or_else(|| {
                Error::Serialization(format!(
                    "Missing field '{}' in schema-driven value",
                    field_name
                ))
            })?;
            let child = prove_selection(&target_field.schema, child_value, &segments[1..])?;

            let mut siblings = Vec::with_capacity(fields.len().saturating_sub(1));
            let mut child_roots = Vec::with_capacity(fields.len());
            for (idx, field) in fields.iter().enumerate() {
                if idx == target_index {
                    child_roots.push(child.root_hash.clone());
                } else {
                    let sibling_value =
                        find_struct_field(entries, &field.name).ok_or_else(|| {
                            Error::Serialization(format!(
                                "Missing field '{}' in schema-driven value",
                                field.name
                            ))
                        })?;
                    let (_, sibling_root) = subtree_payload_and_root(sibling_value)?;
                    siblings.push(sibling_root);
                    child_roots.push(sibling_root);
                }
            }

            let root_hash = struct_commitments_root(
                fields
                    .iter()
                    .map(|field| field.name.as_str())
                    .zip(child_roots.iter().map(Hash32::as_slice)),
            );

            let mut steps = Vec::with_capacity(child.steps.len() + 1);
            steps.push(SelectionProofStep::Struct {
                field_index: target_index as u64,
                field_names: fields.iter().map(|field| field.name.clone()).collect(),
                siblings,
            });
            steps.extend(child.steps);

            Ok(ProvenSelection {
                selected_value: child.selected_value,
                selected_bytes: child.selected_bytes,
                root_hash,
                steps,
            })
        }
        (
            SelectorDescent::Index(index),
            SchemaNode::List { element, .. },
            TreeValue::List(values) | TreeValue::ListHandle(values),
        ) => {
            let idx = index as usize;
            let child_value = values
                .get(idx)
                .ok_or_else(|| Error::Other(format!("Selector index '{}' was not found", index)))?;
            let child = prove_selection(element, child_value, &segments[1..])?;

            let mut hashes = Vec::with_capacity(values.len());
            for (position, item) in values.iter().enumerate() {
                if position == idx {
                    hashes.push(child.root_hash.clone());
                } else {
                    hashes.push(subtree_payload_and_root(item)?.1);
                }
            }
            let (root_hash, siblings) = list_root_and_proof(&hashes, idx)?;

            let mut steps = Vec::with_capacity(child.steps.len() + 1);
            steps.push(SelectionProofStep::List {
                index,
                len: values.len() as u64,
                siblings,
            });
            steps.extend(child.steps);

            Ok(ProvenSelection {
                selected_value: child.selected_value,
                selected_bytes: child.selected_bytes,
                root_hash,
                steps,
            })
        }
        (
            SelectorDescent::Range { start, end },
            SchemaNode::List { .. },
            TreeValue::List(values) | TreeValue::ListHandle(values),
        ) => {
            if segments.len() > 1 {
                return Err(Error::Other(
                    "Range selector segment must be the final segment".into(),
                ));
            }
            let start_idx = start as usize;
            let end_idx = end as usize;
            if start_idx >= end_idx || end_idx > values.len() {
                return Err(Error::Other(format!(
                    "Selector range '{}..{}' is out of bounds for list of length {}",
                    start,
                    end,
                    values.len()
                )));
            }

            let sub_list = TreeValue::List(values[start_idx..end_idx].to_vec());
            let (selected_bytes, _) = subtree_payload_and_root(&sub_list)?;

            let mut hashes = Vec::with_capacity(values.len());
            for item in values {
                hashes.push(subtree_payload_and_root(item)?.1);
            }
            let (root_hash, siblings) = list_root_and_range_proof(&hashes, start_idx, end_idx)?;

            Ok(ProvenSelection {
                selected_value: sub_list,
                selected_bytes,
                root_hash,
                steps: vec![SelectionProofStep::ListRange {
                    start,
                    len: values.len() as u64,
                    siblings,
                }],
            })
        }
        (SelectorDescent::Field(field_name), _, _) => Err(Error::Other(format!(
            "Selector field '{}' was not found in selected value",
            field_name
        ))),
        (SelectorDescent::Index(index), _, _) => Err(Error::Other(format!(
            "Selector index '{}' was not found in selected value",
            index
        ))),
        (SelectorDescent::Range { start, end }, _, _) => Err(Error::Other(format!(
            "Selector range '{}..{}' requires a list value",
            start, end
        ))),
    }
}

pub(crate) fn selected_payload_from_proven(
    selector: &SelectorPath,
    proven: ProvenSelection,
) -> SelectedPayload {
    let selected_hash = selection_payload_hash(&proven.selected_bytes);
    let selected_len = proven.selected_bytes.len() as u64;
    SelectedPayload {
        bytes: proven.selected_bytes,
        commitment: SelectionCommitment {
            path: selector.clone(),
            source_root_hash: proven.root_hash,
            selected_hash,
            selected_len,
            payload_kind: SelectionPayloadKind::Raw,
        },
    }
}

pub(crate) fn selected_payload_from_raster_location(
    data: &impl RasterData,
    selector: &SelectorPath,
    selection: RasterSelectionLocation,
) -> CoreResult<SelectedPayload> {
    let bytes = raster_selection_payload(
        data,
        selection.offset,
        selection.len,
        selection.range,
    )?;
    let selected_hash = selection_payload_hash(&bytes);
    let selected_len = bytes.len() as u64;
    Ok(SelectedPayload {
        bytes,
        commitment: SelectionCommitment {
            path: selector.clone(),
            source_root_hash: selection.root_hash,
            selected_hash,
            selected_len,
            payload_kind: SelectionPayloadKind::Raw,
        },
    })
}

/// A `0x0A` list metadata selection: the authenticated `(len, elements_root)`
/// of the list at `selector`, anchored to the same source root a whole-list
/// selection of the same path anchors to.
///
/// The payload is 41 bytes, or 9 for an empty list, against the entire list
/// today — which is the whole of `lazy-list-recur.md` §2.
pub(crate) fn list_metadata_payload(
    selector: &SelectorPath,
    source_root_hash: Hash32,
    len: u64,
    elements_root: Option<Hash32>,
) -> AuthenticatedListMetadata {
    let bytes = encode_list_metadata_payload(len, elements_root);
    let selected_hash = selection_payload_hash(&bytes);
    let selected_len = bytes.len() as u64;
    AuthenticatedListMetadata {
        len,
        elements_root,
        selected: SelectedPayload {
            bytes,
            commitment: SelectionCommitment {
                path: selector.clone(),
                source_root_hash,
                selected_hash,
                selected_len,
                payload_kind: SelectionPayloadKind::List,
            },
        },
    }
}

/// The witness form of [`list_metadata_payload`].
///
/// The proof steps are the list's own, unchanged: metadata is a different
/// *view* of one node, not a step below it, so the descent that reaches the
/// list is the descent that reaches its metadata. Only the payload differs,
/// and `parse_subtree_root`'s `0x0A` arm recomputes the same root the `0x02`
/// form folds to.
pub(crate) fn list_metadata_witness(
    selector: &SelectorPath,
    selection: RasterSelection,
    len: u64,
    elements_root: Option<Hash32>,
) -> SelectionWitness {
    SelectionWitness::from_payload(
        encode_list_metadata_payload(len, elements_root),
        SelectionProof {
            path: selector.clone(),
            root_hash: selection.root_hash,
            steps: selection.steps,
        },
    )
}

pub(crate) fn selection_witness_from_raster_selection(
    data: &impl RasterData,
    selector: &SelectorPath,
    selection: RasterSelection,
) -> CoreResult<SelectionWitness> {
    Ok(SelectionWitness::from_payload(
        raster_selection_payload(data, selection.offset, selection.len, selection.range)?,
        SelectionProof {
            path: selector.clone(),
            root_hash: selection.root_hash,
            steps: selection.steps,
        },
    ))
}

fn typed_proven_selection<Root: Serialize + Selectable>(
    value: &Root,
    selector: &SelectorPath,
) -> CoreResult<ProvenSelection> {
    let root_tree = tree_value_from_serialize(value)?;
    prove_selection(&Root::schema(), &root_tree, &selector.segments)
}

fn extend_selector_path(prefix: &SelectorPath, suffix: &SelectorPath) -> SelectorPath {
    let mut segments = prefix.segments.clone();
    segments.extend(suffix.segments.clone());
    SelectorPath::new(segments)
}

pub fn select_storage_value<Root, T>(
    value: &StorageValue<Root>,
    selector: &SelectorPath,
) -> CoreResult<StorageValue<T>>
where
    Root: DeserializeOwned + Serialize + Selectable,
    T: DeserializeOwned + Serialize,
{
    let proven = typed_proven_selection(&value.value, selector)?;
    let typed_selected = typed_value_from_tree::<T>(&proven.selected_value).map_err(|e| {
        Error::Serialization(format!(
            "Failed to deserialize selected storage input from selection tree: {}",
            e
        ))
    })?;
    let full_selector = extend_selector_path(&value.selector, selector);
    let selected_hash = selection_payload_hash(&proven.selected_bytes);
    let selected_len = proven.selected_bytes.len() as u64;
    Ok(StorageValue::new_with_selection(
        value.reference.clone(),
        proven.selected_bytes,
        full_selector.clone(),
        SelectionCommitment {
            path: full_selector,
            source_root_hash: value.selection.source_root_hash.clone(),
            selected_hash,
            selected_len,
            payload_kind: SelectionPayloadKind::Raw,
        },
        typed_selected,
    ))
}

fn infer_leaf_type_name(value: &TreeValue) -> CoreResult<String> {
    match value {
        TreeValue::Bool(_) => Ok("bool".into()),
        TreeValue::U8(_) => Ok("u8".into()),
        TreeValue::U16(_) => Ok("u16".into()),
        TreeValue::U32(_) => Ok("u32".into()),
        TreeValue::U64(_) => Ok("u64".into()),
        TreeValue::I8(_) => Ok("i8".into()),
        TreeValue::I16(_) => Ok("i16".into()),
        TreeValue::I32(_) => Ok("i32".into()),
        TreeValue::I64(_) => Ok("i64".into()),
        TreeValue::String(_) => Ok("String".into()),
        TreeValue::BytesPage { .. } => Ok("BytesPage".into()),
        _ => Err(Error::Serialization(
            "Expected leaf value while building raster index".into(),
        )),
    }
}

pub(crate) fn hex_string(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

fn merkle_levels_from_hashes(hashes: &[Hash32]) -> Vec<crate::raster_index::RasterMerkleLevel> {
    use crate::raster_index::RasterMerkleLevel;

    if hashes.is_empty() {
        return Vec::new();
    }

    let mut levels = vec![RasterMerkleLevel {
        hashes: hashes.to_vec(),
    }];
    let mut level = hashes.to_vec();
    while level.len() > 1 {
        let mut padded = level.clone();
        if padded.len() % 2 == 1 {
            padded.push(padded.last().cloned().unwrap());
        }
        let mut next = Vec::with_capacity(padded.len() / 2);
        for pair in padded.chunks(2) {
            next.push(selection_hash(&[
                b"list-node",
                pair[0].as_slice(),
                pair[1].as_slice(),
            ]));
        }
        levels.push(RasterMerkleLevel {
            hashes: next.clone(),
        });
        level = next;
    }

    levels
}

/// A child to be turned into a raster node, with its precomputed byte offset
/// inside the parent's payload.
#[derive(Clone, Copy)]
#[cfg(test)]
struct RasterChildPlan<'a> {
    value: &'a TreeValue,
    node_offset: u64,
}

/// In-progress node on the explicit build stack (replaces a recursive frame).
#[cfg(test)]
struct RasterFrame<'a> {
    value: &'a TreeValue,
    node_id: u64,
    /// This node's data-file position; its children record theirs relative to
    /// it (`rindex04`).
    node_position: u64,
    root_hash: Hash32,
    children: RasterChildren<'a>,
    next: usize,
    /// Node ids of children, accumulated in `children` order as they complete.
    child_ids: Vec<u64>,
}

#[cfg(test)]
struct RasterChildren<'a> {
    plans: Vec<RasterChildPlan<'a>>,
    /// Child root hashes in element order (only consumed by `List` nodes).
    hashes: Vec<Hash32>,
}

/// Compute the ordered children of `value` together with the byte offset each
/// child node occupies inside `value`'s payload. The offset arithmetic and the
/// `Map` ordering match the previous recursive implementation exactly.
#[cfg(test)]
fn prepare_raster_children<'a>(
    value: &'a TreeValue,
    offset: u64,
) -> CoreResult<RasterChildren<'a>> {
    let mut plans = Vec::new();
    let mut hashes = Vec::new();
    match value {
        TreeValue::Struct(fields) => {
            // Each field is laid out by `assemble_subtree` as
            // `(u64 name_len)(name)(u64 payload_len)(payload)`, so a child's
            // own payload starts past both length prefixes and the name.
            let mut child_offset = offset + 1 + 8;
            for (name, child) in fields {
                let (child_payload, child_hash) = subtree_payload_and_root(child)?;
                let name_len = name.len() as u64;
                plans.push(RasterChildPlan {
                    value: child,
                    node_offset: child_offset + 8 + name_len + 8,
                });
                hashes.push(child_hash);
                child_offset += 8 + name_len + 8 + child_payload.len() as u64;
            }
        }
        TreeValue::List(values) => {
            let mut child_offset = offset + 1 + 8;
            for child in values {
                let (child_payload, child_hash) = subtree_payload_and_root(child)?;
                plans.push(RasterChildPlan {
                    value: child,
                    node_offset: child_offset + 8,
                });
                hashes.push(child_hash);
                child_offset += 8 + child_payload.len() as u64;
            }
        }
        TreeValue::ListHandle(values) => {
            // The index node points at the inline `0x02` list (see
            // `enter_raster_frame`), which starts past the handle header. Element
            // offsets are therefore measured from that inner list, exactly as for
            // a plain `List`.
            let mut child_offset = offset + LIST_HANDLE_HEADER_LEN + 1 + 8;
            for child in values {
                let (child_payload, child_hash) = subtree_payload_and_root(child)?;
                plans.push(RasterChildPlan {
                    value: child,
                    node_offset: child_offset + 8,
                });
                hashes.push(child_hash);
                child_offset += 8 + child_payload.len() as u64;
            }
        }
        TreeValue::Map(entries) => {
            let mut records = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                let (key_payload, _) = subtree_payload_and_root(key)?;
                let (value_payload, _) = subtree_payload_and_root(value)?;
                records.push((key, value, key_payload, value_payload));
            }
            records.sort_by(|left, right| left.2.cmp(&right.2).then_with(|| left.3.cmp(&right.3)));

            let mut child_offset = offset + 1 + 8;
            for (key, value, key_payload, value_payload) in &records {
                plans.push(RasterChildPlan {
                    value: *key,
                    node_offset: child_offset + 8,
                });
                child_offset += 8 + key_payload.len() as u64;
                plans.push(RasterChildPlan {
                    value: *value,
                    node_offset: child_offset + 8,
                });
                child_offset += 8 + value_payload.len() as u64;
            }
        }
        TreeValue::EnumNewtype(variant, child) => {
            let child_offset = offset + 1 + 8 + variant.len() as u64 + 8;
            plans.push(RasterChildPlan {
                value: child.as_ref(),
                node_offset: child_offset,
            });
        }
        TreeValue::EnumTuple(variant, values) => {
            let mut child_offset = offset + 1 + 8 + variant.len() as u64 + 8;
            for child in values {
                let (child_payload, child_hash) = subtree_payload_and_root(child)?;
                plans.push(RasterChildPlan {
                    value: child,
                    node_offset: child_offset + 8,
                });
                hashes.push(child_hash);
                child_offset += 8 + child_payload.len() as u64;
            }
        }
        TreeValue::EnumStruct(variant, fields) => {
            let mut child_offset = offset + 1 + 8 + variant.len() as u64 + 8;
            for (_, child) in fields {
                let (child_payload, child_hash) = subtree_payload_and_root(child)?;
                plans.push(RasterChildPlan {
                    value: child,
                    node_offset: child_offset + 8,
                });
                hashes.push(child_hash);
                child_offset += 8 + child_payload.len() as u64;
            }
        }
        _ => {}
    }
    Ok(RasterChildren { plans, hashes })
}

/// Build a node's `RasterNodeKind` from its completed children. `child_ids` is
/// in [`prepare_raster_children`] order; for `Map` that is the sorted
/// [key0, value0, key1, value1, ...] sequence.
pub(crate) fn finalize_raster_kind(
    value: &TreeValue,
    child_ids: &[u64],
    child_hashes: &[Hash32],
) -> CoreResult<crate::raster_index::RasterNodeKind> {
    use crate::raster_index::{RasterMapEntry, RasterNodeKind, RasterStructField};

    let kind = match value {
        TreeValue::Unit => RasterNodeKind::Unit,
        TreeValue::Struct(fields) => RasterNodeKind::Struct {
            fields: fields
                .iter()
                .zip(child_ids)
                .map(|((name, _), &child)| RasterStructField {
                    name: name.clone(),
                    child,
                })
                .collect(),
        },
        // A `ListHandle` indexes identically to a `List`: same element children,
        // same Merkle levels, same selection behaviour. The handle wrapper is a
        // parent-payload detail, invisible to the index kind.
        TreeValue::List(values) | TreeValue::ListHandle(values) => RasterNodeKind::List {
            len: values.len() as u64,
            elements: child_ids.to_vec(),
            merkle_levels: merkle_levels_from_hashes(child_hashes),
        },
        TreeValue::Map(_) => RasterNodeKind::Map {
            entries: child_ids
                .chunks(2)
                .map(|pair| RasterMapEntry {
                    key: pair[0],
                    value: pair[1],
                })
                .collect(),
        },
        TreeValue::EnumUnit(variant) => RasterNodeKind::EnumUnit {
            variant: variant.clone(),
        },
        TreeValue::EnumNewtype(variant, _) => RasterNodeKind::EnumNewtype {
            variant: variant.clone(),
            child: child_ids[0],
        },
        TreeValue::EnumTuple(variant, _) => RasterNodeKind::EnumTuple {
            variant: variant.clone(),
            elements: child_ids.to_vec(),
        },
        TreeValue::EnumStruct(variant, fields) => RasterNodeKind::EnumStruct {
            variant: variant.clone(),
            fields: fields
                .iter()
                .zip(child_ids)
                .map(|((name, _), &child)| RasterStructField {
                    name: name.clone(),
                    child,
                })
                .collect(),
        },
        leaf => RasterNodeKind::Leaf {
            type_name: infer_leaf_type_name(leaf)?,
        },
    };
    Ok(kind)
}

/// Reserve a node slot for `value` (pre-order id assignment) and prepare its
/// children for the build stack.
#[cfg(test)]
fn enter_raster_frame<'a>(
    nodes: &mut Vec<crate::raster_index::RasterNode>,
    value: &'a TreeValue,
    offset: u64,
    parent_position: u64,
) -> CoreResult<RasterFrame<'a>> {
    use crate::raster_index::{RasterNode, RasterNodeKind};

    let (payload, root_hash) = subtree_payload_and_root(value)?;
    // A list handle's payload is `header(49) + inline 0x02 list`. The index node
    // stands for the inline list itself, so it points past the header and its
    // length is the inline region only. Every downstream selection then reads a
    // plain list, and the handle wrapper exists solely inside the parent struct's
    // bytes (where it makes the parent's structural root O(1)).
    let (node_offset, node_len) = match value {
        TreeValue::ListHandle(_) => (
            offset + LIST_HANDLE_HEADER_LEN,
            payload.len() as u64 - LIST_HANDLE_HEADER_LEN,
        ),
        _ => (offset, payload.len() as u64),
    };
    let node_id = nodes.len() as u64;
    nodes.push(RasterNode {
        offset: node_offset - parent_position,
        len: node_len,
        root_hash,
        kind: RasterNodeKind::Unit,
    });
    let children = prepare_raster_children(value, offset)?;
    Ok(RasterFrame {
        value,
        node_id,
        node_position: node_offset,
        root_hash,
        children,
        next: 0,
        child_ids: Vec::new(),
    })
}

#[cfg(test)]
fn build_raster_index_node(
    nodes: &mut Vec<crate::raster_index::RasterNode>,
    root_value: &TreeValue,
    root_offset: u64,
) -> CoreResult<(u64, Hash32)> {
    // Iterative pre-order build with an explicit heap stack. Node ids are still
    // assigned in pre-order (parent before its children, children left to
    // right), so the on-disk layout is unchanged; only the call stack is gone.
    let root_frame = enter_raster_frame(nodes, root_value, root_offset, 0)?;
    let root_id = root_frame.node_id;
    let root_hash = root_frame.root_hash;
    let mut stack: Vec<RasterFrame> = vec![root_frame];
    // Node id of the child that just finished, to be recorded by its parent.
    let mut completed_child: Option<u64> = None;

    while !stack.is_empty() {
        let next_child = {
            let frame = stack.last_mut().unwrap();
            if let Some(id) = completed_child.take() {
                frame.child_ids.push(id);
            }
            if frame.next < frame.children.plans.len() {
                let plan = frame.children.plans[frame.next];
                frame.next += 1;
                Some((plan, frame.node_position))
            } else {
                None
            }
        };

        match next_child {
            Some((plan, parent_position)) => {
                let child_frame =
                    enter_raster_frame(nodes, plan.value, plan.node_offset, parent_position)?;
                stack.push(child_frame);
            }
            None => {
                let frame = stack.pop().unwrap();
                let kind =
                    finalize_raster_kind(frame.value, &frame.child_ids, &frame.children.hashes)?;
                nodes[frame.node_id as usize].kind = kind;
                completed_child = Some(frame.node_id);
            }
        }
    }

    Ok((root_id, root_hash))
}

pub fn encode_raster_value<T: Serialize>(value: &T) -> CoreResult<(Vec<u8>, Vec<u8>, String)> {
    let tree = tree_value_from_serialize(value)?;
    let mut nodes = Vec::new();
    let encoded = crate::raster_encode::encode_indexed(&tree, &mut nodes)?;
    let index = RasterIndex::new(encoded.node, encoded.root, nodes);
    Ok((encoded.payload, index.encode()?, hex_string(&encoded.root)))
}

/// The builder [`crate::raster_encode::encode_indexed`] replaced, kept as the
/// oracle its tests compare against: every selection must agree.
#[cfg(test)]
pub(crate) fn encode_raster_value_reference<T: Serialize>(
    value: &T,
) -> CoreResult<(Vec<u8>, RasterIndex)> {
    let tree = tree_value_from_serialize(value)?;
    let (payload, root_hash) = subtree_payload_and_root(&tree)?;
    let mut nodes = Vec::new();
    let root_node = build_raster_index_node(&mut nodes, &tree, 0)?.0;
    Ok((payload, RasterIndex::new(root_node, root_hash, nodes)))
}

pub fn write_raster_files<T: Serialize>(
    value: &T,
    data_path: &Path,
    index_path: &Path,
) -> CoreResult<String> {
    let (data_bytes, index_bytes, commitment) = encode_raster_value(value)?;
    fs::write(data_path, data_bytes).map_err(|e| {
        Error::Other(format!(
            "Failed to write raster data file '{}': {}",
            data_path.display(),
            e
        ))
    })?;
    fs::write(index_path, index_bytes).map_err(|e| {
        Error::Other(format!(
            "Failed to write raster index file '{}': {}",
            index_path.display(),
            e
        ))
    })?;
    Ok(commitment)
}

/// Where a program's output artifact was written.
pub struct OutputArtifact {
    pub data_path: std::path::PathBuf,
    pub index_path: std::path::PathBuf,
    pub manifest_path: std::path::PathBuf,
    pub commitment: String,
}

/// Export `main`'s returned value as an output artifact, in the exact format
/// external input data takes when a `ProgramStart` loads it: a raster-encoded
/// `output.bin` + `output.rindex`, plus an `output_manifest.json` whose single
/// entry mirrors an input-manifest entry (`type`/`encoding`/`commitment`). The
/// artifact can therefore be handed to a following program as its
/// `--input`/`--input-manifest`.
///
/// Writes only when `RASTER_OUTPUT_DIR` is set (by `cargo raster run`); a plain
/// `cargo run` produces no files and returns `Ok(None)`.
pub fn write_program_output_artifact<T: Serialize>(
    value: &T,
) -> CoreResult<Option<OutputArtifact>> {
    let Some(dir) = std::env::var_os(crate::tracing::OUTPUT_DIR_ENV) else {
        return Ok(None);
    };
    let dir = std::path::PathBuf::from(dir);
    fs::create_dir_all(&dir).map_err(|e| {
        Error::Other(format!(
            "Failed to create output artifact directory '{}': {}",
            dir.display(),
            e
        ))
    })?;

    let data_path = dir.join("output.bin");
    let index_path = dir.join("output.rindex");
    let manifest_path = dir.join("output_manifest.json");

    let commitment = write_raster_files(value, &data_path, &index_path)?;

    // Byte-for-byte the input-manifest entry shape (see input_manifest.json),
    // so the artifact round-trips as a following program's external input.
    let manifest = format!(
        "{{\n  \"output\": {{ \"type\": \"sha256\", \"encoding\": \"raster\", \"commitment\": \"{}\" }}\n}}\n",
        commitment
    );
    fs::write(&manifest_path, manifest).map_err(|e| {
        Error::Other(format!(
            "Failed to write output manifest '{}': {}",
            manifest_path.display(),
            e
        ))
    })?;

    Ok(Some(OutputArtifact {
        data_path,
        index_path,
        manifest_path,
        commitment,
    }))
}

/// Compute the manifest commitment for a Postcard-encoded entry argument:
/// the same selection-tree structural root `start_program`/
/// `verify_postcard_structural_commitment` check against at runtime, hex-
/// encoded. Manifest-authoring tooling (e.g. a project's `gen_input`
/// binary) calls this to produce `input_manifest.json`'s `commitment`
/// field for a Postcard source — it is *not* `sha256(postcard bytes)`.
pub fn postcard_structural_commitment<T: Serialize>(value: &T) -> CoreResult<String> {
    let tree = tree_value_from_serialize(value)?;
    let (_, root_hash) = subtree_payload_and_root(&tree)?;
    Ok(hex_string(&root_hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster_index::RasterIndex;
    use crate::source::{sha256_hex, FileInputSourceResolver};
    use raster_core::input::{verify_selection_proof, SchemaField, SchemaNode, Selectable};
    use raster_core::List;
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::vec;

    static UNIQUE_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
    struct Address {
        lines: List<String>,
        indexes: List<u32>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
    struct PersonalData {
        age: usize,
        name: String,
        addresses: List<Address>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
    enum Pattern {
        Empty,
        String(String),
        Sequence { len: u32 },
        Pair(u8, u8),
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
    struct ComplexSerdeValue {
        maybe_name: Option<String>,
        pattern: Pattern,
        aliases: BTreeMap<String, u32>,
        nested: Option<Pattern>,
    }

    impl Selectable for Address {
        fn schema() -> SchemaNode {
            SchemaNode::Struct {
                type_name: "Address".into(),
                fields: vec![
                    SchemaField::new("lines", "lines", <List<String> as Selectable>::schema()),
                    SchemaField::new("indexes", "indexes", <List<u32> as Selectable>::schema()),
                ],
            }
        }
    }

    impl Selectable for PersonalData {
        fn schema() -> SchemaNode {
            SchemaNode::Struct {
                type_name: "PersonalData".into(),
                fields: vec![
                    SchemaField::new("age", "age", <usize as Selectable>::schema()),
                    SchemaField::new("name", "name", <String as Selectable>::schema()),
                    SchemaField::new(
                        "addresses",
                        "addresses",
                        <List<Address> as Selectable>::schema(),
                    ),
                ],
            }
        }
    }

    fn unique_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let counter = UNIQUE_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("raster-input-test-{}-{}", nanos, counter))
    }

    fn storage_manager(input_path: &Path, manifest_path: &Path) -> FileInputSourceResolver {
        FileInputSourceResolver::from_input_args(input_path.to_str(), manifest_path.to_str())
            .unwrap()
    }

    fn write_external_documents(
        dir: &Path,
        hash: &str,
        input_body: &str,
        manifest_body: &str,
    ) -> (PathBuf, PathBuf) {
        let input_path = dir.join("input.json");
        fs::write(&input_path, input_body).unwrap();

        let manifest_path = dir.join("input_manifest.json");
        fs::write(&manifest_path, manifest_body.replace("{hash}", hash)).unwrap();

        (input_path, manifest_path)
    }

    #[test]
    fn resolves_typed_nested_selection_with_merkle_proof() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let data = PersonalData {
            age: 25,
            name: "John".to_string(),
            addresses: vec![Address {
                lines: vec!["221B Baker Street".to_string(), "Flat B".to_string()].into(),
                indexes: vec![7, 42].into(),
            }]
            .into(),
        };
        let bytes = raster_core::postcard::to_allocvec(&data).unwrap();
        fs::write(dir.join("personal_data.bin"), &bytes).unwrap();
        let hash = sha256_hex(&bytes);
        let (input_path, manifest_path) = write_external_documents(
            &dir,
            &hash,
            r#"{"personal_data_bin":{"path":"personal_data.bin","load_preference":"mmap"}}"#,
            r#"{"personal_data_bin":{"type":"sha256","commitment":"{hash}"}}"#,
        );

        let storage = storage_manager(&input_path, &manifest_path);
        let resolved = storage.resolve("personal_data_bin").unwrap();
        let root: PersonalData = raster_core::postcard::from_bytes(resolved.bytes()).unwrap();
        let selector = SelectorPath::new(vec![
            SelectorSegment::from("addresses"),
            SelectorSegment::from(0usize),
            SelectorSegment::from("lines"),
            SelectorSegment::from(1usize),
        ]);
        let proven = typed_proven_selection(&root, &selector).unwrap();

        let witness = SelectionWitness {
            bytes: proven.selected_bytes.clone(),
            proof: SelectionProof {
                path: selector,
                root_hash: proven.root_hash,
                steps: proven.steps.clone(),
            },
            selected_root: None,
        };

        assert_eq!(
            typed_value_from_tree::<String>(&proven.selected_value).unwrap(),
            "Flat B"
        );
        assert!(verify_selection_proof(&witness.bytes, &witness.proof));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn range_typed_selection_produces_verifiable_slice_payload() {
        let root = Address {
            lines: vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
                "e".to_string(),
            ]
            .into(),
            indexes: vec![1, 2, 3, 4, 5].into(),
        };
        let selector = SelectorPath::new(vec![
            SelectorSegment::from("lines"),
            SelectorSegment::Range { start: 1, end: 4 },
        ]);
        let proven = typed_proven_selection(&root, &selector).unwrap();

        assert_eq!(
            typed_value_from_tree::<Vec<String>>(&proven.selected_value).unwrap(),
            vec!["b".to_string(), "c".to_string(), "d".to_string()]
        );

        let witness = SelectionWitness {
            bytes: proven.selected_bytes.clone(),
            proof: SelectionProof {
                path: selector,
                root_hash: proven.root_hash,
                steps: proven.steps,
            },
            selected_root: None,
        };
        assert!(verify_selection_proof(&witness.bytes, &witness.proof));

        // The slice proof anchors to the same source root as a whole-value
        // selection of the same object.
        let whole = typed_proven_selection(&root, &SelectorPath::default()).unwrap();
        assert_eq!(whole.root_hash, witness.proof.root_hash);
    }

    /// The verifier re-derives an index binding's committed bytes rather than
    /// decoding them (`encode_index_leaf_payload`), so its encoding must agree
    /// with the production selection encoder byte-for-byte. A divergence would
    /// be a silent verification failure — every honest bound index would be
    /// rejected, or worse, a dishonest one accepted — so pin the two together
    /// against the real encoder for every supported width.
    #[test]
    fn bound_index_payload_matches_encoded_leaf() {
        use raster_core::input::{encode_index_leaf_payload, IndexWidth};

        #[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
        struct Widths {
            a: List<u8>,
            b: List<u16>,
            c: List<u32>,
            d: List<u64>,
        }

        impl Selectable for Widths {
            fn schema() -> SchemaNode {
                SchemaNode::Struct {
                    type_name: "Widths".into(),
                    fields: vec![
                        SchemaField::new("a", "a", <List<u8> as Selectable>::schema()),
                        SchemaField::new("b", "b", <List<u16> as Selectable>::schema()),
                        SchemaField::new("c", "c", <List<u32> as Selectable>::schema()),
                        SchemaField::new("d", "d", <List<u64> as Selectable>::schema()),
                    ],
                }
            }
        }

        let root = Widths {
            a: vec![0u8, 7, 255].into(),
            b: vec![0u16, 300, 65_535].into(),
            c: vec![0u32, 262_143, u32::MAX].into(),
            d: vec![0u64, 1 << 40, u64::MAX].into(),
        };

        let cases: Vec<(&str, IndexWidth, Vec<u64>)> = vec![
            ("a", IndexWidth::U8, vec![0, 7, 255]),
            ("b", IndexWidth::U16, vec![0, 300, 65_535]),
            ("c", IndexWidth::U32, vec![0, 262_143, u64::from(u32::MAX)]),
            ("d", IndexWidth::U64, vec![0, 1 << 40, u64::MAX]),
        ];

        for (field, width, values) in cases {
            for (position, value) in values.into_iter().enumerate() {
                let selector = SelectorPath::new(vec![
                    SelectorSegment::from(field),
                    SelectorSegment::from(position),
                ]);
                let proven = typed_proven_selection(&root, &selector).unwrap();
                assert_eq!(
                    proven.selected_bytes,
                    encode_index_leaf_payload(value, width).unwrap(),
                    "encode_index_leaf_payload disagrees with the selection encoder \
                     for {value} at {width:?}",
                );
            }
        }
    }

    /// A `BoundIndex` segment must select and prove exactly what the equivalent
    /// literal `Index` does — same element, same proof, same root. This is the
    /// `SelectorDescent` contract: everything below the segment is
    /// provenance-blind.
    #[test]
    fn bound_index_selection_proves_like_a_literal_index() {
        let root = Address {
            lines: vec!["a".to_string(), "b".to_string(), "c".to_string()].into(),
            indexes: vec![7, 8, 9].into(),
        };

        let literal = SelectorPath::new(vec![
            SelectorSegment::from("lines"),
            SelectorSegment::Index(1),
        ]);
        let bound = SelectorPath::new(vec![
            SelectorSegment::from("lines"),
            SelectorSegment::BoundIndex {
                index: 1,
                source: "@idx/deadbeef".to_string(),
                width: raster_core::input::IndexWidth::U32,
            },
        ]);

        let via_literal = typed_proven_selection(&root, &literal).unwrap();
        let via_bound = typed_proven_selection(&root, &bound).unwrap();

        assert_eq!(via_literal.selected_bytes, via_bound.selected_bytes);
        assert_eq!(via_literal.root_hash, via_bound.root_hash);
        assert_eq!(via_literal.steps, via_bound.steps);
        assert_eq!(
            typed_value_from_tree::<String>(&via_bound.selected_value).unwrap(),
            "b"
        );

        // And the proof verifies against the bound path it claims.
        let witness = SelectionWitness {
            bytes: via_bound.selected_bytes,
            proof: SelectionProof {
                path: bound,
                root_hash: via_bound.root_hash,
                steps: via_bound.steps,
            },
            selected_root: None,
        };
        assert!(verify_selection_proof(&witness.bytes, &witness.proof));
    }

    /// A proof of one element must not verify against a `BoundIndex` claiming a
    /// different one — the `step_proves_segment` pinning, which is what stops a
    /// prover swapping the element while keeping the path.
    #[test]
    fn bound_index_rejects_proof_of_a_different_element() {
        let root = Address {
            lines: vec!["a".to_string(), "b".to_string(), "c".to_string()].into(),
            indexes: List::default(),
        };

        let honest = SelectorPath::new(vec![
            SelectorSegment::from("lines"),
            SelectorSegment::Index(1),
        ]);
        let proven = typed_proven_selection(&root, &honest).unwrap();

        let lying = SelectorPath::new(vec![
            SelectorSegment::from("lines"),
            SelectorSegment::BoundIndex {
                index: 2,
                source: "@idx/deadbeef".to_string(),
                width: raster_core::input::IndexWidth::U32,
            },
        ]);
        let witness = SelectionWitness {
            bytes: proven.selected_bytes,
            proof: SelectionProof {
                path: lying,
                root_hash: proven.root_hash,
                steps: proven.steps,
            },
            selected_root: None,
        };
        assert!(!verify_selection_proof(&witness.bytes, &witness.proof));
    }

    #[test]
    fn range_selection_rejects_out_of_bounds_and_non_terminal_segments() {
        let root = Address {
            lines: vec!["a".to_string(), "b".to_string()].into(),
            indexes: List::default(),
        };

        let out_of_bounds = SelectorPath::new(vec![
            SelectorSegment::from("lines"),
            SelectorSegment::Range { start: 1, end: 3 },
        ]);
        assert!(typed_proven_selection(&root, &out_of_bounds).is_err());

        let non_terminal = SelectorPath::new(vec![
            SelectorSegment::from("lines"),
            SelectorSegment::Range { start: 0, end: 1 },
            SelectorSegment::from(0usize),
        ]);
        assert!(typed_proven_selection(&root, &non_terminal).is_err());
    }

    #[test]
    fn whole_value_typed_selection_produces_verifiable_payload() {
        let root = PersonalData {
            age: 25,
            name: "John".to_string(),
            addresses: vec![Address {
                lines: vec!["221B Baker Street".to_string()].into(),
                indexes: vec![7].into(),
            }]
            .into(),
        };

        let selected = selected_payload_from_proven(
            &SelectorPath::default(),
            typed_proven_selection(&root, &SelectorPath::default()).unwrap(),
        );

        assert!(selected.commitment.path.is_empty());
    }

    #[test]
    fn payload_structural_root_recomputes_manifest_commitment() {
        // The chain "bridge": a program's `output.bin` is exactly this payload,
        // and `payload_structural_root` over those bytes alone must equal the
        // structural root the manifest commits (`encode_raster_value`'s hex
        // commitment) — the manifest-side link hash, recomputable with no index.
        // See docs/proposals/program-chain.md.
        let mut aliases = BTreeMap::new();
        aliases.insert("one".to_string(), 1);
        aliases.insert("two".to_string(), 2);
        let value = ComplexSerdeValue {
            maybe_name: Some("chain".to_string()),
            pattern: Pattern::Pair(3, 9),
            aliases,
            nested: Some(Pattern::Sequence { len: 4 }),
        };

        let (payload, _index, commitment_hex) = encode_raster_value(&value).unwrap();
        let root =
            raster_core::input::payload_structural_root(&payload).expect("payload is well-formed");
        assert_eq!(super::hex_string(&root), commitment_hex);

        // A truncated artifact must not silently produce a root.
        assert!(
            raster_core::input::payload_structural_root(&payload[..payload.len() - 1]).is_none()
        );
    }

    #[test]
    fn raster_round_trip_supports_option_enum_and_map_values() {
        let mut aliases = BTreeMap::new();
        aliases.insert("one".to_string(), 1);
        aliases.insert("two".to_string(), 2);
        let value = ComplexSerdeValue {
            maybe_name: None,
            pattern: Pattern::Sequence { len: 7 },
            aliases,
            nested: Some(Pattern::String("merged".to_string())),
        };

        let (data_bytes, index_bytes, _commitment) = encode_raster_value(&value).unwrap();
        let index = RasterIndex::from_bytes(&index_bytes).unwrap();
        let selection = index.root_selection().unwrap();
        let tree =
            tree_value_from_raster_node(&index, &data_bytes, selection.node_id, selection.offset)
                .unwrap();
        let decoded: ComplexSerdeValue = typed_value_from_tree(&tree).unwrap();
        let selected_hash = raster_core::input::selection_payload_hash(&data_bytes);
        let selected_len = data_bytes.len() as u64;
        let selected = SelectedPayload::new(
            data_bytes,
            SelectionCommitment {
                path: SelectorPath::default(),
                source_root_hash: selection.root_hash,
                selected_hash,
                selected_len,
                payload_kind: SelectionPayloadKind::Raw,
            },
        );
        let witness = SelectionWitness {
            bytes: selected.bytes.clone(),
            proof: SelectionProof {
                path: SelectorPath::default(),
                root_hash: selection.root_hash,
                steps: Vec::new(),
            },
            selected_root: None,
        };

        assert_eq!(decoded, value);
        assert!(verify_selection_proof(&witness.bytes, &witness.proof));
    }

    /// A range selection served straight from the `.rindex`, the way
    /// `AuthenticatedObjectStore::selection_witness` serves one at `--commit` time.
    ///
    /// The in-memory `typed_proven_selection` path has supported ranges since
    /// `Block<T>` landed, so `select!(Block<T>, xs[a..b])` resolves in-process
    /// via the fallback at `raster/src/input.rs:1064-1071`. The index-driven
    /// path did not, which left the two halves of a range proof — the prover
    /// in `RasterIndex::select` and the verifier in `fold_list_range` — never
    /// having met. `lazy-list-recur.md` §6 needs this path for chunked recur.
    #[test]
    fn raster_index_selects_a_range_into_a_verifiable_slice_proof() {
        let value = Address {
            lines: vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
                "e".to_string(),
            ]
            .into(),
            indexes: vec![1, 2, 3, 4, 5].into(),
        };

        let (data_bytes, index_bytes, _commitment) = encode_raster_value(&value).unwrap();
        let index = RasterIndex::from_bytes(&index_bytes).unwrap();

        let selector = SelectorPath::new(vec![
            SelectorSegment::from("lines"),
            SelectorSegment::Range { start: 1, end: 4 },
        ]);
        let selection = index.select(&selector).unwrap();
        let witness =
            selection_witness_from_raster_selection(&data_bytes, &selector, selection).unwrap();

        // The index-driven producer must agree byte-for-byte with the
        // in-memory one, which has served this shape since `Block<T>` landed.
        let proven = typed_proven_selection(&value, &selector).unwrap();
        assert_eq!(witness.bytes, proven.selected_bytes);
        assert_eq!(witness.proof.steps, proven.steps);
        assert_eq!(witness.proof.root_hash, proven.root_hash);

        // And the slice folds through `ListRange` to the committed root.
        assert!(verify_selection_proof(&witness.bytes, &witness.proof));
    }

    #[test]
    fn write_raster_files_roundtrips_bytes_region() {
        let region = raster_core::Bytes::<4>::paged(vec![1, 2, 3, 4, 5]).unwrap();
        raster_core::check_bytes_geometry(&region).unwrap();
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();
        let data_path = dir.join("region.bin");
        let index_path = dir.join("region.rindex");
        let commitment = write_raster_files(&region, &data_path, &index_path).unwrap();

        let index_bytes = fs::read(&index_path).unwrap();
        assert!(index_bytes.starts_with(b"rindex04"));
        let index = RasterIndex::from_bytes(&index_bytes).unwrap();
        assert_eq!(index.root_commitment_hex(), commitment);

        let data = fs::read(&data_path).unwrap();
        let location = index.root_location().unwrap();
        let tree = tree_value_from_raster_location(&index, &data, &location).unwrap();
        match tree {
            TreeValue::Struct(fields) => {
                assert_eq!(fields.len(), 3);
                assert_eq!(fields[0].0, "byte_len");
                assert_eq!(fields[1].0, "page_size");
                assert_eq!(fields[2].0, "pages");
            }
            other => panic!("expected Bytes struct, got {other:?}"),
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rindex_grows_with_page_count_not_byte_len() {
        let small = raster_core::Bytes::<4>::paged(vec![0u8; 16]).unwrap();
        let large = raster_core::Bytes::<4>::paged(vec![0u8; 64]).unwrap();
        let doubled_page = raster_core::Bytes::<8>::paged(vec![0u8; 64]).unwrap();
        let (_, small_index, _) = encode_raster_value(&small).unwrap();
        let (_, large_index, _) = encode_raster_value(&large).unwrap();
        let (_, halved_index, _) = encode_raster_value(&doubled_page).unwrap();
        // 16 bytes / 4 = 4 pages; 64 / 4 = 16 pages — index grows with pages.
        assert!(large_index.len() > small_index.len());
        // Same 64 bytes at page size 8 is 8 pages — roughly half of 16 pages.
        assert!(halved_index.len() < large_index.len());
    }

    /// `paged-bytes.md` §10: the two bridges must agree on a page's structural
    /// root, or a finalized draft holding one could not be selected into like any
    /// other object. Both now read the page through `bytes_page_parts` instead of
    /// their own value trees, so this pins that they still land on one root.
    #[test]
    fn draft_and_direct_encoding_agree_on_a_page_root() {
        for len in [0usize, 1, 3, 4, 5, 9] {
            let payload: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let region = raster_core::Bytes::<4>::paged(payload).unwrap();
            for page in region.pages().iter() {
                let (direct_payload, direct_root) =
                    subtree_payload_and_root(&tree_value_from_serialize(page).unwrap()).unwrap();
                let draft_value =
                    raster_core::draft::draft_value_from_serialize(page).unwrap();
                let (draft_payload, draft_root) =
                    raster_core::draft::draft_value_payload_and_root(&draft_value).unwrap();
                assert_eq!(
                    direct_payload, draft_payload,
                    "payload mismatch at len={len} page={}",
                    page.index()
                );
                assert_eq!(
                    direct_root.as_slice(),
                    draft_root.as_slice(),
                    "root mismatch at len={len} page={}",
                    page.index()
                );
                assert_eq!(direct_payload.first().copied(), Some(0x0B));
            }
        }
    }

    #[test]
    fn rindex03_is_a_clean_version_error() {
        let err = RasterIndex::from_bytes(b"rindex03xxxx").unwrap_err();
        assert!(format!("{err}").contains("rindex03 is no longer supported; re-import as rindex04"));
    }

    /// The property `rindex04` exists for: offsets are parent-relative, so a
    /// list growing moves no recorded offset in a sibling declared after it —
    /// with absolute offsets every node of `b` would shift.
    #[test]
    fn growing_a_list_moves_no_recorded_offset_in_a_later_sibling() {
        #[derive(Serialize)]
        struct Two {
            a: Vec<String>,
            b: Vec<String>,
        }
        let index_of = |a: Vec<&str>| {
            let value = Two {
                a: a.into_iter().map(String::from).collect(),
                b: vec!["x".into(), "yy".into()],
            };
            let (data, index_bytes, _) = encode_raster_value(&value).unwrap();
            (data, RasterIndex::from_bytes(&index_bytes).unwrap())
        };
        let (short_data, short) = index_of(vec!["1"]);
        let (long_data, long) = index_of(vec!["1", "2", "3"]);
        for index in 0..2u64 {
            let path = SelectorPath::new(vec![
                SelectorSegment::Field("b".into()),
                SelectorSegment::Index(index),
            ]);
            let short_node = short.get_node(short.locate(&path).unwrap().node_id).unwrap();
            let long_node = long.get_node(long.locate(&path).unwrap().node_id).unwrap();
            assert_eq!(short_node.offset, long_node.offset);
            // ...while the positions computed on descent still read the right bytes.
            let short_value = tree_value_from_raster_location(&short, &short_data, &short.locate(&path).unwrap()).unwrap();
            let long_value = tree_value_from_raster_location(&long, &long_data, &long.locate(&path).unwrap()).unwrap();
            assert_eq!(short_value, long_value);
        }
    }

    #[test]
    fn rindex02_is_a_clean_version_error() {
        let err = RasterIndex::from_bytes(b"rindex02xxxx").unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("rindex02 is no longer supported"),
            "unexpected error: {msg}"
        );
    }
}
