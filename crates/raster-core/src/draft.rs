use alloc::boxed::Box;
use alloc::collections::BTreeMap;
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

use crate::collections::{bytes_page_parts, BYTES_PAGE_NEWTYPE_NAME};
use crate::input::{
    bytes_page_root, encode_bytes_page_payload, AppendFrontier, Schema, SchemaField,
    SchemaFieldMode, SchemaNode,
};
use crate::{Error, Result};

pub type DraftId = [u8; 32];
pub type DraftRoot = [u8; 32];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct DraftReplayHandle {
    pub draft_id: DraftId,
    pub schema_hash: [u8; 32],
    pub root_before: DraftRoot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct DraftReplayTransition {
    pub draft_id: DraftId,
    pub schema_hash: [u8; 32],
    pub root_before: DraftRoot,
    pub ops: Vec<DraftOp>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TileReplayJournal {
    /// `sha256` of the raw input bytes the tile was replayed on. Committed by
    /// the tile guest so the replay proof self-certifies *what it ran on*: the
    /// transition guest asserts this equals the hash of the recorded input
    /// witness, binding the proof to `B(recorded input) = output` rather than
    /// to `output` being merely some output of `B`. See
    /// `docs/proposals/program-identity.md`.
    pub input_commitment: [u8; 32],
    pub output_bytes: Vec<u8>,
    pub draft_transition: Option<DraftReplayTransition>,
    /// `Some` for every recur tile, `None` for every other tile.
    ///
    /// Membership is by **recur site**, not by subtree. An ordinary tile in a
    /// recur sequence's own body emits `None` — it carries no `RecurInput`, and
    /// a recur sequence cannot terminate early, so that site's completeness
    /// comes from trace structure instead. A `call_recur!` *nested* inside such
    /// an iteration is a different site: its tiles emit `Some`, attributed to
    /// the nested site by their own coordinates.
    ///
    /// This is a **binding, not an authority**. It commits what the tile *saw*
    /// in its input; committing a value does not make it true. Its role is
    /// exactly `input_commitment`'s — "this is what ran" — and authority comes
    /// from checking it against the authenticated source metadata.
    pub recur: Option<RecurTileReplay>,
}

/// The two facts a recur iteration must prove about itself: where it sat in the
/// source, and how it terminated.
///
/// Neither is recoverable from bytes the guest already parses. `RecurInput` is
/// `{ value, index, len }` with `value` **first**, so `index` and `len` sit
/// behind an arbitrary-length `T` and cannot be reached without a full decoder
/// for user types inside guest audit code. Termination has the same problem
/// from the other side: a recur tile's return type varies by mode, so no fixed
/// offset in `output_bytes` holds the control discriminant. So the tile commits
/// both directly, and the replay receipt covers them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RecurTileReplay {
    pub position: RecurPosition,
    /// Always explicit. A recur tile whose return type carries no
    /// `RecurControl` emits `Continue` rather than omitting the field: reading
    /// an absence as `Continue` would put a default in guest audit code, where
    /// it is indistinguishable from a field a malicious or merely buggy
    /// producer failed to set. The wrapper knows the return kind statically, so
    /// the redundancy is resolved at the producer and the audit reads a value
    /// in every case.
    pub control: RecurControlKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct RecurPosition {
    pub iteration_index: u64,
    /// `RecurInput.len` — the number of iterations the tile was told to expect,
    /// **not** the source length. The two differ under `chunk = N`, and the
    /// name must not paper over that: `RecurInput::is_last()` is
    /// `index + 1 == len`, so this is load-bearing user-visible semantics and
    /// has to stay the iteration count. The audit relates it to the
    /// authenticated source length itself.
    pub declared_iterations: u64,
    /// Elements this iteration consumed: 1 for element recur, the chunk size
    /// for chunked recur, short on the final chunk. Replaces
    /// `chunking::iteration_chunk_len`'s leading-varint inspection outright.
    pub consumed_elements: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum RecurControlKind {
    Continue,
    Break,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum DraftValue {
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
    Struct(Vec<(String, DraftValue)>),
    List(Vec<DraftValue>),
    Map(Vec<(DraftValue, DraftValue)>),
    EnumUnit(String),
    EnumNewtype(String, Box<DraftValue>),
    EnumTuple(String, Vec<DraftValue>),
    EnumStruct(String, Vec<(String, DraftValue)>),
    BytesPage {
        index: u64,
        offset: u64,
        len: u64,
        bytes: Vec<u8>,
    },
}

/// The **runtime's** own representation of a draft field: the real object, held
/// in memory so `finalize` can materialize it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum DraftFieldValue {
    Set(DraftValue),
    Append(Vec<DraftValue>),
}

/// What crosses into the trace and the guest.
///
/// An append field is witnessed by the right edge of its Merkle tree, not by
/// the elements: the guest's whole obligation is to supply a preimage of
/// `root_before` that `ops` can be applied to, and for an append-only list that
/// preimage is O(log N) digests rather than the list. It never inspects element
/// values, so it never needed them. See
/// `docs/proposals/incremental-draft-witness.md`.
///
/// Deliberately a different type from [`DraftFieldValue`]: sharing one enum
/// would leave the witness able to express a full append log, which is the
/// thing being removed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum DraftWitnessField {
    Set(DraftValue),
    Append(AppendFrontier),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum DraftOp {
    Set { field: String, value: DraftValue },
    Push { field: String, value: DraftValue },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct DraftStateWitness {
    pub schema: SchemaNode,
    pub fields: Vec<(String, DraftWitnessField)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct DraftTransitionWitness {
    pub pre_state: DraftStateWitness,
    pub native_transition: Option<DraftReplayTransition>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TrackedDraftState {
    pub schema_hash: [u8; 32],
    pub root: DraftRoot,
}

#[derive(Debug, Clone)]
struct DraftSerdeError(String);

impl fmt::Display for DraftSerdeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::error::Error for DraftSerdeError {}

impl ser::Error for DraftSerdeError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

type DraftSerdeResult<T> = core::result::Result<T, DraftSerdeError>;

struct DraftValueSerializer;

struct DraftSeqSerializer {
    values: Vec<DraftValue>,
}

struct DraftStructSerializer {
    fields: Vec<(String, DraftValue)>,
}

struct DraftMapSerializer {
    entries: Vec<(DraftValue, DraftValue)>,
    next_key: Option<DraftValue>,
}

struct DraftVariantSeqSerializer {
    variant: String,
    values: Vec<DraftValue>,
}

struct DraftVariantStructSerializer {
    variant: String,
    fields: Vec<(String, DraftValue)>,
}

impl Serializer for DraftValueSerializer {
    type Ok = DraftValue;
    type Error = DraftSerdeError;
    type SerializeSeq = DraftSeqSerializer;
    type SerializeTuple = DraftSeqSerializer;
    type SerializeTupleStruct = DraftSeqSerializer;
    type SerializeStruct = DraftStructSerializer;
    type SerializeTupleVariant = DraftVariantSeqSerializer;
    type SerializeMap = DraftMapSerializer;
    type SerializeStructVariant = DraftVariantStructSerializer;

    fn serialize_bool(self, value: bool) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::Bool(value))
    }
    fn serialize_i8(self, value: i8) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::I8(value))
    }
    fn serialize_i16(self, value: i16) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::I16(value))
    }
    fn serialize_i32(self, value: i32) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::I32(value))
    }
    fn serialize_i64(self, value: i64) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::I64(value))
    }
    fn serialize_u8(self, value: u8) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::U8(value))
    }
    fn serialize_u16(self, value: u16) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::U16(value))
    }
    fn serialize_u32(self, value: u32) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::U32(value))
    }
    fn serialize_u64(self, value: u64) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::U64(value))
    }
    fn serialize_f32(self, _value: f32) -> DraftSerdeResult<Self::Ok> {
        Err(DraftSerdeError(
            "f32 is not supported by draft transitions".into(),
        ))
    }
    fn serialize_f64(self, _value: f64) -> DraftSerdeResult<Self::Ok> {
        Err(DraftSerdeError(
            "f64 is not supported by draft transitions".into(),
        ))
    }
    fn serialize_char(self, _value: char) -> DraftSerdeResult<Self::Ok> {
        Err(DraftSerdeError(
            "char is not supported by draft transitions".into(),
        ))
    }
    fn serialize_str(self, value: &str) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::String(value.into()))
    }
    fn serialize_bytes(self, _value: &[u8]) -> DraftSerdeResult<Self::Ok> {
        Err(DraftSerdeError(
            "raw bytes are not supported by draft transitions".into(),
        ))
    }
    fn serialize_none(self) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::Unit)
    }
    fn serialize_some<T>(self, value: &T) -> DraftSerdeResult<Self::Ok>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }
    fn serialize_unit(self) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::Unit)
    }
    fn serialize_unit_struct(self, _name: &'static str) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::Unit)
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::EnumUnit(variant.into()))
    }
    fn serialize_newtype_struct<T>(
        self,
        name: &'static str,
        value: &T,
    ) -> DraftSerdeResult<Self::Ok>
    where
        T: ?Sized + Serialize,
    {
        // Act on the name *before* serializing. The general-purpose serializer
        // would expand the page payload into one `DraftValue::U8` per byte, only
        // for `bytes_page_parts` to collapse it right back — see that function's
        // note on why the `List<T>` handle can serialize-then-match and a page
        // cannot.
        if name == BYTES_PAGE_NEWTYPE_NAME {
            let parts = bytes_page_parts(value).map_err(DraftSerdeError)?;
            return Ok(DraftValue::BytesPage {
                index: parts.index,
                offset: parts.offset,
                len: parts.len,
                bytes: parts.bytes,
            });
        }
        value.serialize(DraftValueSerializer)
    }
    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> DraftSerdeResult<Self::Ok>
    where
        T: ?Sized + Serialize,
    {
        Ok(DraftValue::EnumNewtype(
            variant.into(),
            Box::new(value.serialize(DraftValueSerializer)?),
        ))
    }
    fn serialize_seq(self, len: Option<usize>) -> DraftSerdeResult<Self::SerializeSeq> {
        Ok(DraftSeqSerializer {
            values: Vec::with_capacity(len.unwrap_or_default()),
        })
    }
    fn serialize_tuple(self, len: usize) -> DraftSerdeResult<Self::SerializeTuple> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> DraftSerdeResult<Self::SerializeTupleStruct> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> DraftSerdeResult<Self::SerializeTupleVariant> {
        Ok(DraftVariantSeqSerializer {
            variant: variant.into(),
            values: Vec::with_capacity(len),
        })
    }
    fn serialize_map(self, len: Option<usize>) -> DraftSerdeResult<Self::SerializeMap> {
        Ok(DraftMapSerializer {
            entries: Vec::with_capacity(len.unwrap_or_default()),
            next_key: None,
        })
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> DraftSerdeResult<Self::SerializeStruct> {
        Ok(DraftStructSerializer {
            fields: Vec::with_capacity(len),
        })
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> DraftSerdeResult<Self::SerializeStructVariant> {
        Ok(DraftVariantStructSerializer {
            variant: variant.into(),
            fields: Vec::with_capacity(len),
        })
    }
}

impl SerializeSeq for DraftSeqSerializer {
    type Ok = DraftValue;
    type Error = DraftSerdeError;
    fn serialize_element<T>(&mut self, value: &T) -> DraftSerdeResult<()>
    where
        T: ?Sized + Serialize,
    {
        self.values.push(value.serialize(DraftValueSerializer)?);
        Ok(())
    }
    fn end(self) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::List(self.values))
    }
}

impl SerializeTuple for DraftSeqSerializer {
    type Ok = DraftValue;
    type Error = DraftSerdeError;
    fn serialize_element<T>(&mut self, value: &T) -> DraftSerdeResult<()>
    where
        T: ?Sized + Serialize,
    {
        SerializeSeq::serialize_element(self, value)
    }
    fn end(self) -> DraftSerdeResult<Self::Ok> {
        SerializeSeq::end(self)
    }
}

impl SerializeTupleStruct for DraftSeqSerializer {
    type Ok = DraftValue;
    type Error = DraftSerdeError;
    fn serialize_field<T>(&mut self, value: &T) -> DraftSerdeResult<()>
    where
        T: ?Sized + Serialize,
    {
        SerializeSeq::serialize_element(self, value)
    }
    fn end(self) -> DraftSerdeResult<Self::Ok> {
        SerializeSeq::end(self)
    }
}

impl SerializeStruct for DraftStructSerializer {
    type Ok = DraftValue;
    type Error = DraftSerdeError;
    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> DraftSerdeResult<()>
    where
        T: ?Sized + Serialize,
    {
        self.fields
            .push((key.into(), value.serialize(DraftValueSerializer)?));
        Ok(())
    }
    fn end(self) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::Struct(self.fields))
    }
}

impl SerializeMap for DraftMapSerializer {
    type Ok = DraftValue;
    type Error = DraftSerdeError;
    fn serialize_key<T>(&mut self, key: &T) -> DraftSerdeResult<()>
    where
        T: ?Sized + Serialize,
    {
        self.next_key = Some(key.serialize(DraftValueSerializer)?);
        Ok(())
    }
    fn serialize_value<T>(&mut self, value: &T) -> DraftSerdeResult<()>
    where
        T: ?Sized + Serialize,
    {
        let key = self
            .next_key
            .take()
            .ok_or_else(|| DraftSerdeError("serialize_value called before serialize_key".into()))?;
        self.entries
            .push((key, value.serialize(DraftValueSerializer)?));
        Ok(())
    }
    fn end(self) -> DraftSerdeResult<Self::Ok> {
        if self.next_key.is_some() {
            return Err(DraftSerdeError(
                "serialize_map ended with a dangling key".into(),
            ));
        }
        Ok(DraftValue::Map(self.entries))
    }
}

impl SerializeTupleVariant for DraftVariantSeqSerializer {
    type Ok = DraftValue;
    type Error = DraftSerdeError;
    fn serialize_field<T>(&mut self, value: &T) -> DraftSerdeResult<()>
    where
        T: ?Sized + Serialize,
    {
        self.values.push(value.serialize(DraftValueSerializer)?);
        Ok(())
    }
    fn end(self) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::EnumTuple(self.variant, self.values))
    }
}

impl SerializeStructVariant for DraftVariantStructSerializer {
    type Ok = DraftValue;
    type Error = DraftSerdeError;
    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> DraftSerdeResult<()>
    where
        T: ?Sized + Serialize,
    {
        self.fields
            .push((key.into(), value.serialize(DraftValueSerializer)?));
        Ok(())
    }
    fn end(self) -> DraftSerdeResult<Self::Ok> {
        Ok(DraftValue::EnumStruct(self.variant, self.fields))
    }
}

pub fn draft_value_from_serialize<T: Serialize>(value: &T) -> Result<DraftValue> {
    value
        .serialize(DraftValueSerializer)
        .map_err(|e| Error::Serialization(format!("Failed to serialize draft value: {}", e)))
}

fn selection_hash(parts: &[&[u8]]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().to_vec()
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn encode_leaf_bytes(value: &DraftValue) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    match value {
        DraftValue::Unit => {}
        DraftValue::Bool(value) => out.push(u8::from(*value)),
        DraftValue::U8(value) => out.push(*value),
        DraftValue::U16(value) => out.extend_from_slice(&value.to_le_bytes()),
        DraftValue::U32(value) => out.extend_from_slice(&value.to_le_bytes()),
        DraftValue::U64(value) => out.extend_from_slice(&value.to_le_bytes()),
        DraftValue::I8(value) => out.push(*value as u8),
        DraftValue::I16(value) => out.extend_from_slice(&value.to_le_bytes()),
        DraftValue::I32(value) => out.extend_from_slice(&value.to_le_bytes()),
        DraftValue::I64(value) => out.extend_from_slice(&value.to_le_bytes()),
        DraftValue::String(value) => {
            push_u64(&mut out, value.len() as u64);
            out.extend_from_slice(value.as_bytes());
        }
        DraftValue::Struct(_)
        | DraftValue::List(_)
        | DraftValue::Map(_)
        | DraftValue::EnumUnit(_)
        | DraftValue::EnumNewtype(_, _)
        | DraftValue::EnumTuple(_, _)
        | DraftValue::EnumStruct(_, _)
        | DraftValue::BytesPage { .. } => {
            return Err(Error::Serialization(
                "Expected leaf value while encoding draft payload".into(),
            ))
        }
    }
    Ok(out)
}

fn list_root_from_hashes(hashes: &[Vec<u8>]) -> Vec<u8> {
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

pub fn draft_value_payload_and_root(value: &DraftValue) -> Result<(Vec<u8>, Vec<u8>)> {
    match value {
        DraftValue::Unit => Ok((vec![0x03], selection_hash(&[b"unit"]))),
        DraftValue::Struct(fields) => {
            // Field names go into both the payload and the root hash: a
            // draft's root must be indistinguishable from the selection-tree
            // root of the same data (that equality is what lets a finalized
            // draft be selected into like any other object), so this must
            // track `input::assemble_subtree` exactly. Sharing
            // `struct_commitments_root` is what keeps that true.
            let mut payload = Vec::new();
            payload.push(0x01);
            push_u64(&mut payload, fields.len() as u64);
            let mut child_roots = Vec::with_capacity(fields.len());
            for (name, child) in fields {
                let (child_payload, child_root) = draft_value_payload_and_root(child)?;
                push_u64(&mut payload, name.len() as u64);
                payload.extend_from_slice(name.as_bytes());
                push_u64(&mut payload, child_payload.len() as u64);
                payload.extend_from_slice(&child_payload);
                child_roots.push((name.as_str(), child_root));
            }
            let root = crate::input::struct_commitments_root(
                child_roots
                    .iter()
                    .map(|(name, root)| (*name, root.as_slice())),
            );
            Ok((payload, root.to_vec()))
        }
        DraftValue::List(values) => {
            let mut payload = Vec::new();
            payload.push(0x02);
            push_u64(&mut payload, values.len() as u64);
            let mut child_roots = Vec::with_capacity(values.len());
            for child in values {
                let (child_payload, child_root) = draft_value_payload_and_root(child)?;
                push_u64(&mut payload, child_payload.len() as u64);
                payload.extend_from_slice(&child_payload);
                child_roots.push(child_root);
            }
            Ok((payload, list_root_from_hashes(&child_roots)))
        }
        DraftValue::Map(entries) => {
            let mut entries_with_payloads = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                let (key_payload, key_root) = draft_value_payload_and_root(key)?;
                let (value_payload, value_root) = draft_value_payload_and_root(value)?;
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
            Ok((payload, selection_hash(&parts)))
        }
        DraftValue::EnumUnit(variant) => {
            let mut payload = Vec::new();
            payload.push(0x05);
            push_u64(&mut payload, variant.len() as u64);
            payload.extend_from_slice(variant.as_bytes());
            Ok((payload, selection_hash(&[b"enum-unit", variant.as_bytes()])))
        }
        DraftValue::EnumNewtype(variant, value) => {
            let (child_payload, child_root) = draft_value_payload_and_root(value)?;
            let mut payload = Vec::new();
            payload.push(0x06);
            push_u64(&mut payload, variant.len() as u64);
            payload.extend_from_slice(variant.as_bytes());
            push_u64(&mut payload, child_payload.len() as u64);
            payload.extend_from_slice(&child_payload);
            Ok((
                payload,
                selection_hash(&[b"enum-newtype", variant.as_bytes(), child_root.as_slice()]),
            ))
        }
        DraftValue::EnumTuple(variant, values) => {
            let mut payload = Vec::new();
            payload.push(0x07);
            push_u64(&mut payload, variant.len() as u64);
            payload.extend_from_slice(variant.as_bytes());
            push_u64(&mut payload, values.len() as u64);
            let mut child_roots = Vec::with_capacity(values.len());
            for child in values {
                let (child_payload, child_root) = draft_value_payload_and_root(child)?;
                push_u64(&mut payload, child_payload.len() as u64);
                payload.extend_from_slice(&child_payload);
                child_roots.push(child_root);
            }
            let mut parts: Vec<&[u8]> = Vec::with_capacity(child_roots.len() + 2);
            parts.push(b"enum-tuple");
            parts.push(variant.as_bytes());
            for root in &child_roots {
                parts.push(root.as_slice());
            }
            Ok((payload, selection_hash(&parts)))
        }
        DraftValue::EnumStruct(variant, fields) => {
            let mut payload = Vec::new();
            payload.push(0x08);
            push_u64(&mut payload, variant.len() as u64);
            payload.extend_from_slice(variant.as_bytes());
            push_u64(&mut payload, fields.len() as u64);
            let mut child_roots = Vec::with_capacity(fields.len());
            for (_, child) in fields {
                let (child_payload, child_root) = draft_value_payload_and_root(child)?;
                push_u64(&mut payload, child_payload.len() as u64);
                payload.extend_from_slice(&child_payload);
                child_roots.push(child_root);
            }
            let mut parts: Vec<&[u8]> = Vec::with_capacity(child_roots.len() + 2);
            parts.push(b"enum-struct");
            parts.push(variant.as_bytes());
            for root in &child_roots {
                parts.push(root.as_slice());
            }
            Ok((payload, selection_hash(&parts)))
        }
        DraftValue::BytesPage {
            index,
            offset,
            len,
            bytes,
        } => Ok((
            encode_bytes_page_payload(*index, *offset, *len, bytes),
            bytes_page_root(*index, *offset, *len, bytes).to_vec(),
        )),
        _ => {
            let leaf_bytes = encode_leaf_bytes(value)?;
            let mut payload = Vec::new();
            payload.push(0x00);
            push_u64(&mut payload, leaf_bytes.len() as u64);
            payload.extend_from_slice(&leaf_bytes);
            Ok((payload, selection_hash(&[b"leaf", leaf_bytes.as_slice()])))
        }
    }
}

pub fn draft_value_root(value: &DraftValue) -> Result<DraftRoot> {
    let (_, root) = draft_value_payload_and_root(value)?;
    root.as_slice()
        .try_into()
        .map_err(|_| Error::Other("Draft root must be 32 bytes".into()))
}

pub fn schema_hash(schema: &SchemaNode) -> [u8; 32] {
    let schema_bytes = postcard::to_allocvec(schema).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(schema_bytes);
    hasher.finalize().into()
}

fn schema_struct_fields(schema: &SchemaNode) -> Result<&[SchemaField]> {
    match schema {
        SchemaNode::Struct { fields, .. } => Ok(fields.as_slice()),
        _ => Err(Error::Other(
            "Drafts currently support only struct schemas at the root".into(),
        )),
    }
}

fn locate_schema_field<'a>(schema: &'a SchemaNode, name: &str) -> Result<&'a SchemaField> {
    schema_struct_fields(schema)?
        .iter()
        .find(|field| field.name == name)
        .ok_or_else(|| Error::Other(format!("Unknown draft field '{}'", name)))
}

/// Materialize the whole draft object from the runtime's own field values.
///
/// The finalize path, and only the finalize path — O(N) **once**, which was
/// never the problem. Witnesses go through [`draft_root_from_witness`], which
/// never materializes anything.
pub fn draft_tree_from_fields(
    schema: &SchemaNode,
    fields: &BTreeMap<String, DraftFieldValue>,
    require_complete: bool,
) -> Result<DraftValue> {
    let schema_fields = schema_struct_fields(schema)?;
    let mut values = Vec::with_capacity(schema_fields.len());
    for field in schema_fields {
        let value = match (field.mode, fields.get(&field.name)) {
            (SchemaFieldMode::SetOnce, Some(DraftFieldValue::Set(value))) => value.clone(),
            (SchemaFieldMode::SetOnce, Some(DraftFieldValue::Append(_))) => {
                return Err(Error::Other(format!(
                    "Draft field '{}' expected a set-once value, found append log",
                    field.name
                )))
            }
            (SchemaFieldMode::SetOnce, None) if require_complete => {
                return Err(Error::Other(format!(
                    "Draft field '{}' must be written before finalize",
                    field.name
                )))
            }
            (SchemaFieldMode::SetOnce, None) => DraftValue::Unit,
            (SchemaFieldMode::AppendOnlyVec, Some(DraftFieldValue::Append(values))) => {
                DraftValue::List(values.clone())
            }
            (SchemaFieldMode::AppendOnlyVec, Some(DraftFieldValue::Set(_))) => {
                return Err(Error::Other(format!(
                    "Draft field '{}' expected an append-only vector, found scalar value",
                    field.name
                )))
            }
            (SchemaFieldMode::AppendOnlyVec, None) => DraftValue::List(Vec::new()),
        };
        values.push((field.name.clone(), value));
    }
    Ok(DraftValue::Struct(values))
}

pub fn witness_fields_map(
    fields: &[(String, DraftWitnessField)],
) -> BTreeMap<String, DraftWitnessField> {
    fields.iter().cloned().collect()
}

/// The root a field contributes when the draft has never written it: `Unit` for
/// a set-once field, the empty list for an append field.
///
/// Spelled through the same functions the written cases use, so an unwritten
/// field cannot drift away from a written-then-emptied one.
fn absent_field_root(mode: SchemaFieldMode) -> Result<DraftRoot> {
    match mode {
        SchemaFieldMode::SetOnce => draft_value_root(&DraftValue::Unit),
        SchemaFieldMode::AppendOnlyVec => AppendFrontier::empty()
            .root()
            .ok_or_else(|| Error::Other("Empty append frontier must have a root".into())),
    }
}

/// Compose a draft's root from per-field roots, without materializing a field.
///
/// The struct step is [`crate::input::struct_commitments_root`], untouched —
/// that shared spelling is what keeps a draft root indistinguishable from the
/// selection-tree root of the same data.
pub fn draft_root_from_field_roots(
    schema: &SchemaNode,
    roots: &BTreeMap<String, DraftRoot>,
) -> Result<DraftRoot> {
    let schema_fields = schema_struct_fields(schema)?;
    let mut child_roots = Vec::with_capacity(schema_fields.len());
    for field in schema_fields {
        let root = match roots.get(&field.name) {
            Some(root) => *root,
            None => absent_field_root(field.mode)?,
        };
        child_roots.push((field.name.as_str(), root));
    }
    Ok(crate::input::struct_commitments_root(
        child_roots
            .iter()
            .map(|(name, root)| (*name, root.as_slice())),
    ))
}

/// The root of one witnessed field, plus the mode conformance check that used
/// to live in [`draft_tree_from_fields`].
fn witness_field_root(field: &SchemaField, value: &DraftWitnessField) -> Result<DraftRoot> {
    match (field.mode, value) {
        (SchemaFieldMode::SetOnce, DraftWitnessField::Set(value)) => draft_value_root(value),
        (SchemaFieldMode::SetOnce, DraftWitnessField::Append(_)) => Err(Error::Other(format!(
            "Draft field '{}' expected a set-once value, found append frontier",
            field.name
        ))),
        (SchemaFieldMode::AppendOnlyVec, DraftWitnessField::Append(frontier)) => {
            frontier.root().ok_or_else(|| {
                Error::Other(format!(
                    "Draft field '{}' carries a malformed append frontier",
                    field.name
                ))
            })
        }
        (SchemaFieldMode::AppendOnlyVec, DraftWitnessField::Set(_)) => Err(Error::Other(format!(
            "Draft field '{}' expected an append-only vector, found scalar value",
            field.name
        ))),
    }
}

/// Recompute a draft root from a witness in O(#fields · log N).
///
/// There is no `require_complete` counterpart: completeness is a finalize
/// concern, and every witness call site is mid-draft by construction.
pub fn draft_root_from_witness(
    schema: &SchemaNode,
    fields: &BTreeMap<String, DraftWitnessField>,
) -> Result<DraftRoot> {
    let mut roots = BTreeMap::new();
    for field in schema_struct_fields(schema)? {
        let Some(value) = fields.get(&field.name) else {
            continue;
        };
        roots.insert(field.name.clone(), witness_field_root(field, value)?);
    }
    // A field the schema does not declare contributes nothing to the root, and
    // is ignored rather than rejected — exactly as the materializing path did.
    // Rejecting it would be a new rule, and this change adds none.
    draft_root_from_field_roots(schema, &roots)
}

pub fn apply_draft_ops(
    witness: &DraftStateWitness,
    ops: &[DraftOp],
) -> Result<(DraftStateWitness, DraftRoot)> {
    let mut fields = witness_fields_map(&witness.fields);
    for op in ops {
        match op {
            DraftOp::Set { field, value } => {
                let schema_field = locate_schema_field(&witness.schema, field)?;
                if schema_field.mode != SchemaFieldMode::SetOnce {
                    return Err(Error::Other(format!(
                        "Draft field '{}' does not support set; use push",
                        field
                    )));
                }
                if fields.contains_key(field) {
                    return Err(Error::Other(format!(
                        "Draft field '{}' can only be written once",
                        field
                    )));
                }
                fields.insert(field.clone(), DraftWitnessField::Set(value.clone()));
            }
            DraftOp::Push { field, value } => {
                let schema_field = locate_schema_field(&witness.schema, field)?;
                if schema_field.mode != SchemaFieldMode::AppendOnlyVec {
                    return Err(Error::Other(format!(
                        "Draft field '{}' does not support push; use set",
                        field
                    )));
                }
                // The element's leaf hash is the same one `list_root_from_hashes`
                // consumes — taken from `ops`, which is per-step and already in
                // the replay journal. Nothing new has to be recorded to append.
                let leaf = draft_value_root(value)?;
                match fields.entry(field.clone()) {
                    alloc::collections::btree_map::Entry::Vacant(entry) => {
                        let mut frontier = AppendFrontier::empty();
                        frontier.push(leaf);
                        entry.insert(DraftWitnessField::Append(frontier));
                    }
                    alloc::collections::btree_map::Entry::Occupied(mut entry) => {
                        match entry.get_mut() {
                            DraftWitnessField::Append(frontier) => frontier.push(leaf),
                            DraftWitnessField::Set(_) => {
                                return Err(Error::Other(format!(
                                    "Draft field '{}' is not appendable",
                                    field
                                )))
                            }
                        }
                    }
                }
            }
        }
    }
    let root = draft_root_from_witness(&witness.schema, &fields)?;
    let fields = fields.into_iter().collect();
    Ok((
        DraftStateWitness {
            schema: witness.schema.clone(),
            fields,
        },
        root,
    ))
}

pub fn verify_witness_root(witness: &DraftStateWitness, expected_root: &DraftRoot) -> Result<()> {
    let fields = witness_fields_map(&witness.fields);
    let actual_root = draft_root_from_witness(&witness.schema, &fields)?;
    if &actual_root != expected_root {
        return Err(Error::Other(format!(
            "Draft witness root mismatch: expected {:?}, found {:?}",
            expected_root, actual_root
        )));
    }
    Ok(())
}

pub fn replay_handle_for_schema<S: Schema>(
    draft_id: DraftId,
    root_before: DraftRoot,
) -> DraftReplayHandle {
    DraftReplayHandle {
        draft_id,
        schema_hash: S::schema_hash(),
        root_before,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Selectable;

    struct DemoDraft;

    impl Selectable for DemoDraft {
        fn schema() -> SchemaNode {
            SchemaNode::Struct {
                type_name: "DemoDraft".into(),
                fields: vec![
                    SchemaField {
                        name: "title".into(),
                        label: "Title".into(),
                        mode: SchemaFieldMode::SetOnce,
                        schema: Box::new(SchemaNode::Leaf {
                            type_name: "String".into(),
                        }),
                    },
                    SchemaField {
                        name: "items".into(),
                        label: "Items".into(),
                        mode: SchemaFieldMode::AppendOnlyVec,
                        schema: Box::new(SchemaNode::List {
                            type_name: "Vec<String>".into(),
                            element: Box::new(SchemaNode::Leaf {
                                type_name: "String".into(),
                            }),
                        }),
                    },
                ],
            }
        }
    }

    fn demo_draft_value() -> DraftValue {
        DraftValue::Struct(vec![
            ("title".into(), DraftValue::String("collected".into())),
            (
                "items".into(),
                DraftValue::List(
                    (0..5)
                        .map(|index| DraftValue::String(format!("item-{}", index)))
                        .collect(),
                ),
            ),
        ])
    }

    /// A draft root is a committed artifact: it is what a finalized draft is
    /// selected against, so it must be indistinguishable from the selection-tree
    /// root of the same data. Pinned as a literal so a change to the hashing is
    /// caught here rather than in a dependent repository's fixtures.
    ///
    /// Computed on the value path, which `incremental-draft-witness` does not
    /// touch — the witness path is then checked against *this*, closing the
    /// chain frontier → witness root → value root → committed constant.
    #[test]
    fn draft_value_root_is_pinned() {
        assert_eq!(
            hex_root(&draft_value_root(&demo_draft_value()).unwrap()),
            "c15acacaebc3b29f20323b3cb5dad331226a2b0da19f6e07d42b1814fe5ac1cb",
        );
    }

    fn hex_root(root: &DraftRoot) -> String {
        root.iter().map(|byte| format!("{:02x}", byte)).collect()
    }

    #[test]
    fn apply_draft_ops_advances_roots_across_steps() {
        let witness = DraftStateWitness {
            schema: DemoDraft::schema(),
            fields: Vec::new(),
        };
        let empty_root = draft_root_from_witness(&witness.schema, &BTreeMap::new()).unwrap();

        let (step_one_state, step_one_root) = apply_draft_ops(
            &witness,
            &[
                DraftOp::Set {
                    field: "title".into(),
                    value: DraftValue::String("collected".into()),
                },
                DraftOp::Push {
                    field: "items".into(),
                    value: DraftValue::String("first".into()),
                },
            ],
        )
        .unwrap();
        assert_ne!(step_one_root, empty_root);

        let (_step_two_state, step_two_root) = apply_draft_ops(
            &step_one_state,
            &[DraftOp::Push {
                field: "items".into(),
                value: DraftValue::String("second".into()),
            }],
        )
        .unwrap();
        assert_ne!(step_two_root, step_one_root);
    }

    #[test]
    fn apply_draft_ops_rejects_duplicate_setonce_writes() {
        let witness = DraftStateWitness {
            schema: DemoDraft::schema(),
            fields: Vec::new(),
        };

        let error = apply_draft_ops(
            &witness,
            &[
                DraftOp::Set {
                    field: "title".into(),
                    value: DraftValue::String("first".into()),
                },
                DraftOp::Set {
                    field: "title".into(),
                    value: DraftValue::String("second".into()),
                },
            ],
        )
        .unwrap_err();

        assert!(error.to_string().contains("can only be written once"));
    }

    /// The invariant the whole change rests on: a root witnessed by a frontier
    /// equals the root of the materialized object, at every intermediate length.
    ///
    /// Together with `draft_value_root_is_pinned` and the frontier equivalence
    /// test in `input.rs`, this closes the chain — frontier root = witness root
    /// = value root = committed constant.
    #[test]
    fn witness_root_matches_the_materialized_value_root() {
        let schema = DemoDraft::schema();
        let mut witness = DraftStateWitness {
            schema: schema.clone(),
            fields: Vec::new(),
        };
        let mut values: Vec<DraftValue> = Vec::new();
        let mut materialized = BTreeMap::new();

        let (next, root) = apply_draft_ops(
            &witness,
            &[DraftOp::Set {
                field: "title".into(),
                value: DraftValue::String("collected".into()),
            }],
        )
        .unwrap();
        witness = next;
        materialized.insert(
            "title".to_string(),
            DraftFieldValue::Set(DraftValue::String("collected".into())),
        );
        assert_eq!(
            root,
            draft_value_root(&draft_tree_from_fields(&schema, &materialized, false).unwrap())
                .unwrap()
        );

        // 300 pushes crosses every padding transition in the range, including
        // the ones where the duplicated node moves up a level.
        for index in 0..300u32 {
            let value = DraftValue::String(format!("item-{}", index));
            let (next, root) = apply_draft_ops(
                &witness,
                &[DraftOp::Push {
                    field: "items".into(),
                    value: value.clone(),
                }],
            )
            .unwrap();
            witness = next;
            values.push(value);
            materialized.insert(
                "items".to_string(),
                DraftFieldValue::Append(values.clone()),
            );

            assert_eq!(
                root,
                draft_value_root(&draft_tree_from_fields(&schema, &materialized, false).unwrap())
                    .unwrap(),
                "witness root diverged from the materialized root after {} pushes",
                index + 1
            );
        }
    }

    /// The witness is flat in the number of appended elements — the property the
    /// authoring rule already promised and the encoding did not deliver.
    #[test]
    fn witness_size_does_not_track_the_appended_length() {
        let mut witness = DraftStateWitness {
            schema: DemoDraft::schema(),
            fields: Vec::new(),
        };

        let mut first_size = None;
        for index in 0..2048u32 {
            let (next, _) = apply_draft_ops(
                &witness,
                &[DraftOp::Push {
                    field: "items".into(),
                    value: DraftValue::String(format!("item-{}", index)),
                }],
            )
            .unwrap();
            witness = next;

            let size = postcard::to_allocvec(&witness.fields).unwrap().len();
            let first = *first_size.get_or_insert(size);
            assert!(
                size <= first + 512,
                "witness grew to {} bytes from {} after {} pushes",
                size,
                first,
                index + 1
            );
        }
    }
}
