use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;
use serde::{de::DeserializeOwned, Serialize};

pub use raster_core::input::{
    verify_selection_proof, ExternalArg, ExternalRef, ExternalSelection, ListProofDirection,
    ListProofSibling, ResolvedArg, SchemaField, SchemaNode, Selectable, SelectedPayload,
    SelectionProof, SelectionProofStep, SelectorPath, SelectorSegment,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypedExternalBinding<Root> {
    name: String,
    marker: PhantomData<fn() -> Root>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypedSelectorPath<Root, Selected> {
    path: SelectorPath,
    marker: PhantomData<fn() -> (Root, Selected)>,
}

impl<Root> TypedExternalBinding<Root> {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            marker: PhantomData,
        }
    }

    pub fn into_selection(self) -> ExternalSelection {
        ExternalSelection::new(self.name)
    }
}

impl<Root, Selected> TypedSelectorPath<Root, Selected> {
    pub fn new(path: SelectorPath) -> Self {
        Self {
            path,
            marker: PhantomData,
        }
    }

    pub fn path(&self) -> &SelectorPath {
        &self.path
    }

    pub fn into_path(self) -> SelectorPath {
        self.path
    }
}

pub fn typed_external<Root>(name: &str) -> TypedExternalBinding<Root> {
    TypedExternalBinding::new(name)
}

pub fn typed_selector_path<Root, Selected>(
    path: SelectorPath,
) -> TypedSelectorPath<Root, Selected> {
    TypedSelectorPath::new(path)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypedSelectedExternalBinding<Root, Selected> {
    source: TypedExternalBinding<Root>,
    selector: TypedSelectorPath<Root, Selected>,
}

pub fn select_source<Root, Selected>(
    source: TypedExternalBinding<Root>,
    selector: TypedSelectorPath<Root, Selected>,
) -> TypedSelectedExternalBinding<Root, Selected> {
    TypedSelectedExternalBinding { source, selector }
}

pub fn selector_path(segments: Vec<SelectorSegment>) -> SelectorPath {
    SelectorPath::new(segments)
}

pub trait IntoResolvedArg<T> {
    fn into_resolved_arg(self) -> raster_core::Result<ResolvedArg<T>>;
}

impl<T> IntoResolvedArg<T> for T
where
    T: Serialize,
{
    fn into_resolved_arg(self) -> raster_core::Result<ResolvedArg<T>> {
        Ok(ResolvedArg::inline(self))
    }
}

impl<Root> IntoResolvedArg<Root> for TypedExternalBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    fn into_resolved_arg(self) -> raster_core::Result<ResolvedArg<Root>> {
        let value = resolve_external_value::<Root>(self.into_selection())?;
        Ok(ResolvedArg::external(value))
    }
}

impl<Root, Selected> IntoResolvedArg<Selected> for TypedSelectedExternalBinding<Root, Selected>
where
    Root: DeserializeOwned + Serialize + Selectable,
    Selected: DeserializeOwned + Serialize,
{
    fn into_resolved_arg(self) -> raster_core::Result<ResolvedArg<Selected>> {
        let value = resolve_typed_external_value::<Root, Selected>(
            ExternalSelection::with_selector(self.source.name, self.selector.into_path()),
        )?;
        Ok(ResolvedArg::external(value))
    }
}

pub fn into_resolved_arg<T, A>(arg: A) -> raster_core::Result<ResolvedArg<T>>
where
    A: IntoResolvedArg<T>,
{
    arg.into_resolved_arg()
}

pub fn resolve_external_value<T: DeserializeOwned + Serialize>(
    reference: ExternalSelection,
) -> raster_core::Result<raster_core::input::ExternalArg<T>> {
    #[cfg(feature = "std")]
    {
        return raster_runtime::resolve_external_value(reference);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = reference;
        Err(raster_core::Error::Other(alloc::format!(
            "External input resolution requires the `std` feature"
        )))
    }
}

pub fn resolve_typed_external_value<Root, T>(
    reference: ExternalSelection,
) -> raster_core::Result<raster_core::input::ExternalArg<T>>
where
    Root: DeserializeOwned + Serialize + Selectable,
    T: DeserializeOwned + Serialize,
{
    #[cfg(feature = "std")]
    {
        return raster_runtime::resolve_typed_external_value::<Root, T>(reference);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = reference;
        Err(raster_core::Error::Other(alloc::format!(
            "Typed external resolution requires the `std` feature"
        )))
    }
}
