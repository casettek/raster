//! Raster collection vocabulary: [`List`], [`Block`], [`Bytes`], [`BytesPage`],
//! and the [`Materializable`] marker trait.
//!
//! `Vec` is deliberately **not** a Rastered type. Its two properties —
//! *selectability* (reach into a value while it stays in committed storage) and
//! *materializability* (bring the value out whole into one replay unit) — are
//! split across two collection types so that unbounded materialization is
//! unrepresentable rather than merely forbidden:
//!
//! - [`List<T>`] — the unbounded collection. `Selectable`, **not**
//!   `Materializable`. It is referenced (selection source, recur input, draft
//!   target); it never crosses a tile boundary whole.
//! - [`Block<T>`] — a bounded window into a list. `Selectable` **and**
//!   `Materializable` — the only collection type that may be materialized into a
//!   tile. Constructed by the framework from operations whose size bound is
//!   pinned as a literal in the CFS (range `select!`, `chunk = N`), or by tile
//!   code via [`Block::build`].

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::{Deref, DerefMut};
use serde::{Deserialize, Serialize};

use crate::input::{SchemaField, SchemaNode, Selectable};
use crate::{Error, Result};

/// A value bounded enough to be materialized whole into one replay unit.
///
/// The dual of [`Selectable`]: `Selectable` reaches *into* a value that stays in
/// committed storage; `Materializable` permits the value to *leave* storage
/// whole. Scalars, `String`, `Block<T>`, and derived Rastered structs implement
/// it. `List<T>` (and `Vec<T>`) deliberately do not — a collection is iterated,
/// not passed whole.
///
/// The trait has no methods; it is pure evidence, checked by the `#[tile]` macro
/// at every argument and return position.
#[cfg_attr(
    not(doc),
    diagnostic::on_unimplemented(
        message = "`{Self}` cannot be materialized into a tile: collections are iterated, not passed whole",
        label = "not materializable",
        note = "make the collection the `input` of `call_recur!` (one element per step, or `chunk = N` blocks)",
        note = "for a bounded slice, use `select!` with a literal range `xs[a..b]` — it yields a `Block<T>`"
    )
)]
pub trait Materializable {}

macro_rules! impl_materializable_scalar {
    ($($ty:ty),* $(,)?) => { $(impl Materializable for $ty {})* };
}
impl_materializable_scalar!(
    (),
    bool,
    char,
    String,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
);

impl<T: Materializable> Materializable for Option<T> {}
impl<T: Materializable, const N: usize> Materializable for [T; N] {}

macro_rules! impl_materializable_tuple {
    ($($name:ident),+) => {
        impl<$($name: Materializable),+> Materializable for ($($name,)+) {}
    };
}
impl_materializable_tuple!(A);
impl_materializable_tuple!(A, B);
impl_materializable_tuple!(A, B, C);
impl_materializable_tuple!(A, B, C, D);
impl_materializable_tuple!(A, B, C, D, E);
impl_materializable_tuple!(A, B, C, D, E, F);

/// Sentinel container name emitted for a [`List<T>`] via
/// `serialize_newtype_struct` / `deserialize_newtype_struct`. Transparent
/// serializers (postcard, serde_json) ignore the name and treat a `List` exactly
/// like the `Vec` it wraps, so wire and JSON bytes are unchanged. The
/// selection-tree serializer, however, keys on this name to encode a `List`
/// **field** of a struct as a compact `(root, len)` handle (payload node `0x09`)
/// rather than inlining every element — the Phase 2 chunked-commitment layout
/// (see `docs/proposals/bounded-collections.md`). The `$` prefix keeps it from
/// colliding with any user newtype name.
pub const LIST_HANDLE_NEWTYPE_NAME: &str = "$raster::ListHandle";

/// An unbounded, storage-resident collection.
///
/// `List<T>` is `Selectable` but not `Materializable`: it can be reached into
/// (`select!` an element, a range, the whole reference) and iterated
/// (`call_recur!`), but it can never be materialized whole into a tile. On the
/// wire it is transparent to `Vec<T>` for postcard/JSON; the selection-tree
/// serializer keys on [`LIST_HANDLE_NEWTYPE_NAME`] to store a `List` struct
/// field as a `(root, len)` handle (payload node `0x09`) instead of inlining
/// elements.
#[derive(Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename = "$raster::ListHandle")]
pub struct List<T>(Vec<T>);

impl<T: core::fmt::Debug> core::fmt::Debug for List<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Transparent to the underlying slice: a `List` prints like its elements,
        // matching the `Vec` it replaces.
        self.0.fmt(f)
    }
}

impl<T> List<T> {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn into_vec(self) -> Vec<T> {
        self.0
    }

    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
}

impl<T> Deref for List<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for List<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> From<Vec<T>> for List<T> {
    fn from(items: Vec<T>) -> Self {
        Self(items)
    }
}

impl<T> From<List<T>> for Vec<T> {
    fn from(list: List<T>) -> Self {
        list.0
    }
}

impl<T> FromIterator<T> for List<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self(Vec::from_iter(iter))
    }
}

impl<T> IntoIterator for List<T> {
    type Item = T;
    type IntoIter = alloc::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a List<T> {
    type Item = &'a T;
    type IntoIter = core::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<T> Selectable for List<T>
where
    T: Selectable,
{
    fn schema() -> SchemaNode {
        SchemaNode::List {
            type_name: "List".into(),
            element: Box::new(T::schema()),
        }
    }
}

/// A bounded window of elements, materializable into one replay unit.
///
/// `Block<T>` is the only collection type that may cross a tile boundary. It is
/// constructed by the framework from operations whose size bound is pinned in the
/// CFS — a literal-range `select!` or a `chunk = N` recur driver — or by tile
/// code via [`Block::build`]. On the wire it is transparent to `Vec<T>`.
#[derive(Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Block<T>(Vec<T>);

impl<T: core::fmt::Debug> core::fmt::Debug for Block<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

impl<T> Block<T> {
    /// Framework constructor used by generated `select!` / recur code, where the
    /// size bound is already pinned in the CFS. Not part of the authored surface.
    #[doc(hidden)]
    pub fn __from_selection(items: Vec<T>) -> Self {
        Self(items)
    }

    /// Tile-side constructor: build a bounded window from within a tile body
    /// (e.g. a tile that produces a small collection as its output).
    ///
    /// This is legal only inside a tile — the sequence grammar has no expression
    /// position that could construct one as authorized dataflow, and a
    /// sequence-level call surfaces as an unauthenticated `external` binding in
    /// the CFS. (A runtime tile-scope assertion is a planned defense-in-depth
    /// hardening; see docs/proposals/bounded-collections.md enforcement point 5.)
    pub fn build(items: Vec<T>) -> Self {
        Self(items)
    }

    pub fn into_vec(self) -> Vec<T> {
        self.0
    }

    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    pub fn iter(&self) -> core::slice::Iter<'_, T> {
        self.0.iter()
    }
}

impl<T> Deref for Block<T> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> IntoIterator for Block<T> {
    type Item = T;
    type IntoIter = alloc::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a Block<T> {
    type Item = &'a T;
    type IntoIter = core::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<T> Materializable for Block<T> where T: Materializable {}

impl<T> Selectable for Block<T>
where
    T: Selectable,
{
    fn schema() -> SchemaNode {
        SchemaNode::List {
            type_name: "Block".into(),
            element: Box::new(T::schema()),
        }
    }
}

/// Sentinel name the selection-tree serializer keys on to emit payload tag
/// `0x0B` instead of an ordinary struct. Postcard/JSON ignore the name.
pub const BYTES_PAGE_NEWTYPE_NAME: &str = "$raster::BytesPage";

/// The four fields of a page, pulled out of [`BytesPageWire`] without building
/// a generic value tree. See [`bytes_page_parts`].
pub struct BytesPageParts {
    pub index: u64,
    pub offset: u64,
    pub len: u64,
    pub bytes: Vec<u8>,
}

/// Extract a page's fields from the value behind a [`BYTES_PAGE_NEWTYPE_NAME`]
/// newtype, for serializers that build a value tree.
///
/// **Why this is not a plain `value.serialize(TheTreeSerializer)`.** The tree
/// serializers learn a value is a page from the newtype *name*, which arrives as
/// a parameter of `serialize_newtype_struct` — before the inner value is
/// serialized. Running the general-purpose serializer first and pattern-matching
/// the result afterwards is what the `List<T>` handle does one arm up, and there
/// it is free: `ListHandle(values)` re-wraps the very `Vec` that had to be built
/// anyway. For a page it is not free. The wire struct's `Vec<u8>` payload goes
/// through `serialize_seq`, so the intermediate is one 56-byte enum node per
/// byte — 14.7 MB for a 256 KiB page — built, walked once, and discarded.
///
/// This serializer knows the shape statically, so it collects the payload
/// straight into a `Vec<u8>`. It is deliberately narrow: everything that is not
/// the page wire struct is an error, which keeps it useless as a general
/// bytes-into-the-tree channel (`serialize_bytes` on the real serializers still
/// refuses raw bytes — that is what keeps byte data off the `List<u8>` path).
pub fn bytes_page_parts<T>(value: &T) -> core::result::Result<BytesPageParts, String>
where
    T: ?Sized + Serialize,
{
    value.serialize(PageSerializer).map_err(|e| e.0)
}

#[derive(Debug)]
struct PageSerdeError(String);

impl core::fmt::Display for PageSerdeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::error::Error for PageSerdeError {}

impl serde::ser::Error for PageSerdeError {
    fn custom<T: core::fmt::Display>(msg: T) -> Self {
        Self(alloc::format!("{msg}"))
    }
}

type PageResult<T> = core::result::Result<T, PageSerdeError>;

fn page_unsupported<T>(what: &str) -> PageResult<T> {
    Err(PageSerdeError(alloc::format!(
        "a `{BYTES_PAGE_NEWTYPE_NAME}` newtype must wrap the page wire struct, not {what}"
    )))
}

/// Accepts only `serialize_struct` — the wire struct — and rejects everything else.
struct PageSerializer;

/// Accepts only `serialize_u64`-ish (the coordinates) and `serialize_seq` /
/// `serialize_bytes` (the payload).
struct PageFieldSerializer;

enum PageField {
    Int(u64),
    Bytes(Vec<u8>),
}

/// Collects a `Vec<u8>` payload into a flat buffer instead of a node per byte.
struct PageBytesSerializer {
    buf: Vec<u8>,
}

/// Accepts only `serialize_u8` — one payload element.
struct PageByteSerializer;

macro_rules! page_reject {
    ($($method:ident($($arg:ty),*) -> $what:literal;)*) => {
        $(fn $method(self, $(_: $arg),*) -> PageResult<Self::Ok> {
            page_unsupported($what)
        })*
    };
}

impl serde::Serializer for PageSerializer {
    type Ok = BytesPageParts;
    type Error = PageSerdeError;
    type SerializeSeq = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeTuple = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeTupleStruct = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeTupleVariant = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeMap = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeStruct = PageStructSerializer;
    type SerializeStructVariant = serde::ser::Impossible<Self::Ok, Self::Error>;

    fn serialize_struct(self, _name: &'static str, _len: usize) -> PageResult<PageStructSerializer> {
        Ok(PageStructSerializer {
            index: None,
            offset: None,
            len: None,
            bytes: None,
        })
    }

    page_reject! {
        serialize_bool(bool) -> "a bool";
        serialize_i8(i8) -> "an integer";
        serialize_i16(i16) -> "an integer";
        serialize_i32(i32) -> "an integer";
        serialize_i64(i64) -> "an integer";
        serialize_u8(u8) -> "an integer";
        serialize_u16(u16) -> "an integer";
        serialize_u32(u32) -> "an integer";
        serialize_u64(u64) -> "an integer";
        serialize_f32(f32) -> "a float";
        serialize_f64(f64) -> "a float";
        serialize_char(char) -> "a char";
        serialize_str(&str) -> "a string";
        serialize_bytes(&[u8]) -> "raw bytes";
        serialize_unit() -> "a unit";
    }

    fn serialize_none(self) -> PageResult<Self::Ok> {
        page_unsupported("an option")
    }
    fn serialize_some<T>(self, _value: &T) -> PageResult<Self::Ok>
    where
        T: ?Sized + Serialize,
    {
        page_unsupported("an option")
    }
    fn serialize_unit_struct(self, _name: &'static str) -> PageResult<Self::Ok> {
        page_unsupported("a unit struct")
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
    ) -> PageResult<Self::Ok> {
        page_unsupported("an enum")
    }
    fn serialize_newtype_struct<T>(self, _name: &'static str, value: &T) -> PageResult<Self::Ok>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> PageResult<Self::Ok>
    where
        T: ?Sized + Serialize,
    {
        page_unsupported("an enum")
    }
    fn serialize_seq(self, _len: Option<usize>) -> PageResult<Self::SerializeSeq> {
        page_unsupported("a sequence")
    }
    fn serialize_tuple(self, _len: usize) -> PageResult<Self::SerializeTuple> {
        page_unsupported("a tuple")
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> PageResult<Self::SerializeTupleStruct> {
        page_unsupported("a tuple struct")
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> PageResult<Self::SerializeTupleVariant> {
        page_unsupported("an enum")
    }
    fn serialize_map(self, _len: Option<usize>) -> PageResult<Self::SerializeMap> {
        page_unsupported("a map")
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> PageResult<Self::SerializeStructVariant> {
        page_unsupported("an enum")
    }
}

struct PageStructSerializer {
    index: Option<u64>,
    offset: Option<u64>,
    len: Option<u64>,
    bytes: Option<Vec<u8>>,
}

impl serde::ser::SerializeStruct for PageStructSerializer {
    type Ok = BytesPageParts;
    type Error = PageSerdeError;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> PageResult<()>
    where
        T: ?Sized + Serialize,
    {
        let field = value.serialize(PageFieldSerializer)?;
        match (key, field) {
            ("index", PageField::Int(n)) => self.index = Some(n),
            ("offset", PageField::Int(n)) => self.offset = Some(n),
            ("len", PageField::Int(n)) => self.len = Some(n),
            ("bytes", PageField::Bytes(b)) => self.bytes = Some(b),
            (key, _) => {
                return Err(PageSerdeError(alloc::format!(
                    "unexpected type for bytes-page field `{key}`"
                )))
            }
        }
        Ok(())
    }

    fn end(self) -> PageResult<BytesPageParts> {
        let (Some(index), Some(offset), Some(len), Some(bytes)) =
            (self.index, self.offset, self.len, self.bytes)
        else {
            return Err(PageSerdeError(
                "bytes page is missing index, offset, len, or bytes".into(),
            ));
        };
        Ok(BytesPageParts {
            index,
            offset,
            len,
            bytes,
        })
    }
}

impl serde::Serializer for PageFieldSerializer {
    type Ok = PageField;
    type Error = PageSerdeError;
    type SerializeSeq = PageBytesSerializer;
    type SerializeTuple = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeTupleStruct = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeTupleVariant = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeMap = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeStruct = serde::ser::Impossible<Self::Ok, Self::Error>;
    type SerializeStructVariant = serde::ser::Impossible<Self::Ok, Self::Error>;

    fn serialize_u8(self, v: u8) -> PageResult<PageField> {
        Ok(PageField::Int(u64::from(v)))
    }
    fn serialize_u16(self, v: u16) -> PageResult<PageField> {
        Ok(PageField::Int(u64::from(v)))
    }
    fn serialize_u32(self, v: u32) -> PageResult<PageField> {
        Ok(PageField::Int(u64::from(v)))
    }
    fn serialize_u64(self, v: u64) -> PageResult<PageField> {
        Ok(PageField::Int(v))
    }

    /// The payload as a `Vec<u8>`: collected flat, one buffer, no node per byte.
    fn serialize_seq(self, len: Option<usize>) -> PageResult<PageBytesSerializer> {
        Ok(PageBytesSerializer {
            buf: Vec::with_capacity(len.unwrap_or(0)),
        })
    }

    /// The payload if the wire struct ever switches to `serialize_bytes`. Safe
    /// to accept here — this serializer is only ever reached from inside a page.
    fn serialize_bytes(self, v: &[u8]) -> PageResult<PageField> {
        Ok(PageField::Bytes(v.to_vec()))
    }

    page_reject! {
        serialize_bool(bool) -> "a bool";
        serialize_i8(i8) -> "a signed integer";
        serialize_i16(i16) -> "a signed integer";
        serialize_i32(i32) -> "a signed integer";
        serialize_i64(i64) -> "a signed integer";
        serialize_f32(f32) -> "a float";
        serialize_f64(f64) -> "a float";
        serialize_char(char) -> "a char";
        serialize_str(&str) -> "a string";
        serialize_unit() -> "a unit";
    }

    fn serialize_none(self) -> PageResult<Self::Ok> {
        page_unsupported("an option")
    }
    fn serialize_some<T>(self, _value: &T) -> PageResult<Self::Ok>
    where
        T: ?Sized + Serialize,
    {
        page_unsupported("an option")
    }
    fn serialize_unit_struct(self, _name: &'static str) -> PageResult<Self::Ok> {
        page_unsupported("a unit struct")
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
    ) -> PageResult<Self::Ok> {
        page_unsupported("an enum")
    }
    fn serialize_newtype_struct<T>(self, _name: &'static str, value: &T) -> PageResult<Self::Ok>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> PageResult<Self::Ok>
    where
        T: ?Sized + Serialize,
    {
        page_unsupported("an enum")
    }
    fn serialize_tuple(self, _len: usize) -> PageResult<Self::SerializeTuple> {
        page_unsupported("a tuple")
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> PageResult<Self::SerializeTupleStruct> {
        page_unsupported("a tuple struct")
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> PageResult<Self::SerializeTupleVariant> {
        page_unsupported("an enum")
    }
    fn serialize_map(self, _len: Option<usize>) -> PageResult<Self::SerializeMap> {
        page_unsupported("a map")
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> PageResult<Self::SerializeStruct> {
        page_unsupported("a nested struct")
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> PageResult<Self::SerializeStructVariant> {
        page_unsupported("an enum")
    }
}

impl serde::ser::SerializeSeq for PageBytesSerializer {
    type Ok = PageField;
    type Error = PageSerdeError;

    fn serialize_element<T>(&mut self, value: &T) -> PageResult<()>
    where
        T: ?Sized + Serialize,
    {
        self.buf.push(value.serialize(PageByteSerializer)?);
        Ok(())
    }

    fn end(self) -> PageResult<PageField> {
        Ok(PageField::Bytes(self.buf))
    }
}

impl serde::Serializer for PageByteSerializer {
    type Ok = u8;
    type Error = PageSerdeError;
    type SerializeSeq = serde::ser::Impossible<u8, PageSerdeError>;
    type SerializeTuple = serde::ser::Impossible<u8, PageSerdeError>;
    type SerializeTupleStruct = serde::ser::Impossible<u8, PageSerdeError>;
    type SerializeTupleVariant = serde::ser::Impossible<u8, PageSerdeError>;
    type SerializeMap = serde::ser::Impossible<u8, PageSerdeError>;
    type SerializeStruct = serde::ser::Impossible<u8, PageSerdeError>;
    type SerializeStructVariant = serde::ser::Impossible<u8, PageSerdeError>;

    fn serialize_u8(self, v: u8) -> PageResult<u8> {
        Ok(v)
    }

    page_reject! {
        serialize_bool(bool) -> "a bool";
        serialize_i8(i8) -> "a non-u8 payload element";
        serialize_i16(i16) -> "a non-u8 payload element";
        serialize_i32(i32) -> "a non-u8 payload element";
        serialize_i64(i64) -> "a non-u8 payload element";
        serialize_u16(u16) -> "a non-u8 payload element";
        serialize_u32(u32) -> "a non-u8 payload element";
        serialize_u64(u64) -> "a non-u8 payload element";
        serialize_f32(f32) -> "a float";
        serialize_f64(f64) -> "a float";
        serialize_char(char) -> "a char";
        serialize_str(&str) -> "a string";
        serialize_bytes(&[u8]) -> "a nested byte string";
        serialize_unit() -> "a unit";
    }

    fn serialize_none(self) -> PageResult<u8> {
        page_unsupported("an option")
    }
    fn serialize_some<T>(self, _value: &T) -> PageResult<u8>
    where
        T: ?Sized + Serialize,
    {
        page_unsupported("an option")
    }
    fn serialize_unit_struct(self, _name: &'static str) -> PageResult<u8> {
        page_unsupported("a unit struct")
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
    ) -> PageResult<u8> {
        page_unsupported("an enum")
    }
    fn serialize_newtype_struct<T>(self, _name: &'static str, value: &T) -> PageResult<u8>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> PageResult<u8>
    where
        T: ?Sized + Serialize,
    {
        page_unsupported("an enum")
    }
    fn serialize_seq(self, _len: Option<usize>) -> PageResult<Self::SerializeSeq> {
        page_unsupported("a nested sequence")
    }
    fn serialize_tuple(self, _len: usize) -> PageResult<Self::SerializeTuple> {
        page_unsupported("a tuple")
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> PageResult<Self::SerializeTupleStruct> {
        page_unsupported("a tuple struct")
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> PageResult<Self::SerializeTupleVariant> {
        page_unsupported("an enum")
    }
    fn serialize_map(self, _len: Option<usize>) -> PageResult<Self::SerializeMap> {
        page_unsupported("a map")
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> PageResult<Self::SerializeStruct> {
        page_unsupported("a struct")
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> PageResult<Self::SerializeStructVariant> {
        page_unsupported("an enum")
    }
}

/// Schema for `Bytes<PAGE_SIZE>` — one construction so the derive, the
/// compiler's AST walk, and `Selectable for Bytes<P>` cannot drift.
pub fn bytes_schema(page_size: u64) -> SchemaNode {
    SchemaNode::Struct {
        type_name: alloc::format!("$raster::Bytes<{page_size}>"),
        fields: alloc::vec![
            SchemaField::new("byte_len", "byte_len", u64::schema()),
            SchemaField::new("page_size", "page_size", u64::schema()),
            SchemaField::new("pages", "pages", List::<BytesPage>::schema()),
        ],
    }
}

/// Expected length of page `index` in a region of `byte_len` paged at `page_size`.
pub fn expected_page_len(byte_len: u64, page_size: u64, index: u64) -> u64 {
    let offset = index.saturating_mul(page_size.max(1));
    core::cmp::min(page_size.max(1), byte_len.saturating_sub(offset))
}

/// `⌈byte_len / page_size⌉`, the page count a region implies.
pub fn page_count(byte_len: u64, page_size: u64) -> u64 {
    if page_size == 0 {
        return 0;
    }
    byte_len.div_ceil(page_size)
}

/// One page of a [`Bytes`] region. `Materializable` and terminal — the only
/// byte value that may cross a tile boundary.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct BytesPage {
    index: u64,
    offset: u64,
    len: u64,
    bytes: Vec<u8>,
}

impl BytesPage {
    /// Build a page from its committed coordinates and payload.
    ///
    /// `len` is taken from `bytes.len()`. Framework and encoder use this;
    /// authored tile code has no public constructor.
    #[doc(hidden)]
    pub fn __from_parts(index: u64, offset: u64, bytes: Vec<u8>) -> Self {
        let len = bytes.len() as u64;
        Self {
            index,
            offset,
            len,
            bytes,
        }
    }

    pub fn index(&self) -> u64 {
        self.index
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
}

impl core::fmt::Debug for BytesPage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BytesPage")
            .field("index", &self.index)
            .field("offset", &self.offset)
            .field("len", &self.len)
            .finish()
    }
}

impl Materializable for BytesPage {}

impl Selectable for BytesPage {
    fn schema() -> SchemaNode {
        SchemaNode::Leaf {
            type_name: "BytesPage".into(),
        }
    }
}

/// Owning form — what `Deserialize` produces.
#[derive(Deserialize)]
struct BytesPageWire {
    index: u64,
    offset: u64,
    len: u64,
    bytes: Vec<u8>,
}

/// Borrowing form — what `Serialize` emits. Same field names in the same order,
/// so the wire bytes are identical to the owning form; borrowing is what keeps
/// `serialize` from cloning the whole page payload. The struct *name* differs
/// and is not observable: postcard ignores struct names, and so does
/// [`bytes_page_parts`].
#[derive(Serialize)]
struct BytesPageWireRef<'a> {
    index: u64,
    offset: u64,
    len: u64,
    bytes: &'a [u8],
}

impl Serialize for BytesPage {
    fn serialize<S>(&self, serializer: S) -> core::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_newtype_struct(
            BYTES_PAGE_NEWTYPE_NAME,
            &BytesPageWireRef {
                index: self.index,
                offset: self.offset,
                len: self.len,
                bytes: &self.bytes,
            },
        )
    }
}

impl<'de> Deserialize<'de> for BytesPage {
    fn deserialize<D>(deserializer: D) -> core::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = deserializer.deserialize_newtype_struct(
            BYTES_PAGE_NEWTYPE_NAME,
            BytesPageWireVisitor,
        )?;
        Ok(Self {
            index: wire.index,
            offset: wire.offset,
            len: wire.len,
            bytes: wire.bytes,
        })
    }
}

struct BytesPageWireVisitor;

impl<'de> serde::de::Visitor<'de> for BytesPageWireVisitor {
    type Value = BytesPageWire;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        formatter.write_str("a BytesPage newtype")
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> core::result::Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        BytesPageWire::deserialize(deserializer)
    }
}

/// A paged byte region. `Selectable`, never `Materializable`.
///
/// Page size is a type parameter so `select!` can convert literal byte offsets
/// to page indices without type-level information the proc macro does not have.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Bytes<const PAGE_SIZE: u64> {
    byte_len: u64,
    page_size: u64,
    /// Public because `bytes_schema` already advertises `pages` as a struct
    /// field (see [`bytes_schema`]), and generated `select!` code reaches it by
    /// that name as a plain field access. Read-only in practice: the sibling
    /// fields stay private, so a `Bytes` still cannot be built by struct
    /// literal outside this module. [`Bytes::pages`] remains the authored
    /// accessor.
    pub pages: List<BytesPage>,
}

impl<const PAGE_SIZE: u64> Bytes<PAGE_SIZE> {
    /// Split `bytes` into `⌈len / PAGE_SIZE⌉` pages, the last one short.
    pub fn paged(bytes: impl Into<Vec<u8>>) -> Result<Self> {
        if PAGE_SIZE == 0 {
            return Err(Error::PageSizeZero);
        }
        let bytes = bytes.into();
        let byte_len = bytes.len() as u64;
        let mut pages = Vec::new();
        if byte_len == 0 {
            return Ok(Self {
                byte_len: 0,
                page_size: PAGE_SIZE,
                pages: List::from(pages),
            });
        }
        let count = page_count(byte_len, PAGE_SIZE);
        for index in 0..count {
            let offset = index.saturating_mul(PAGE_SIZE);
            let take = expected_page_len(byte_len, PAGE_SIZE, index) as usize;
            let start = offset as usize;
            let page_bytes = bytes[start..start + take].to_vec();
            pages.push(BytesPage::__from_parts(index, offset, page_bytes));
        }
        Ok(Self {
            byte_len,
            page_size: PAGE_SIZE,
            pages: List::from(pages),
        })
    }

    pub fn byte_len(&self) -> u64 {
        self.byte_len
    }

    pub fn page_size(&self) -> u64 {
        self.page_size
    }

    pub fn pages(&self) -> &List<BytesPage> {
        &self.pages
    }

    pub fn into_pages(self) -> List<BytesPage> {
        self.pages
    }
}

impl<const PAGE_SIZE: u64> core::fmt::Debug for Bytes<PAGE_SIZE> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Bytes")
            .field("byte_len", &self.byte_len)
            .field("page_size", &self.page_size)
            .field("pages", &self.pages.len())
            .finish()
    }
}

impl<const PAGE_SIZE: u64> Selectable for Bytes<PAGE_SIZE> {
    fn schema() -> SchemaNode {
        bytes_schema(PAGE_SIZE)
    }
}

/// A type whose page size is a compile-time constant — `Bytes<P>` and
/// `AuthRef<Bytes<P>>`. `select!` uses this to convert a literal byte offset
/// on the region itself into a page index.
pub trait PageSized {
    const PAGE_SIZE: u64;
}

impl<const PAGE_SIZE: u64> PageSized for Bytes<PAGE_SIZE> {
    const PAGE_SIZE: u64 = PAGE_SIZE;
}

/// FNV-1a 64-bit, so the derive and `select!` agree on a field's const key
/// without sharing a string in the type system.
pub const fn bytes_field_key(name: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        i += 1;
    }
    hash
}

/// Per-field page size on a user struct (`ModelFile::weights` → `Bytes<N>`).
/// The derive emits one impl per `Bytes` field; `AuthRef<T>` forwards.
pub trait BytesFieldPageSize<const FIELD: u64> {
    const PAGE_SIZE: u64;
}

/// Convert a literal byte offset on a `Bytes<P>` region into a page index.
/// Alignment is a `const` assert: `cargo build` / monomorphization fails on an
/// unaligned literal. `cargo check` (trybuild) does not instantiate it.
pub fn page_index_for_region<T: PageSized, const OFFSET: u64>(_source: &T) -> u64 {
    const { assert!(OFFSET % T::PAGE_SIZE == 0, "select! byte offset is not page-aligned"); }
    OFFSET / T::PAGE_SIZE
}

pub fn page_index_for_field<T: BytesFieldPageSize<FIELD>, const FIELD: u64, const OFFSET: u64>(
    _source: &T,
) -> u64 {
    const { assert!(OFFSET % T::PAGE_SIZE == 0, "select! byte offset is not page-aligned"); }
    OFFSET / T::PAGE_SIZE
}

pub fn page_range_for_region<T: PageSized, const START: u64, const END: u64>(
    _source: &T,
) -> (u64, u64) {
    const {
        assert!(
            START % T::PAGE_SIZE == 0 && END % T::PAGE_SIZE == 0 && START <= END,
            "select! byte range is not page-aligned"
        );
    }
    (START / T::PAGE_SIZE, END / T::PAGE_SIZE)
}

pub fn page_range_for_field<
    T: BytesFieldPageSize<FIELD>,
    const FIELD: u64,
    const START: u64,
    const END: u64,
>(
    _source: &T,
) -> (u64, u64) {
    const {
        assert!(
            START % T::PAGE_SIZE == 0 && END % T::PAGE_SIZE == 0 && START <= END,
            "select! byte range is not page-aligned"
        );
    }
    (START / T::PAGE_SIZE, END / T::PAGE_SIZE)
}

/// Rule 2: `pages.len() == ⌈byte_len / page_size⌉`.
pub fn check_page_partition(byte_len: u64, page_size: u64, count: u64) -> Result<()> {
    if page_size == 0 {
        return Err(Error::PageSizeZero);
    }
    let expected = page_count(byte_len, page_size);
    if count != expected {
        return Err(Error::PageShape {
            index: count,
            offset: byte_len,
            len: expected,
        });
    }
    Ok(())
}

/// Rule 3: page `i` has `offset == i × page_size` and
/// `len == min(page_size, byte_len − offset)`.
pub fn check_page_shape(
    byte_len: u64,
    page_size: u64,
    index: u64,
    offset: u64,
    len: u64,
) -> Result<()> {
    if page_size == 0 {
        return Err(Error::PageSizeZero);
    }
    let expected_offset = index.saturating_mul(page_size);
    let expected_len = expected_page_len(byte_len, page_size, index);
    if offset != expected_offset || len != expected_len {
        return Err(Error::PageShape { index, offset, len });
    }
    Ok(())
}

/// Host-side partition + shape over a constructed region.
pub fn check_bytes_geometry<const PAGE_SIZE: u64>(region: &Bytes<PAGE_SIZE>) -> Result<()> {
    if region.page_size() != PAGE_SIZE {
        return Err(Error::PageSizeMismatch {
            declared: PAGE_SIZE,
            artifact: region.page_size(),
        });
    }
    check_page_partition(region.byte_len(), PAGE_SIZE, region.pages().len() as u64)?;
    for (i, page) in region.pages().iter().enumerate() {
        check_page_shape(
            region.byte_len(),
            PAGE_SIZE,
            i as u64,
            page.offset(),
            page.len() as u64,
        )?;
        if page.index() != i as u64 {
            return Err(Error::PageShape {
                index: page.index(),
                offset: page.offset(),
                len: page.len() as u64,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn paged_rejects_zero_page_size() {
        assert!(matches!(
            Bytes::<0>::paged(vec![1, 2, 3]),
            Err(Error::PageSizeZero)
        ));
    }

    #[test]
    fn paged_empty_region() {
        let region = Bytes::<4>::paged(Vec::<u8>::new()).unwrap();
        assert_eq!(region.byte_len(), 0);
        assert_eq!(region.page_size(), 4);
        assert!(region.pages().is_empty());
    }

    #[test]
    fn paged_shorter_than_page() {
        let region = Bytes::<4>::paged(vec![1, 2]).unwrap();
        assert_eq!(region.pages().len(), 1);
        let page = &region.pages()[0];
        assert_eq!(page.index(), 0);
        assert_eq!(page.offset(), 0);
        assert_eq!(page.as_slice(), &[1, 2]);
    }

    #[test]
    fn paged_exact_page() {
        let region = Bytes::<4>::paged(vec![1, 2, 3, 4]).unwrap();
        assert_eq!(region.pages().len(), 1);
        assert_eq!(region.pages()[0].as_slice(), &[1, 2, 3, 4]);
    }

    #[test]
    fn paged_one_past_page() {
        let region = Bytes::<4>::paged(vec![1, 2, 3, 4, 5]).unwrap();
        assert_eq!(region.pages().len(), 2);
        assert_eq!(region.pages()[0].as_slice(), &[1, 2, 3, 4]);
        assert_eq!(region.pages()[1].index(), 1);
        assert_eq!(region.pages()[1].offset(), 4);
        assert_eq!(region.pages()[1].as_slice(), &[5]);
    }

    #[test]
    fn paged_exact_multiple() {
        let region = Bytes::<4>::paged(vec![0; 8]).unwrap();
        assert_eq!(region.pages().len(), 2);
        assert_eq!(region.pages()[1].offset(), 4);
        assert_eq!(region.pages()[1].len(), 4);
    }

    #[test]
    fn bytes_schema_embeds_page_size() {
        let schema = bytes_schema(262_144);
        match schema {
            SchemaNode::Struct { type_name, fields } => {
                assert_eq!(type_name, "$raster::Bytes<262144>");
                assert_eq!(fields.len(), 3);
                assert_eq!(fields[0].name, "byte_len");
                assert_eq!(fields[1].name, "page_size");
                assert_eq!(fields[2].name, "pages");
            }
            other => panic!("expected struct schema, got {other:?}"),
        }
    }

    #[test]
    fn check_page_shape_accepts_partition() {
        check_page_shape(5, 4, 0, 0, 4).unwrap();
        check_page_shape(5, 4, 1, 4, 1).unwrap();
        assert!(check_page_shape(5, 4, 1, 3, 1).is_err());
        assert!(check_page_shape(5, 4, 0, 0, 3).is_err());
    }

    #[test]
    fn check_page_partition_matches_ceil() {
        check_page_partition(0, 4, 0).unwrap();
        check_page_partition(4, 4, 1).unwrap();
        check_page_partition(5, 4, 2).unwrap();
        assert!(check_page_partition(5, 4, 1).is_err());
    }

    #[test]
    fn paged_region_passes_geometry() {
        let region = Bytes::<4>::paged(vec![1, 2, 3, 4, 5]).unwrap();
        check_bytes_geometry(&region).unwrap();
    }

    /// The page wire format, derived from the postcard spec rather than from
    /// this implementation: three varint coordinates, then a varint length and
    /// the raw payload. Pinned because `bytes_page_parts` and the borrowing
    /// `BytesPageWireRef` changed *how* the tree bridges read a page — if either
    /// ever changed what postcard *writes*, `input_commitment` and every image
    /// id would move, turning an optimization into a migration.
    #[test]
    fn postcard_wire_is_pinned() {
        let region = Bytes::<4>::paged(vec![7u8, 8, 9, 10, 11]).unwrap();

        let mut buf = [0u8; 64];
        let page = crate::postcard::to_slice(&region.pages()[1], &mut buf).unwrap();
        //          index  offset  len   seq-len  payload
        assert_eq!(page, &[1, 4, 1, 1, 11]);

        let mut buf = [0u8; 64];
        let whole = crate::postcard::to_slice(&region, &mut buf).unwrap();
        assert_eq!(
            whole,
            &[
                5, 4, // byte_len, page_size
                2, // pages: seq len
                0, 0, 4, 4, 7, 8, 9, 10, // page 0
                1, 4, 1, 1, 11, // page 1
            ]
        );
    }

    #[test]
    fn postcard_round_trips_every_page_shape() {
        for len in [0usize, 1, 3, 4, 5, 8, 9] {
            let payload: Vec<u8> = (0..len).map(|i| i as u8).collect();
            let region = Bytes::<4>::paged(payload.clone()).unwrap();
            let mut buf = [0u8; 256];
            let encoded = crate::postcard::to_slice(&region, &mut buf).unwrap();
            let decoded: Bytes<4> = crate::postcard::from_bytes(encoded).unwrap();
            assert_eq!(decoded.byte_len(), len as u64, "byte_len at len={len}");
            let joined: Vec<u8> = decoded
                .pages()
                .iter()
                .flat_map(|page| page.as_slice().to_vec())
                .collect();
            assert_eq!(joined, payload, "payload at len={len}");
        }
    }

    /// `bytes_page_parts` accepts a `serialize_bytes` payload so the wire struct
    /// could switch to one — but it is reachable only from inside a page. The
    /// general-purpose serializers must still refuse raw bytes outright, which is
    /// what keeps byte data off the `List<u8>` path that `paged-bytes.md`
    /// §Problem exists to close.
    #[test]
    fn raw_bytes_are_still_not_a_rastered_type() {
        struct RawBytes(Vec<u8>);

        impl Serialize for RawBytes {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> core::result::Result<S::Ok, S::Error> {
                serializer.serialize_bytes(&self.0)
            }
        }

        let err = crate::draft::draft_value_from_serialize(&RawBytes(vec![1, 2, 3]))
            .expect_err("draft serializer must refuse raw bytes");
        assert!(
            alloc::format!("{err}").contains("raw bytes are not supported"),
            "unexpected error: {err}"
        );
    }

    /// A page must not be mistakable for anything else through the narrow
    /// serializer: it is a field extractor for one known shape, not a general
    /// bytes channel.
    #[test]
    fn bytes_page_parts_rejects_non_page_values() {
        assert!(bytes_page_parts(&42u64).is_err());
        assert!(bytes_page_parts("not a page").is_err());
        assert!(bytes_page_parts(&alloc::vec![1u8, 2, 3]).is_err());
    }

    #[test]
    fn bytes_page_parts_reads_the_wire_struct() {
        let region = Bytes::<4>::paged(vec![1, 2, 3, 4, 5]).unwrap();
        let parts = bytes_page_parts(&region.pages()[1]).unwrap();
        assert_eq!((parts.index, parts.offset, parts.len), (1, 4, 1));
        assert_eq!(parts.bytes, alloc::vec![5]);
    }

    #[test]
    fn aligned_literal_converts_to_page_index() {
        let region = Bytes::<4>::paged(vec![0; 8]).unwrap();
        assert_eq!(page_index_for_region::<_, 4>(&region), 1);
        assert_eq!(page_range_for_region::<_, 0, 8>(&region), (0, 2));
    }
}
