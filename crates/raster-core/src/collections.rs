//! Raster collection vocabulary: [`List`], [`Block`], and the [`Materializable`]
//! marker trait.
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

use crate::input::{Selectable, SchemaNode};

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
    (), bool, char, String,
    u8, u16, u32, u64, u128, usize,
    i8, i16, i32, i64, i128, isize,
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
