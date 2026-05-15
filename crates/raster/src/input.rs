use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;
use serde::{de::DeserializeOwned, Serialize};

pub use raster_core::input::{
    verify_selection_proof, ArgKind, External, ExternalArgInfo, ExternalRef, ExternalSelection,
    ExternalValue, ListProofDirection, ListProofSibling, Merklized, ResolvedArg, SchemaField,
    SchemaNode, Selectable, SelectedPayload, SelectionProof, SelectionProofStep, SelectorPath,
    SelectorSegment, StructProofSibling,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExternalArg {
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypedExternal<Root> {
    name: String,
    marker: PhantomData<fn() -> Root>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SelectedExternalArg {
    source: ExternalArg,
    selector: SelectorPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypedSelectedExternal<Root> {
    source: TypedExternal<Root>,
    selector: SelectorPath,
}

pub trait CanSelectPath {
    type Output;

    fn with_selector(self, selector: SelectorPath) -> Self::Output;
}

impl ExternalArg {
    pub fn new(name: &str) -> Self {
        Self { name: name.into() }
    }

    pub fn into_selection(self) -> ExternalSelection {
        ExternalSelection::new(self.name)
    }
}

impl<Root> TypedExternal<Root> {
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

impl CanSelectPath for ExternalArg {
    type Output = SelectedExternalArg;

    fn with_selector(self, selector: SelectorPath) -> Self::Output {
        SelectedExternalArg {
            source: self,
            selector,
        }
    }
}

impl<Root> CanSelectPath for TypedExternal<Root> {
    type Output = TypedSelectedExternal<Root>;

    fn with_selector(self, selector: SelectorPath) -> Self::Output {
        TypedSelectedExternal {
            source: self,
            selector,
        }
    }
}

pub fn external(name: &str) -> ExternalArg {
    ExternalArg::new(name)
}

pub fn typed_external<Root>(name: &str) -> TypedExternal<Root> {
    TypedExternal::new(name)
}

pub fn select_source<S>(source: S, selector: SelectorPath) -> S::Output
where
    S: CanSelectPath,
{
    source.with_selector(selector)
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

impl<T> IntoResolvedArg<T> for ExternalArg
where
    T: DeserializeOwned + Serialize,
{
    fn into_resolved_arg(self) -> raster_core::Result<ResolvedArg<T>> {
        let selection = self.into_selection();
        let value = resolve_external_value::<T>(selection)?;
        Ok(ResolvedArg::external(
            value.value,
            ExternalArgInfo {
                name: value.name,
                selector: value.selector,
                commitment: value.commitment,
                bytes: value.bytes,
                selected: value.selected,
            },
        ))
    }
}

impl<T> IntoResolvedArg<T> for SelectedExternalArg
where
    T: DeserializeOwned + Serialize,
{
    fn into_resolved_arg(self) -> raster_core::Result<ResolvedArg<T>> {
        let value = resolve_external_value::<T>(ExternalSelection::with_selector(
            self.source.name,
            self.selector,
        ))?;
        Ok(ResolvedArg::external(
            value.value,
            ExternalArgInfo {
                name: value.name,
                selector: value.selector,
                commitment: value.commitment,
                bytes: value.bytes,
                selected: value.selected,
            },
        ))
    }
}

impl<T> IntoResolvedArg<T> for External<T>
where
    T: DeserializeOwned + Serialize,
{
    fn into_resolved_arg(self) -> raster_core::Result<ResolvedArg<T>> {
        ExternalArg::new(self.name()).into_resolved_arg()
    }
}

impl<Root> IntoResolvedArg<Root> for TypedExternal<Root>
where
    Root: DeserializeOwned + Serialize,
{
    fn into_resolved_arg(self) -> raster_core::Result<ResolvedArg<Root>> {
        ExternalArg::new(&self.name).into_resolved_arg()
    }
}

impl<Root, T> IntoResolvedArg<T> for TypedSelectedExternal<Root>
where
    Root: DeserializeOwned + Serialize + Selectable + Merklized,
    T: DeserializeOwned + Serialize,
{
    fn into_resolved_arg(self) -> raster_core::Result<ResolvedArg<T>> {
        let value = resolve_typed_external_value::<Root, T>(ExternalSelection::with_selector(
            self.source.name,
            self.selector,
        ))?;
        Ok(ResolvedArg::external(
            value.value,
            ExternalArgInfo {
                name: value.name,
                selector: value.selector,
                commitment: value.commitment,
                bytes: value.bytes,
                selected: value.selected,
            },
        ))
    }
}

pub fn into_resolved_arg<T, A>(arg: A) -> raster_core::Result<ResolvedArg<T>>
where
    A: IntoResolvedArg<T>,
{
    arg.into_resolved_arg()
}

pub fn parse_program_input_value<T: DeserializeOwned>(name: Option<&str>) -> Option<T> {
    #[cfg(feature = "std")]
    {
        return raster_runtime::parse_program_input_value(name);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = name;
        None
    }
}

pub fn resolve_external_value<T: DeserializeOwned + Serialize>(
    reference: ExternalSelection,
) -> raster_core::Result<raster_core::input::ExternalValue<T>> {
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
) -> raster_core::Result<raster_core::input::ExternalValue<T>>
where
    Root: DeserializeOwned + Serialize + Selectable + Merklized,
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
