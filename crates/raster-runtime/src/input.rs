use raster_core::input::{
    selection_payload_hash, Hash32, InternalValue, ListProofDirection, ListProofSibling,
    SchemaNode, Selectable, SelectedPayload, SelectionCommitment, SelectionProof,
    SelectionProofStep, SelectionWitness, SelectorPath, SelectorSegment,
};
use raster_core::{Error, Result as CoreResult};
use serde::de::{
    self, DeserializeOwned, DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess, Visitor,
};
use serde::ser::{
    self, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
    SerializeTupleStruct, SerializeTupleVariant,
};
use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt;
use std::format;
use std::fs;
use std::path::Path;
use std::string::{String, ToString};
use std::vec::Vec;

use crate::raster_index::{RasterIndex, RasterNodeKind, RasterSelection, RasterSelectionLocation};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TreeValue {
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

struct TreeMapAccess<'de> {
    iter: std::slice::Iter<'de, (TreeValue, TreeValue)>,
    value: Option<&'de TreeValue>,
}

struct TreeEnumAccess<'de> {
    variant: &'de str,
    value: TreeEnumValue<'de>,
}

enum TreeEnumValue<'de> {
    Unit,
    Newtype(&'de TreeValue),
    Tuple(&'de [TreeValue]),
    Struct(&'de [(String, TreeValue)]),
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

impl<'de> MapAccess<'de> for TreeMapAccess<'de> {
    type Error = TreeSerdeError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: DeserializeSeed<'de>,
    {
        match self.iter.next() {
            Some((key, value)) => {
                self.value = Some(value);
                seed.deserialize(TreeValueDeserializer::new(key)).map(Some)
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
            .ok_or_else(|| TreeSerdeError("missing map value".into()))?;
        seed.deserialize(TreeValueDeserializer::new(value))
    }
}

impl<'de> de::EnumAccess<'de> for TreeEnumAccess<'de> {
    type Error = TreeSerdeError;
    type Variant = Self;

    fn variant_seed<V>(self, seed: V) -> Result<(V::Value, Self::Variant), Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        let variant = seed.deserialize(self.variant.into_deserializer())?;
        Ok((variant, self))
    }
}

impl<'de> de::VariantAccess<'de> for TreeEnumAccess<'de> {
    type Error = TreeSerdeError;

    fn unit_variant(self) -> Result<(), Self::Error> {
        match self.value {
            TreeEnumValue::Unit => Ok(()),
            _ => Err(TreeSerdeError("expected unit variant".into())),
        }
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        match self.value {
            TreeEnumValue::Newtype(value) => seed.deserialize(TreeValueDeserializer::new(value)),
            _ => Err(TreeSerdeError("expected newtype variant".into())),
        }
    }

    fn tuple_variant<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeEnumValue::Tuple(values) => visitor.visit_seq(TreeSeqAccess {
                iter: values.iter(),
            }),
            _ => Err(TreeSerdeError("expected tuple variant".into())),
        }
    }

    fn struct_variant<V>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeEnumValue::Struct(fields) => visitor.visit_map(TreeStructAccess {
                iter: fields.iter(),
                value: None,
            }),
            _ => Err(TreeSerdeError("expected struct variant".into())),
        }
    }
}

impl<'de> de::Deserializer<'de> for TreeValueDeserializer<'de> {
    type Error = TreeSerdeError;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::Unit => visitor.visit_unit(),
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
            TreeValue::Map(entries) => visitor.visit_map(TreeMapAccess {
                iter: entries.iter(),
                value: None,
            }),
            TreeValue::EnumUnit(variant) => visitor.visit_enum(TreeEnumAccess {
                variant,
                value: TreeEnumValue::Unit,
            }),
            TreeValue::EnumNewtype(variant, value) => visitor.visit_enum(TreeEnumAccess {
                variant,
                value: TreeEnumValue::Newtype(value.as_ref()),
            }),
            TreeValue::EnumTuple(variant, values) => visitor.visit_enum(TreeEnumAccess {
                variant,
                value: TreeEnumValue::Tuple(values.as_slice()),
            }),
            TreeValue::EnumStruct(variant, fields) => visitor.visit_enum(TreeEnumAccess {
                variant,
                value: TreeEnumValue::Struct(fields.as_slice()),
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

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::Unit => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::Unit => visitor.visit_unit(),
            _ => Err(TreeSerdeError("expected unit".into())),
        }
    }

    fn deserialize_unit_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_unit(visitor)
    }

    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::Map(entries) => visitor.visit_map(TreeMapAccess {
                iter: entries.iter(),
                value: None,
            }),
            _ => Err(TreeSerdeError("expected map".into())),
        }
    }

    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.value {
            TreeValue::EnumUnit(variant) => visitor.visit_enum(TreeEnumAccess {
                variant,
                value: TreeEnumValue::Unit,
            }),
            TreeValue::EnumNewtype(variant, value) => visitor.visit_enum(TreeEnumAccess {
                variant,
                value: TreeEnumValue::Newtype(value.as_ref()),
            }),
            TreeValue::EnumTuple(variant, values) => visitor.visit_enum(TreeEnumAccess {
                variant,
                value: TreeEnumValue::Tuple(values.as_slice()),
            }),
            TreeValue::EnumStruct(variant, fields) => visitor.visit_enum(TreeEnumAccess {
                variant,
                value: TreeEnumValue::Struct(fields.as_slice()),
            }),
            _ => Err(TreeSerdeError("expected enum".into())),
        }
    }

    serde::forward_to_deserialize_any! {
        i128 u128 f32 f64 char bytes byte_buf
    }
}

pub(crate) fn tree_value_from_serialize<T: Serialize>(value: &T) -> CoreResult<TreeValue> {
    value.serialize(TreeValueSerializer).map_err(|e| {
        Error::Serialization(format!(
            "Failed to encode external input into selection tree: {}",
            e
        ))
    })
}

pub(crate) fn typed_value_from_tree<T: DeserializeOwned>(value: &TreeValue) -> CoreResult<T> {
    T::deserialize(TreeValueDeserializer::new(value)).map_err(|e| {
        Error::Serialization(format!(
            "Failed to deserialize selected external input from selection tree: {}",
            e
        ))
    })
}

fn parse_leaf_value(type_name: &str, subtree_bytes: &[u8]) -> CoreResult<TreeValue> {
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

pub(crate) fn tree_value_from_raster_location(
    index: &RasterIndex,
    data_bytes: &[u8],
    selection: &RasterSelectionLocation,
) -> CoreResult<TreeValue> {
    tree_value_from_raster_node(index, data_bytes, selection.node_id)
}

fn tree_value_from_raster_node(
    index: &RasterIndex,
    data_bytes: &[u8],
    node_id: u64,
) -> CoreResult<TreeValue> {
    let node = index.get_node(node_id)?;
    match &node.kind {
        RasterNodeKind::Unit => Ok(TreeValue::Unit),
        RasterNodeKind::Leaf { type_name } => {
            let subtree = raster_subtree_bytes(data_bytes, node.offset, node.len)?;
            parse_leaf_value(type_name, subtree)
        }
        RasterNodeKind::Struct { fields } => {
            let mut values = Vec::with_capacity(fields.len());
            for field in fields {
                values.push((
                    field.name.clone(),
                    tree_value_from_raster_node(index, data_bytes, field.child)?,
                ));
            }
            Ok(TreeValue::Struct(values))
        }
        RasterNodeKind::List { elements, .. } => {
            let mut values = Vec::with_capacity(elements.len());
            for child in elements {
                values.push(tree_value_from_raster_node(index, data_bytes, *child)?);
            }
            Ok(TreeValue::List(values))
        }
        RasterNodeKind::Map { entries } => {
            let mut values = Vec::with_capacity(entries.len());
            for entry in entries {
                values.push((
                    tree_value_from_raster_node(index, data_bytes, entry.key)?,
                    tree_value_from_raster_node(index, data_bytes, entry.value)?,
                ));
            }
            Ok(TreeValue::Map(values))
        }
        RasterNodeKind::EnumUnit { variant } => Ok(TreeValue::EnumUnit(variant.clone())),
        RasterNodeKind::EnumNewtype { variant, child } => Ok(TreeValue::EnumNewtype(
            variant.clone(),
            Box::new(tree_value_from_raster_node(index, data_bytes, *child)?),
        )),
        RasterNodeKind::EnumTuple { variant, elements } => {
            let mut values = Vec::with_capacity(elements.len());
            for child in elements {
                values.push(tree_value_from_raster_node(index, data_bytes, *child)?);
            }
            Ok(TreeValue::EnumTuple(variant.clone(), values))
        }
        RasterNodeKind::EnumStruct { variant, fields } => {
            let mut values = Vec::with_capacity(fields.len());
            for field in fields {
                values.push((
                    field.name.clone(),
                    tree_value_from_raster_node(index, data_bytes, field.child)?,
                ));
            }
            Ok(TreeValue::EnumStruct(variant.clone(), values))
        }
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
        | TreeValue::Map(_)
        | TreeValue::EnumUnit(_)
        | TreeValue::EnumNewtype(_, _)
        | TreeValue::EnumTuple(_, _)
        | TreeValue::EnumStruct(_, _) => {
            return Err(Error::Serialization(
                "Expected leaf value while encoding selection payload".into(),
            ))
        }
    }
    Ok(out)
}

/// Direct children of a `TreeValue`, in the order their payloads/roots are laid
/// out by [`assemble_subtree`]. Used to drive an explicit-stack post-order
/// traversal instead of recursing (which overflows the stack on deeply nested
/// recur-sequence values).
fn subtree_children(value: &TreeValue) -> Vec<&TreeValue> {
    match value {
        TreeValue::Struct(fields) => fields.iter().map(|(_, child)| child).collect(),
        TreeValue::List(values) => values.iter().collect(),
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
fn assemble_subtree(
    value: &TreeValue,
    children: Vec<(Vec<u8>, Hash32)>,
) -> CoreResult<(Vec<u8>, Hash32)> {
    let result = match value {
        TreeValue::Unit => (vec![0x03], selection_hash(&[b"unit"])),
        TreeValue::Struct(_) => {
            let mut payload = Vec::new();
            payload.push(0x01);
            push_u64(&mut payload, children.len() as u64);
            for (child_payload, _) in &children {
                push_u64(&mut payload, child_payload.len() as u64);
                payload.extend_from_slice(child_payload);
            }

            let mut parts: Vec<&[u8]> = Vec::with_capacity(children.len() + 1);
            parts.push(b"struct");
            for (_, child_root) in &children {
                parts.push(child_root.as_slice());
            }
            (payload, selection_hash(&parts))
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

pub(crate) fn subtree_payload_and_root(root: &TreeValue) -> CoreResult<(Vec<u8>, Hash32)> {
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

fn list_root_from_hashes(hashes: &[Hash32]) -> Hash32 {
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
                    siblings.push(sibling_root);
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
        },
    }
}

pub(crate) fn selected_payload_from_raster_location(
    data_bytes: &[u8],
    selector: &SelectorPath,
    selection: RasterSelectionLocation,
) -> CoreResult<SelectedPayload> {
    let bytes = raster_subtree_bytes(data_bytes, selection.offset, selection.len)?.to_vec();
    let selected_hash = selection_payload_hash(&bytes);
    let selected_len = bytes.len() as u64;
    Ok(SelectedPayload {
        bytes,
        commitment: SelectionCommitment {
            path: selector.clone(),
            source_root_hash: selection.root_hash,
            selected_hash,
            selected_len,
        },
    })
}

pub(crate) fn selection_witness_from_raster_selection(
    data_bytes: &[u8],
    selector: &SelectorPath,
    selection: RasterSelection,
) -> CoreResult<SelectionWitness> {
    Ok(SelectionWitness {
        bytes: raster_subtree_bytes(data_bytes, selection.offset, selection.len)?.to_vec(),
        proof: SelectionProof {
            path: selector.clone(),
            root_hash: selection.root_hash,
            steps: selection.steps,
        },
    })
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

pub fn select_internal_value<Root, T>(
    value: &InternalValue<Root>,
    selector: &SelectorPath,
) -> CoreResult<InternalValue<T>>
where
    Root: DeserializeOwned + Serialize + Selectable,
    T: DeserializeOwned + Serialize,
{
    let proven = typed_proven_selection(&value.value, selector)?;
    let typed_selected = typed_value_from_tree::<T>(&proven.selected_value).map_err(|e| {
        Error::Serialization(format!(
            "Failed to deserialize selected internal input from selection tree: {}",
            e
        ))
    })?;
    let full_selector = extend_selector_path(&value.selector, selector);
    let selected_hash = selection_payload_hash(&proven.selected_bytes);
    let selected_len = proven.selected_bytes.len() as u64;
    Ok(InternalValue::new_with_selection(
        value.reference.clone(),
        proven.selected_bytes,
        full_selector.clone(),
        SelectionCommitment {
            path: full_selector,
            source_root_hash: value.selection.source_root_hash.clone(),
            selected_hash,
            selected_len,
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
struct RasterChildPlan<'a> {
    value: &'a TreeValue,
    node_offset: u64,
}

/// In-progress node on the explicit build stack (replaces a recursive frame).
struct RasterFrame<'a> {
    value: &'a TreeValue,
    node_id: u64,
    root_hash: Hash32,
    children: RasterChildren<'a>,
    next: usize,
    /// Node ids of children, accumulated in `children` order as they complete.
    child_ids: Vec<u64>,
}

struct RasterChildren<'a> {
    plans: Vec<RasterChildPlan<'a>>,
    /// Child root hashes in element order (only consumed by `List` nodes).
    hashes: Vec<Hash32>,
}

/// Compute the ordered children of `value` together with the byte offset each
/// child node occupies inside `value`'s payload. The offset arithmetic and the
/// `Map` ordering match the previous recursive implementation exactly.
fn prepare_raster_children<'a>(
    value: &'a TreeValue,
    offset: u64,
) -> CoreResult<RasterChildren<'a>> {
    let mut plans = Vec::new();
    let mut hashes = Vec::new();
    match value {
        TreeValue::Struct(fields) => {
            let mut child_offset = offset + 1 + 8;
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
fn finalize_raster_kind(
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
        TreeValue::List(values) => RasterNodeKind::List {
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
fn enter_raster_frame<'a>(
    nodes: &mut Vec<crate::raster_index::RasterNode>,
    value: &'a TreeValue,
    offset: u64,
) -> CoreResult<RasterFrame<'a>> {
    use crate::raster_index::{RasterNode, RasterNodeKind};

    let (payload, root_hash) = subtree_payload_and_root(value)?;
    let node_id = nodes.len() as u64;
    nodes.push(RasterNode {
        offset,
        len: payload.len() as u64,
        root_hash,
        kind: RasterNodeKind::Unit,
    });
    let children = prepare_raster_children(value, offset)?;
    Ok(RasterFrame {
        value,
        node_id,
        root_hash,
        children,
        next: 0,
        child_ids: Vec::new(),
    })
}

fn build_raster_index_node(
    nodes: &mut Vec<crate::raster_index::RasterNode>,
    root_value: &TreeValue,
    root_offset: u64,
) -> CoreResult<(u64, Hash32)> {
    // Iterative pre-order build with an explicit heap stack. Node ids are still
    // assigned in pre-order (parent before its children, children left to
    // right), so the on-disk layout is unchanged; only the call stack is gone.
    let root_frame = enter_raster_frame(nodes, root_value, root_offset)?;
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
                Some(plan)
            } else {
                None
            }
        };

        match next_child {
            Some(plan) => {
                let child_frame = enter_raster_frame(nodes, plan.value, plan.node_offset)?;
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
    let (payload, root_hash) = subtree_payload_and_root(&tree)?;
    let mut nodes = Vec::new();
    let root_node = build_raster_index_node(&mut nodes, &tree, 0)?.0;
    let index = RasterIndex::new(root_node, root_hash.clone(), nodes);
    Ok((payload, index.encode()?, hex_string(&root_hash)))
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

/// Compute the manifest commitment for a Postcard-encoded entry argument:
/// the same selection-tree structural root `bind_entry_arguments`/
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
    use crate::external_storage::{sha256_hex, ExternalStorageManager};
    use crate::raster_index::RasterIndex;
    use raster_core::input::{verify_selection_proof, SchemaField, SchemaNode, Selectable};
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
        lines: Vec<String>,
        indexes: Vec<u32>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
    struct PersonalData {
        age: usize,
        name: String,
        addresses: Vec<Address>,
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
        let counter = UNIQUE_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("raster-input-test-{}-{}", nanos, counter))
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

        let witness = SelectionWitness {
            bytes: proven.selected_bytes.clone(),
            proof: SelectionProof {
                path: selector,
                root_hash: proven.root_hash,
                steps: proven.steps.clone(),
            },
        };

        assert_eq!(
            typed_value_from_tree::<String>(&proven.selected_value).unwrap(),
            "Flat B"
        );
        assert!(verify_selection_proof(&witness.bytes, &witness.proof));
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

        assert!(selected.commitment.path.is_empty());
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
        let tree = tree_value_from_raster_node(&index, &data_bytes, selection.node_id).unwrap();
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
            },
        );
        let witness = SelectionWitness {
            bytes: selected.bytes.clone(),
            proof: SelectionProof {
                path: SelectorPath::default(),
                root_hash: selection.root_hash,
                steps: Vec::new(),
            },
        };

        assert_eq!(decoded, value);
        assert!(verify_selection_proof(&witness.bytes, &witness.proof));
    }
}
