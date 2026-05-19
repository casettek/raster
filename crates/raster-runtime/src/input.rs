use raster_core::input::{
    ExternalArg, ExternalSelection, ListProofDirection, ListProofSibling, SchemaNode, Selectable,
    SelectedPayload, SelectionProof, SelectionProofStep, SelectorPath, SelectorSegment,
};
use raster_core::{Error, Result as CoreResult};
use serde::de::{
    self, DeserializeOwned, DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess, Visitor,
};
use serde::ser::{
    self, Impossible, SerializeSeq, SerializeStruct, SerializeTuple, SerializeTupleStruct,
};
use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt;
use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

use crate::external_storage::{ExternalStorageManager, ResolvedExternalData};

fn load_external_storage() -> CoreResult<Option<ExternalStorageManager>> {
    ExternalStorageManager::from_cli_args()
}

#[derive(Debug, Clone, PartialEq)]
enum TreeValue {
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
}

#[derive(Debug, Clone)]
struct TreeSerdeError(String);

impl fmt::Display for TreeSerdeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TreeSerdeError {}

impl ser::Error for TreeSerdeError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

impl de::Error for TreeSerdeError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

type Result<T, E = TreeSerdeError> = std::result::Result<T, E>;

struct TreeValueSerializer;

struct TreeSeqSerializer {
    values: Vec<TreeValue>,
}

struct TreeStructSerializer {
    fields: Vec<(String, TreeValue)>,
}

impl Serializer for TreeValueSerializer {
    type Ok = TreeValue;
    type Error = TreeSerdeError;
    type SerializeSeq = TreeSeqSerializer;
    type SerializeTuple = TreeSeqSerializer;
    type SerializeTupleStruct = TreeSeqSerializer;
    type SerializeStruct = TreeStructSerializer;

    type SerializeTupleVariant = Impossible<TreeValue, TreeSerdeError>;
    type SerializeMap = Impossible<TreeValue, TreeSerdeError>;
    type SerializeStructVariant = Impossible<TreeValue, TreeSerdeError>;

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
        Err(TreeSerdeError(
            "Option::None is not supported by selection proofs".into(),
        ))
    }

    fn serialize_some<T>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        Err(TreeSerdeError(
            "unit values are not supported by selection proofs".into(),
        ))
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        Err(TreeSerdeError(
            "unit structs are not supported by selection proofs".into(),
        ))
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        Err(TreeSerdeError(
            "enum variants are not supported by selection proofs".into(),
        ))
    }

    fn serialize_newtype_struct<T>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        Err(TreeSerdeError(
            "enum variants are not supported by selection proofs".into(),
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
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Err(TreeSerdeError(
            "tuple variants are not supported by selection proofs".into(),
        ))
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Err(TreeSerdeError(
            "maps are not supported by selection proofs".into(),
        ))
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
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Err(TreeSerdeError(
            "struct variants are not supported by selection proofs".into(),
        ))
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

struct TreeValueDeserializer<'de> {
    value: &'de TreeValue,
}

impl<'de> TreeValueDeserializer<'de> {
    fn new(value: &'de TreeValue) -> Self {
        Self { value }
    }
}

struct TreeSeqAccess<'de> {
    iter: std::slice::Iter<'de, TreeValue>,
}

impl<'de> SeqAccess<'de> for TreeSeqAccess<'de> {
    type Error = TreeSerdeError;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        match self.iter.next() {
            Some(value) => seed
                .deserialize(TreeValueDeserializer::new(value))
                .map(Some),
            None => Ok(None),
        }
    }
}

struct TreeStructAccess<'de> {
    iter: std::slice::Iter<'de, (String, TreeValue)>,
    value: Option<&'de TreeValue>,
}

impl<'de> MapAccess<'de> for TreeStructAccess<'de> {
    type Error = TreeSerdeError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: DeserializeSeed<'de>,
    {
        match self.iter.next() {
            Some((key, value)) => {
                self.value = Some(value);
                seed.deserialize(key.as_str().into_deserializer()).map(Some)
            }
            None => Ok(None),
        }
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        let value = self
            .value
            .take()
            .ok_or_else(|| TreeSerdeError("missing struct field value".into()))?;
        seed.deserialize(TreeValueDeserializer::new(value))
    }
}

impl<'de> de::Deserializer<'de> for TreeValueDeserializer<'de> {
    type Error = TreeSerdeError;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::Bool(value) => visitor.visit_bool(*value),
            TreeValue::U8(value) => visitor.visit_u8(*value),
            TreeValue::U16(value) => visitor.visit_u16(*value),
            TreeValue::U32(value) => visitor.visit_u32(*value),
            TreeValue::U64(value) => visitor.visit_u64(*value),
            TreeValue::I8(value) => visitor.visit_i8(*value),
            TreeValue::I16(value) => visitor.visit_i16(*value),
            TreeValue::I32(value) => visitor.visit_i32(*value),
            TreeValue::I64(value) => visitor.visit_i64(*value),
            TreeValue::String(value) => visitor.visit_string(value.clone()),
            TreeValue::Struct(fields) => visitor.visit_map(TreeStructAccess {
                iter: fields.iter(),
                value: None,
            }),
            TreeValue::List(values) => visitor.visit_seq(TreeSeqAccess {
                iter: values.iter(),
            }),
        }
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::Bool(value) => visitor.visit_bool(*value),
            _ => Err(TreeSerdeError("expected bool".into())),
        }
    }

    fn deserialize_u8<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::U8(value) => visitor.visit_u8(*value),
            _ => Err(TreeSerdeError("expected u8".into())),
        }
    }

    fn deserialize_u16<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::U16(value) => visitor.visit_u16(*value),
            TreeValue::U8(value) => visitor.visit_u16(*value as u16),
            _ => Err(TreeSerdeError("expected u16".into())),
        }
    }

    fn deserialize_u32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::U32(value) => visitor.visit_u32(*value),
            TreeValue::U16(value) => visitor.visit_u32(*value as u32),
            TreeValue::U8(value) => visitor.visit_u32(*value as u32),
            _ => Err(TreeSerdeError("expected u32".into())),
        }
    }

    fn deserialize_u64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::U64(value) => visitor.visit_u64(*value),
            TreeValue::U32(value) => visitor.visit_u64(*value as u64),
            TreeValue::U16(value) => visitor.visit_u64(*value as u64),
            TreeValue::U8(value) => visitor.visit_u64(*value as u64),
            _ => Err(TreeSerdeError("expected u64".into())),
        }
    }

    fn deserialize_i8<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::I8(value) => visitor.visit_i8(*value),
            _ => Err(TreeSerdeError("expected i8".into())),
        }
    }

    fn deserialize_i16<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::I16(value) => visitor.visit_i16(*value),
            TreeValue::I8(value) => visitor.visit_i16(*value as i16),
            _ => Err(TreeSerdeError("expected i16".into())),
        }
    }

    fn deserialize_i32<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::I32(value) => visitor.visit_i32(*value),
            TreeValue::I16(value) => visitor.visit_i32(*value as i32),
            TreeValue::I8(value) => visitor.visit_i32(*value as i32),
            _ => Err(TreeSerdeError("expected i32".into())),
        }
    }

    fn deserialize_i64<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::I64(value) => visitor.visit_i64(*value),
            TreeValue::I32(value) => visitor.visit_i64(*value as i64),
            TreeValue::I16(value) => visitor.visit_i64(*value as i64),
            TreeValue::I8(value) => visitor.visit_i64(*value as i64),
            _ => Err(TreeSerdeError("expected i64".into())),
        }
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::String(value) => visitor.visit_str(value.as_str()),
            _ => Err(TreeSerdeError("expected string".into())),
        }
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::String(value) => visitor.visit_string(value.clone()),
            _ => Err(TreeSerdeError("expected string".into())),
        }
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::List(values) => visitor.visit_seq(TreeSeqAccess {
                iter: values.iter(),
            }),
            _ => Err(TreeSerdeError("expected list".into())),
        }
    }

    fn deserialize_tuple<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::Struct(fields) => visitor.visit_map(TreeStructAccess {
                iter: fields.iter(),
                value: None,
            }),
            _ => Err(TreeSerdeError("expected struct".into())),
        }
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_string(visitor)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }

    serde::forward_to_deserialize_any! {
        i128 u128 f32 f64 char bytes byte_buf option unit unit_struct map enum
    }
}

fn tree_value_from_serialize<T: Serialize>(value: &T) -> CoreResult<TreeValue> {
    value.serialize(TreeValueSerializer).map_err(|e| {
        Error::Serialization(format!(
            "Failed to encode external input into selection tree: {}",
            e
        ))
    })
}

fn typed_value_from_tree<T: DeserializeOwned>(value: &TreeValue) -> CoreResult<T> {
    T::deserialize(TreeValueDeserializer::new(value)).map_err(|e| {
        Error::Serialization(format!(
            "Failed to deserialize selected external input from selection tree: {}",
            e
        ))
    })
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

fn encode_leaf_bytes(value: &TreeValue) -> CoreResult<Vec<u8>> {
    let mut out = Vec::new();
    match value {
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
        TreeValue::Struct(_) | TreeValue::List(_) => {
            return Err(Error::Serialization(
                "Expected leaf value while encoding selection payload".into(),
            ))
        }
    }
    Ok(out)
}

fn subtree_payload_and_root(value: &TreeValue) -> CoreResult<(Vec<u8>, Vec<u8>)> {
    match value {
        TreeValue::Struct(fields) => {
            let mut payload = Vec::new();
            payload.push(0x01);
            push_u64(&mut payload, fields.len() as u64);

            let mut child_roots = Vec::with_capacity(fields.len());
            for (_, child) in fields {
                let (child_payload, child_root) = subtree_payload_and_root(child)?;
                push_u64(&mut payload, child_payload.len() as u64);
                payload.extend_from_slice(&child_payload);
                child_roots.push(child_root);
            }

            let mut parts: Vec<&[u8]> = Vec::with_capacity(child_roots.len() + 1);
            parts.push(b"struct");
            for root in &child_roots {
                parts.push(root.as_slice());
            }
            Ok((payload, selection_hash(&parts)))
        }
        TreeValue::List(values) => {
            let mut payload = Vec::new();
            payload.push(0x02);
            push_u64(&mut payload, values.len() as u64);

            let mut child_roots = Vec::with_capacity(values.len());
            for child in values {
                let (child_payload, child_root) = subtree_payload_and_root(child)?;
                push_u64(&mut payload, child_payload.len() as u64);
                payload.extend_from_slice(&child_payload);
                child_roots.push(child_root);
            }

            Ok((payload, list_root_from_hashes(&child_roots)))
        }
        _ => {
            let leaf_bytes = encode_leaf_bytes(value)?;
            let mut payload = Vec::with_capacity(1 + 8 + leaf_bytes.len());
            payload.push(0x00);
            push_u64(&mut payload, leaf_bytes.len() as u64);
            payload.extend_from_slice(&leaf_bytes);
            let root = selection_hash(&[b"leaf", leaf_bytes.as_slice()]);
            Ok((payload, root))
        }
    }
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

fn list_root_and_proof(
    hashes: &[Vec<u8>],
    index: usize,
) -> CoreResult<(Vec<u8>, Vec<ListProofSibling>)> {
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
            hash: level[sibling_index].clone(),
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

fn find_struct_field<'a>(entries: &'a [(String, TreeValue)], name: &str) -> Option<&'a TreeValue> {
    entries
        .iter()
        .find(|(field_name, _)| field_name == name)
        .map(|(_, value)| value)
}

struct ProvenSelection {
    selected_value: TreeValue,
    selected_bytes: Vec<u8>,
    root_hash: Vec<u8>,
    steps: Vec<SelectionProofStep>,
}

fn prove_selection(
    schema: &SchemaNode,
    value: &TreeValue,
    segments: &[SelectorSegment],
) -> CoreResult<ProvenSelection> {
    if segments.is_empty() {
        let (selected_bytes, root_hash) = subtree_payload_and_root(value)?;
        return Ok(ProvenSelection {
            selected_value: value.clone(),
            selected_bytes,
            root_hash,
            steps: Vec::new(),
        });
    }

    match (&segments[0], schema, value) {
        (
            SelectorSegment::Field(field_name),
            SchemaNode::Struct { fields, .. },
            TreeValue::Struct(entries),
        ) => {
            let target_index = fields
                .iter()
                .position(|field| field.name == *field_name)
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
                    siblings.push(sibling_root.clone());
                    child_roots.push(sibling_root);
                }
            }

            let mut parts: Vec<&[u8]> = Vec::with_capacity(child_roots.len() + 1);
            parts.push(b"struct");
            for root in &child_roots {
                parts.push(root.as_slice());
            }

            let mut steps = Vec::with_capacity(child.steps.len() + 1);
            steps.push(SelectionProofStep::Struct {
                field_index: target_index as u64,
                field_count: fields.len() as u64,
                siblings,
            });
            steps.extend(child.steps);

            Ok(ProvenSelection {
                selected_value: child.selected_value,
                selected_bytes: child.selected_bytes,
                root_hash: selection_hash(&parts),
                steps,
            })
        }
        (
            SelectorSegment::Index(index),
            SchemaNode::List { element, .. },
            TreeValue::List(values),
        ) => {
            let idx = *index as usize;
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
                index: *index,
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
        (SelectorSegment::Field(field_name), _, _) => Err(Error::Other(format!(
            "Selector field '{}' was not found in selected value",
            field_name
        ))),
        (SelectorSegment::Index(index), _, _) => Err(Error::Other(format!(
            "Selector index '{}' was not found in selected value",
            index
        ))),
    }
}

fn selected_payload_from_proven(
    selector: &SelectorPath,
    proven: ProvenSelection,
) -> SelectedPayload {
    SelectedPayload {
        bytes: proven.selected_bytes,
        proof: SelectionProof {
            path: selector.clone(),
            root_hash: proven.root_hash,
            steps: proven.steps,
        },
    }
}

fn dynamic_selected_payload<T: Serialize>(
    name: &str,
    value: &T,
    selector: &SelectorPath,
) -> CoreResult<SelectedPayload> {
    if !selector.is_empty() {
        return Err(Error::Other(format!(
            "External selector for '{}' requires typed_external<Root>(...) with postcard path inputs",
            name
        )));
    }

    let tree = tree_value_from_serialize(value)?;
    let proven = prove_selection(
        &SchemaNode::Leaf {
            type_name: "DynamicRoot".into(),
        },
        &tree,
        &[],
    )?;
    Ok(selected_payload_from_proven(selector, proven))
}

fn typed_proven_selection<Root: Serialize + Selectable>(
    value: &Root,
    selector: &SelectorPath,
) -> CoreResult<ProvenSelection> {
    let root_tree = tree_value_from_serialize(value)?;
    prove_selection(&Root::schema(), &root_tree, &selector.segments)
}

fn external_value_from_parts<T>(
    name: &str,
    selector: SelectorPath,
    resolved: ResolvedExternalData,
    selected: SelectedPayload,
    value: T,
) -> ExternalArg<T> {
    ExternalArg::new(
        name,
        selector,
        Some(resolved.commitment().to_string()),
        selected,
        value,
    )
}

pub fn resolve_external_value<T: DeserializeOwned + Serialize>(
    reference: ExternalSelection,
) -> CoreResult<ExternalArg<T>> {
    let storage = load_external_storage()?.ok_or_else(|| {
        Error::Other(
            "External input resolution requires CLI input context from --input and --input-manifest"
                .into(),
        )
    })?;

    let resolved = storage.resolve(reference.name())?;
    if reference.selector().is_empty() {
        let value = resolved.deserialize()?;
        let selected = dynamic_selected_payload(reference.name(), &value, reference.selector())?;
        return Ok(external_value_from_parts(
            reference.name(),
            reference.selector().clone(),
            resolved,
            selected,
            value,
        ));
    }

    Err(Error::Other(format!(
        "External selector for '{}' requires typed_external<Root>(...) with postcard path inputs",
        reference.name()
    )))
}

pub fn resolve_typed_external_value<Root, T>(
    reference: ExternalSelection,
) -> CoreResult<ExternalArg<T>>
where
    Root: DeserializeOwned + Serialize + Selectable,
    T: DeserializeOwned + Serialize,
{
    let storage = load_external_storage()?.ok_or_else(|| {
        Error::Other(
            "External input resolution requires CLI input context from --input and --input-manifest"
                .into(),
        )
    })?;

    let resolved = storage.resolve(reference.name())?;
    let root: Root = resolved.deserialize()?;
    let proven = typed_proven_selection(&root, reference.selector())?;

    let typed_selected = typed_value_from_tree::<T>(&proven.selected_value).map_err(|e| {
        Error::Serialization(format!(
            "Failed to deserialize selected external input '{}' from selection tree: {}",
            reference.name(),
            e
        ))
    })?;
    let selected = selected_payload_from_proven(reference.selector(), proven);

    Ok(external_value_from_parts(
        reference.name(),
        reference.selector().clone(),
        resolved,
        selected,
        typed_selected,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_storage::{sha256_hex, ExternalStorageManager};
    use raster_core::input::{verify_selection_proof, SchemaField, SchemaNode, Selectable};
    use serde::{Deserialize, Serialize};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::vec;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Flight {
        id: u32,
        seats: u16,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
    struct Address {
        lines: Vec<String>,
        indexes: Vec<u32>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
    struct PersonalData {
        age: usize,
        name: String,
        addresses: Vec<Address>,
    }

    impl Selectable for Address {
        fn schema() -> SchemaNode {
            SchemaNode::Struct {
                type_name: "Address".into(),
                fields: vec![
                    SchemaField::new("lines", "lines", <Vec<String> as Selectable>::schema()),
                    SchemaField::new("indexes", "indexes", <Vec<u32> as Selectable>::schema()),
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
                        <Vec<Address> as Selectable>::schema(),
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
        std::env::temp_dir().join(format!("raster-input-test-{}", nanos))
    }

    fn storage_manager(input_path: &Path, manifest_path: &Path) -> ExternalStorageManager {
        ExternalStorageManager::from_input_args(input_path.to_str(), manifest_path.to_str())
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

    fn leaf_payload(body: &[u8]) -> Vec<u8> {
        let mut out = vec![0x00];
        out.extend_from_slice(&(body.len() as u64).to_le_bytes());
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn resolves_file_backed_seed_through_external_value_path() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let bytes = raster_core::postcard::to_allocvec(&123u64).unwrap();
        fs::write(dir.join("seed.bin"), &bytes).unwrap();
        let hash = sha256_hex(&bytes);
        let (input_path, manifest_path) = write_external_documents(
            &dir,
            &hash,
            r#"{"seed":{"path":"seed.bin","load_preference":"read"}}"#,
            r#"{"seed":{"type":"sha256","commitment":"{hash}"}}"#,
        );

        let storage = storage_manager(&input_path, &manifest_path);
        let resolved = storage.resolve("seed").unwrap();
        let value: u64 = resolved.deserialize().unwrap();
        let selected = dynamic_selected_payload("seed", &value, &SelectorPath::default()).unwrap();

        assert_eq!(resolved.bytes(), bytes.as_slice());
        assert_eq!(resolved.commitment(), hash);
        assert_eq!(value, 123);
        assert_eq!(selected.bytes, leaf_payload(&123u64.to_le_bytes()));
        assert!(verify_selection_proof(&selected.bytes, &selected.proof));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn whole_value_dynamic_selection_produces_verifiable_payload() {
        let selected = dynamic_selected_payload("seed", &123u64, &SelectorPath::default()).unwrap();

        assert_eq!(selected.bytes, leaf_payload(&123u64.to_le_bytes()));
        assert!(selected.proof.path.is_empty());
        assert!(verify_selection_proof(&selected.bytes, &selected.proof));
    }

    #[test]
    fn resolve_external_value_errors_without_cli_context() {
        let err = resolve_external_value::<Flight>(ExternalSelection::new("flight_data"))
            .expect_err("missing CLI context should fail");

        assert_eq!(
            err.to_string(),
            "External input resolution requires CLI input context from --input and --input-manifest"
        );
    }

    #[test]
    fn resolves_typed_nested_selection_with_merkle_proof() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let data = PersonalData {
            age: 25,
            name: "John".to_string(),
            addresses: vec![Address {
                lines: vec!["221B Baker Street".to_string(), "Flat B".to_string()],
                indexes: vec![7, 42],
            }],
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

        let selected = SelectedPayload {
            bytes: proven.selected_bytes.clone(),
            proof: SelectionProof {
                path: selector,
                root_hash: proven.root_hash.clone(),
                steps: proven.steps.clone(),
            },
        };

        assert_eq!(
            typed_value_from_tree::<String>(&proven.selected_value).unwrap(),
            "Flat B"
        );
        assert!(verify_selection_proof(&selected.bytes, &selected.proof));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn whole_value_typed_selection_produces_verifiable_payload() {
        let root = PersonalData {
            age: 25,
            name: "John".to_string(),
            addresses: vec![Address {
                lines: vec!["221B Baker Street".to_string()],
                indexes: vec![7],
            }],
        };

        let selected = selected_payload_from_proven(
            &SelectorPath::default(),
            typed_proven_selection(&root, &SelectorPath::default()).unwrap(),
        );

        assert!(selected.proof.path.is_empty());
        assert!(verify_selection_proof(&selected.bytes, &selected.proof));
    }
}
