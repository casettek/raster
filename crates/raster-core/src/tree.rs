//! The raster value tree and its one encoder.
//!
//! [`TreeValue`] is a value as raster stores it; [`subtree_payload_and_root`]
//! lays it out as the canonical payload and computes its **raster root** — the
//! commitment storage keeps for an object, a selection proof folds to, and a
//! draft's root must equal. There is exactly one implementation, here, so the
//! host (`raster-runtime`, which also builds the index) and the guests (which
//! link only this crate) cannot disagree on a root. `DraftValue` is this type.
//!
//! `no_std` + `alloc`.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use serde::ser::{
    self, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
    SerializeTupleStruct, SerializeTupleVariant,
};
use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::collections::BYTES_PAGE_NEWTYPE_NAME;
use crate::input::{bytes_page_root, encode_bytes_page_payload, struct_commitments_root, Hash32};
use crate::{Error, Result as CoreResult};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum TreeValue {
    Unit,
    Bool(bool),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    String(String),
    Struct(Vec<(String, TreeValue)>),
    List(Vec<TreeValue>),
    Map(Vec<(TreeValue, TreeValue)>),
    EnumUnit(String),
    EnumNewtype(String, Box<TreeValue>),
    EnumTuple(String, Vec<TreeValue>),
    EnumStruct(String, Vec<(String, TreeValue)>),
    BytesPage {
        index: u64,
        offset: u64,
        len: u64,
        bytes: Vec<u8>,
    },
    /// Last, so adding it left the serde encoding of every other variant — and
    /// so of every existing draft witness, where this type travels as
    /// `DraftValue` — unchanged.
    ///
    /// A `List<T>` value (as opposed to a `Block<T>`, which stays [`List`]).
    /// Encoded as a `(root, len)` handle node (`0x09`) wrapping the inline list
    /// so a parent struct's structural root skips its elements. Produced only on
    /// the serialize/encode path (keyed by [`LIST_HANDLE_NEWTYPE_NAME`]); the
    /// decode path is index-driven and reconstructs a plain [`List`].
    ListHandle(Vec<TreeValue>),
}

#[derive(Debug, Clone)]
struct TreeSerdeError(String);

impl fmt::Display for TreeSerdeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::error::Error for TreeSerdeError {}

impl ser::Error for TreeSerdeError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

type Result<T, E = TreeSerdeError> = core::result::Result<T, E>;

struct TreeValueSerializer;

struct TreeSeqSerializer {
    values: Vec<TreeValue>,
}

struct TreeStructSerializer {
    fields: Vec<(String, TreeValue)>,
}

struct TreeMapSerializer {
    entries: Vec<(TreeValue, TreeValue)>,
    next_key: Option<TreeValue>,
}

struct TreeVariantSeqSerializer {
    variant: String,
    values: Vec<TreeValue>,
}

struct TreeVariantStructSerializer {
    variant: String,
    fields: Vec<(String, TreeValue)>,
}

impl Serializer for TreeValueSerializer {
    type Ok = TreeValue;
    type Error = TreeSerdeError;
    type SerializeSeq = TreeSeqSerializer;
    type SerializeTuple = TreeSeqSerializer;
    type SerializeTupleStruct = TreeSeqSerializer;
    type SerializeStruct = TreeStructSerializer;
    type SerializeTupleVariant = TreeVariantSeqSerializer;
    type SerializeMap = TreeMapSerializer;
    type SerializeStructVariant = TreeVariantStructSerializer;

    fn serialize_bool(self, value: bool) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::Bool(value))
    }

    fn serialize_i8(self, value: i8) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::I8(value))
    }

    fn serialize_i16(self, value: i16) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::I16(value))
    }

    fn serialize_i32(self, value: i32) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::I32(value))
    }

    fn serialize_i64(self, value: i64) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::I64(value))
    }

    fn serialize_u8(self, value: u8) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::U8(value))
    }

    fn serialize_u16(self, value: u16) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::U16(value))
    }

    fn serialize_u32(self, value: u32) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::U32(value))
    }

    fn serialize_u64(self, value: u64) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::U64(value))
    }

    fn serialize_f32(self, _value: f32) -> Result<Self::Ok, Self::Error> {
        Err(TreeSerdeError(
            "f32 is not supported by selection proofs".into(),
        ))
    }

    fn serialize_f64(self, _value: f64) -> Result<Self::Ok, Self::Error> {
        Err(TreeSerdeError(
            "f64 is not supported by selection proofs".into(),
        ))
    }

    fn serialize_char(self, _value: char) -> Result<Self::Ok, Self::Error> {
        Err(TreeSerdeError(
            "char is not supported by selection proofs".into(),
        ))
    }

    fn serialize_str(self, value: &str) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::String(value.into()))
    }

    fn serialize_bytes(self, _value: &[u8]) -> Result<Self::Ok, Self::Error> {
        Err(TreeSerdeError(
            "raw bytes are not supported by selection proofs".into(),
        ))
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::Unit)
    }

    fn serialize_some<T>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::Unit)
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::Unit)
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::EnumUnit(variant.into()))
    }

    fn serialize_newtype_struct<T>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        // A page is recognized *before* its inner value is serialized. The
        // general-purpose serializer below would expand the payload into one
        // `TreeValue::U8` per byte, only for the collapse back to a flat
        // `Vec<u8>` to throw all of it away — see `bytes_page_parts` for why the
        // `List<T>` arm can serialize-then-match and this one cannot.
        if name == BYTES_PAGE_NEWTYPE_NAME {
            let parts = crate::collections::bytes_page_parts(value).map_err(TreeSerdeError)?;
            return Ok(TreeValue::BytesPage {
                index: parts.index,
                offset: parts.offset,
                len: parts.len,
                bytes: parts.bytes,
            });
        }
        let inner = value.serialize(TreeValueSerializer)?;
        // A `List<T>` announces itself through this newtype name (transparent to
        // postcard/JSON). Tag it so `assemble_subtree` stores it as a `(root,
        // len)` handle; every other newtype stays transparent, and a `Block<T>`
        // (a plain seq) stays an inline `List`. Serializing first is free here:
        // the elements had to be built anyway and `ListHandle` re-wraps the very
        // same `Vec`.
        if name == crate::collections::LIST_HANDLE_NEWTYPE_NAME {
            if let TreeValue::List(values) = inner {
                return Ok(TreeValue::ListHandle(values));
            }
        }
        Ok(inner)
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        Ok(TreeValue::EnumNewtype(
            variant.into(),
            Box::new(value.serialize(TreeValueSerializer)?),
        ))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Ok(TreeSeqSerializer {
            values: Vec::with_capacity(len.unwrap_or_default()),
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Ok(TreeVariantSeqSerializer {
            variant: variant.into(),
            values: Vec::with_capacity(len),
        })
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Ok(TreeMapSerializer {
            entries: Vec::with_capacity(len.unwrap_or_default()),
            next_key: None,
        })
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Ok(TreeStructSerializer {
            fields: Vec::with_capacity(len),
        })
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Ok(TreeVariantStructSerializer {
            variant: variant.into(),
            fields: Vec::with_capacity(len),
        })
    }
}

impl SerializeSeq for TreeSeqSerializer {
    type Ok = TreeValue;
    type Error = TreeSerdeError;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        self.values.push(value.serialize(TreeValueSerializer)?);
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::List(self.values))
    }
}

impl SerializeTuple for TreeSeqSerializer {
    type Ok = TreeValue;
    type Error = TreeSerdeError;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        SerializeSeq::end(self)
    }
}

impl SerializeTupleStruct for TreeSeqSerializer {
    type Ok = TreeValue;
    type Error = TreeSerdeError;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        SerializeSeq::end(self)
    }
}

impl SerializeStruct for TreeStructSerializer {
    type Ok = TreeValue;
    type Error = TreeSerdeError;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        self.fields
            .push((key.into(), value.serialize(TreeValueSerializer)?));
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::Struct(self.fields))
    }
}

impl SerializeMap for TreeMapSerializer {
    type Ok = TreeValue;
    type Error = TreeSerdeError;

    fn serialize_key<T>(&mut self, key: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        self.next_key = Some(key.serialize(TreeValueSerializer)?);
        Ok(())
    }

    fn serialize_value<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        let key = self
            .next_key
            .take()
            .ok_or_else(|| TreeSerdeError("serialize_value called before serialize_key".into()))?;
        self.entries
            .push((key, value.serialize(TreeValueSerializer)?));
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        if self.next_key.is_some() {
            return Err(TreeSerdeError(
                "serialize_map ended with a dangling key".into(),
            ));
        }
        Ok(TreeValue::Map(self.entries))
    }
}

impl SerializeTupleVariant for TreeVariantSeqSerializer {
    type Ok = TreeValue;
    type Error = TreeSerdeError;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        self.values.push(value.serialize(TreeValueSerializer)?);
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::EnumTuple(self.variant, self.values))
    }
}

impl SerializeStructVariant for TreeVariantStructSerializer {
    type Ok = TreeValue;
    type Error = TreeSerdeError;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        self.fields
            .push((key.into(), value.serialize(TreeValueSerializer)?));
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(TreeValue::EnumStruct(self.variant, self.fields))
    }
}

pub fn tree_value_from_serialize<T: Serialize>(value: &T) -> CoreResult<TreeValue> {
    value.serialize(TreeValueSerializer).map_err(|e| {
        Error::Serialization(format!(
            "Failed to encode external input into selection tree: {}",
            e
        ))
    })
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

fn encode_leaf_bytes(value: &TreeValue) -> CoreResult<Vec<u8>> {
    let mut out = Vec::new();
    match value {
        TreeValue::Unit => {}
        TreeValue::Bool(value) => out.push(u8::from(*value)),
        TreeValue::U8(value) => out.push(*value),
        TreeValue::U16(value) => out.extend_from_slice(&value.to_le_bytes()),
        TreeValue::U32(value) => out.extend_from_slice(&value.to_le_bytes()),
        TreeValue::U64(value) => out.extend_from_slice(&value.to_le_bytes()),
        TreeValue::I8(value) => out.push(*value as u8),
        TreeValue::I16(value) => out.extend_from_slice(&value.to_le_bytes()),
        TreeValue::I32(value) => out.extend_from_slice(&value.to_le_bytes()),
        TreeValue::I64(value) => out.extend_from_slice(&value.to_le_bytes()),
        TreeValue::String(value) => {
            push_u64(&mut out, value.len() as u64);
            out.extend_from_slice(value.as_bytes());
        }
        TreeValue::Struct(_)
        | TreeValue::List(_)
        | TreeValue::ListHandle(_)
        | TreeValue::Map(_)
        | TreeValue::EnumUnit(_)
        | TreeValue::EnumNewtype(_, _)
        | TreeValue::EnumTuple(_, _)
        | TreeValue::EnumStruct(_, _)
        | TreeValue::BytesPage { .. } => {
            return Err(Error::Serialization(
                "Expected leaf value while encoding selection payload".into(),
            ))
        }
    }
    Ok(out)
}

/// Byte length of a list-handle header: `0x09` tag + 32-byte root + 8-byte len
/// + 8-byte inner-length prefix. The inline `0x02` list payload follows.
pub const LIST_HANDLE_HEADER_LEN: u64 = 1 + 32 + 8 + 8;

/// Direct children of a `TreeValue`, in the order their payloads/roots are laid
/// out by [`assemble_subtree`]. Used to drive an explicit-stack post-order
/// traversal instead of recursing (which overflows the stack on deeply nested
/// recur-sequence values).
pub fn subtree_children(value: &TreeValue) -> Vec<&TreeValue> {
    match value {
        TreeValue::Struct(fields) => fields.iter().map(|(_, child)| child).collect(),
        TreeValue::List(values) | TreeValue::ListHandle(values) => values.iter().collect(),
        TreeValue::Map(entries) => {
            let mut children = Vec::with_capacity(entries.len() * 2);
            for (key, value) in entries {
                children.push(key);
                children.push(value);
            }
            children
        }
        TreeValue::EnumNewtype(_, child) => vec![child.as_ref()],
        TreeValue::EnumTuple(_, values) => values.iter().collect(),
        TreeValue::EnumStruct(_, fields) => fields.iter().map(|(_, child)| child).collect(),
        _ => Vec::new(),
    }
}

/// Combine a node's already-computed child `(payload, root)` results into the
/// node's own `(payload, root)`. `children` must be in [`subtree_children`]
/// order. Byte-for-byte identical to the previous recursive implementation.
pub fn assemble_subtree(
    value: &TreeValue,
    children: Vec<(Vec<u8>, Hash32)>,
) -> CoreResult<(Vec<u8>, Hash32)> {
    let result = match value {
        TreeValue::Unit => (vec![0x03], selection_hash(&[b"unit"])),
        TreeValue::Struct(fields) => {
            if fields.len() != children.len() {
                return Err(Error::Serialization(
                    "Struct child count does not match its field count".into(),
                ));
            }
            let mut payload = Vec::new();
            payload.push(0x01);
            push_u64(&mut payload, children.len() as u64);
            for ((name, _), (child_payload, _)) in fields.iter().zip(children.iter()) {
                push_u64(&mut payload, name.len() as u64);
                payload.extend_from_slice(name.as_bytes());
                push_u64(&mut payload, child_payload.len() as u64);
                payload.extend_from_slice(child_payload);
            }

            let root = struct_commitments_root(
                fields
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .zip(children.iter().map(|(_, root)| root.as_slice())),
            );
            (payload, root)
        }
        TreeValue::List(_) => {
            let mut payload = Vec::new();
            payload.push(0x02);
            push_u64(&mut payload, children.len() as u64);
            for (child_payload, _) in &children {
                push_u64(&mut payload, child_payload.len() as u64);
                payload.extend_from_slice(child_payload);
            }

            let child_roots: Vec<Hash32> = children.iter().map(|(_, root)| *root).collect();
            (payload, list_root_from_hashes(&child_roots))
        }
        TreeValue::ListHandle(_) => {
            // First build the inline `0x02` list payload (identical to `List`)…
            let mut inner = Vec::new();
            inner.push(0x02);
            push_u64(&mut inner, children.len() as u64);
            for (child_payload, _) in &children {
                push_u64(&mut inner, child_payload.len() as u64);
                inner.extend_from_slice(child_payload);
            }
            let child_roots: Vec<Hash32> = children.iter().map(|(_, root)| *root).collect();
            let root = list_root_from_hashes(&child_roots);

            // …then wrap it in a handle header `0x09 [root:32][len:8][inner_len:8]`.
            // A parent's `parse_subtree_root` reads the stored root and skips the
            // inline region, so it never re-Merkleizes the list; the elements are
            // still present for selection.
            let mut payload = Vec::with_capacity(LIST_HANDLE_HEADER_LEN as usize + inner.len());
            payload.push(0x09);
            payload.extend_from_slice(&root);
            push_u64(&mut payload, children.len() as u64);
            push_u64(&mut payload, inner.len() as u64);
            payload.extend_from_slice(&inner);
            (payload, root)
        }
        TreeValue::Map(_) => {
            // `children` is [key0, value0, key1, value1, ...]; re-pair before sorting.
            let mut entries_with_payloads = Vec::with_capacity(children.len() / 2);
            let mut iter = children.into_iter();
            while let (Some((key_payload, key_root)), Some((value_payload, value_root))) =
                (iter.next(), iter.next())
            {
                entries_with_payloads.push((key_payload, key_root, value_payload, value_root));
            }
            entries_with_payloads.sort_by(|left, right| left.0.cmp(&right.0));

            let mut payload = Vec::new();
            payload.push(0x04);
            push_u64(&mut payload, entries_with_payloads.len() as u64);
            for (key_payload, _, value_payload, _) in &entries_with_payloads {
                push_u64(&mut payload, key_payload.len() as u64);
                payload.extend_from_slice(key_payload);
                push_u64(&mut payload, value_payload.len() as u64);
                payload.extend_from_slice(value_payload);
            }

            let entry_count = entries_with_payloads.len() as u64;
            let len_bytes = entry_count.to_le_bytes();
            let mut parts: Vec<&[u8]> = Vec::with_capacity(entries_with_payloads.len() * 2 + 2);
            parts.push(b"map");
            parts.push(&len_bytes);
            for (_, key_root, _, value_root) in &entries_with_payloads {
                parts.push(key_root.as_slice());
                parts.push(value_root.as_slice());
            }
            (payload, selection_hash(&parts))
        }
        TreeValue::EnumUnit(variant) => {
            let mut payload = Vec::new();
            payload.push(0x05);
            push_u64(&mut payload, variant.len() as u64);
            payload.extend_from_slice(variant.as_bytes());
            (payload, selection_hash(&[b"enum-unit", variant.as_bytes()]))
        }
        TreeValue::EnumNewtype(variant, _) => {
            let (child_payload, child_root) = &children[0];
            let mut payload = Vec::new();
            payload.push(0x06);
            push_u64(&mut payload, variant.len() as u64);
            payload.extend_from_slice(variant.as_bytes());
            push_u64(&mut payload, child_payload.len() as u64);
            payload.extend_from_slice(child_payload);
            (
                payload,
                selection_hash(&[b"enum-newtype", variant.as_bytes(), child_root.as_slice()]),
            )
        }
        TreeValue::EnumTuple(variant, _) => {
            let mut payload = Vec::new();
            payload.push(0x07);
            push_u64(&mut payload, variant.len() as u64);
            payload.extend_from_slice(variant.as_bytes());
            push_u64(&mut payload, children.len() as u64);
            for (child_payload, _) in &children {
                push_u64(&mut payload, child_payload.len() as u64);
                payload.extend_from_slice(child_payload);
            }

            let mut parts: Vec<&[u8]> = Vec::with_capacity(children.len() + 2);
            parts.push(b"enum-tuple");
            parts.push(variant.as_bytes());
            for (_, child_root) in &children {
                parts.push(child_root.as_slice());
            }
            (payload, selection_hash(&parts))
        }
        TreeValue::EnumStruct(variant, _) => {
            let mut payload = Vec::new();
            payload.push(0x08);
            push_u64(&mut payload, variant.len() as u64);
            payload.extend_from_slice(variant.as_bytes());
            push_u64(&mut payload, children.len() as u64);
            for (child_payload, _) in &children {
                push_u64(&mut payload, child_payload.len() as u64);
                payload.extend_from_slice(child_payload);
            }

            let mut parts: Vec<&[u8]> = Vec::with_capacity(children.len() + 2);
            parts.push(b"enum-struct");
            parts.push(variant.as_bytes());
            for (_, child_root) in &children {
                parts.push(child_root.as_slice());
            }
            (payload, selection_hash(&parts))
        }
        TreeValue::BytesPage {
            index,
            offset,
            len,
            bytes,
        } => (
            encode_bytes_page_payload(*index, *offset, *len, bytes),
            bytes_page_root(*index, *offset, *len, bytes),
        ),
        _ => {
            let leaf_bytes = encode_leaf_bytes(value)?;
            let mut payload = Vec::with_capacity(1 + 8 + leaf_bytes.len());
            payload.push(0x00);
            push_u64(&mut payload, leaf_bytes.len() as u64);
            payload.extend_from_slice(&leaf_bytes);
            let root = selection_hash(&[b"leaf", leaf_bytes.as_slice()]);
            (payload, root)
        }
    };
    Ok(result)
}

pub fn subtree_payload_and_root(root: &TreeValue) -> CoreResult<(Vec<u8>, Hash32)> {
    // Iterative post-order traversal with an explicit heap stack, so nesting
    // depth no longer consumes the call stack. Each frame collects its
    // children's results (in order) before assembling its own.
    struct Frame<'a> {
        value: &'a TreeValue,
        children: Vec<&'a TreeValue>,
        next: usize,
        results: Vec<(Vec<u8>, Hash32)>,
    }

    let mut stack: Vec<Frame> = vec![Frame {
        value: root,
        children: subtree_children(root),
        next: 0,
        results: Vec::new(),
    }];
    // Result of the most recently completed subtree, handed up to its parent.
    let mut completed: Option<(Vec<u8>, Hash32)> = None;

    while !stack.is_empty() {
        let next_child = {
            let frame = stack.last_mut().unwrap();
            if let Some(result) = completed.take() {
                frame.results.push(result);
            }
            if frame.next < frame.children.len() {
                let child = frame.children[frame.next];
                frame.next += 1;
                Some(child)
            } else {
                None
            }
        };

        match next_child {
            Some(child) => stack.push(Frame {
                value: child,
                children: subtree_children(child),
                next: 0,
                results: Vec::new(),
            }),
            None => {
                let frame = stack.pop().unwrap();
                completed = Some(assemble_subtree(frame.value, frame.results)?);
            }
        }
    }

    completed.ok_or_else(|| Error::Serialization("empty selection tree".into()))
}

pub fn list_root_from_hashes(hashes: &[Hash32]) -> Hash32 {
    let len = hashes.len() as u64;
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
