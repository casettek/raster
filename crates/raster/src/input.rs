use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;
use serde::{de::DeserializeOwned, Serialize};

pub use raster_core::input::{
    verify_selection_proof, ExternalArg, ExternalEncoding, ExternalRef, ExternalSelection,
    ListProofDirection, ListProofSibling, ResolvedArg, SchemaField, SchemaNode, Selectable,
    SelectedPayload, SelectionProof, SelectionProofStep, SelectorPath, SelectorSegment,
};
use raster_core::trace::{ExternalData as TraceExternalData, FnInputValue};

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct TypedExternalBinding<Root> {
    name: String,
    marker: PhantomData<fn() -> Root>,
}

#[derive(Debug, PartialEq, Eq, Hash)]
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

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct TypedSelectedExternalBinding<Root, Selected> {
    source: TypedExternalBinding<Root>,
    selector: TypedSelectorPath<Root, Selected>,
}

#[doc(hidden)]
#[derive(Debug)]
pub struct DeferredExternal<Root, Current> {
    name: String,
    selector: SelectorPath,
    resolve: fn(ExternalSelection) -> raster_core::Result<ExternalArg<Current>>,
    marker: PhantomData<fn() -> (Root, Current)>,
}

#[derive(Debug)]
pub enum SequenceArg<Root, Current> {
    Inline(Current),
    External(DeferredExternal<Root, Current>),
}

impl<Root> Clone for TypedExternalBinding<Root> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            marker: PhantomData,
        }
    }
}

impl<Root, Selected> Clone for TypedSelectorPath<Root, Selected> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            marker: PhantomData,
        }
    }
}

impl<Root, Selected> Clone for TypedSelectedExternalBinding<Root, Selected> {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            selector: self.selector.clone(),
        }
    }
}

impl<Root, Current> Clone for DeferredExternal<Root, Current> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            selector: self.selector.clone(),
            resolve: self.resolve,
            marker: PhantomData,
        }
    }
}

impl<Root, Current> Clone for SequenceArg<Root, Current>
where
    Current: Clone,
{
    fn clone(&self) -> Self {
        match self {
            Self::Inline(value) => Self::Inline(value.clone()),
            Self::External(binding) => Self::External(binding.clone()),
        }
    }
}

pub trait IntoSequenceArg<Current> {
    type Root;

    fn into_sequence_arg(self) -> SequenceArg<Self::Root, Current>;
}

pub trait TypedSequenceRoot: DeserializeOwned + Serialize + Selectable {}

impl<T> TypedSequenceRoot for T where T: DeserializeOwned + Serialize + Selectable {}

pub trait SelectSource {
    type Root;
    type Current;
    type Selected<Selected>;

    fn select<Selected>(
        self,
        selector: TypedSelectorPath<Self::Current, Selected>,
    ) -> Self::Selected<Selected>
    where
        Selected: DeserializeOwned + Serialize;
}

impl<Root> SelectSource for TypedExternalBinding<Root> {
    type Root = Root;
    type Current = Root;
    type Selected<Selected> = TypedSelectedExternalBinding<Root, Selected>;

    fn select<Selected>(
        self,
        selector: TypedSelectorPath<Self::Current, Selected>,
    ) -> Self::Selected<Selected>
    where
        Selected: DeserializeOwned + Serialize,
    {
        TypedSelectedExternalBinding {
            source: self,
            selector,
        }
    }
}

impl<Root, Current> SelectSource for TypedSelectedExternalBinding<Root, Current> {
    type Root = Root;
    type Current = Current;
    type Selected<Selected> = TypedSelectedExternalBinding<Root, Selected>;

    fn select<Selected>(
        self,
        selector: TypedSelectorPath<Self::Current, Selected>,
    ) -> Self::Selected<Selected>
    where
        Selected: DeserializeOwned + Serialize,
    {
        let selector = TypedSelectorPath::new(compose_selector_paths(
            self.selector.into_path(),
            selector.into_path(),
        ));
        TypedSelectedExternalBinding {
            source: self.source,
            selector,
        }
    }
}

impl<Root, Current> SelectSource for SequenceArg<Root, Current>
where
    Root: TypedSequenceRoot,
{
    type Root = Root;
    type Current = Current;
    type Selected<Selected> = SequenceArg<Root, Selected>;

    fn select<Selected>(
        self,
        selector: TypedSelectorPath<Self::Current, Selected>,
    ) -> Self::Selected<Selected>
    where
        Selected: DeserializeOwned + Serialize,
    {
        match self {
            SequenceArg::Inline(_) => {
                panic!(
                    "select! on inline sequence values is not supported; only external bindings can be refined inside sequences"
                )
            }
            SequenceArg::External(binding) => SequenceArg::External(DeferredExternal {
                name: binding.name,
                selector: compose_selector_paths(binding.selector, selector.into_path()),
                resolve: resolve_typed_external_value::<Root, Selected>,
                marker: PhantomData,
            }),
        }
    }
}

pub fn select_source<Source, Selected>(
    source: Source,
    selector: TypedSelectorPath<Source::Current, Selected>,
) -> Source::Selected<Selected>
where
    Source: SelectSource,
    Selected: DeserializeOwned + Serialize,
{
    source.select(selector)
}

fn compose_selector_paths(prefix: SelectorPath, suffix: SelectorPath) -> SelectorPath {
    let mut segments = prefix.segments;
    segments.extend(suffix.segments);
    SelectorPath::new(segments)
}

pub fn selector_path(segments: Vec<SelectorSegment>) -> SelectorPath {
    SelectorPath::new(segments)
}

impl<T> IntoSequenceArg<T> for T
where
    T: Serialize,
{
    type Root = T;

    fn into_sequence_arg(self) -> SequenceArg<Self::Root, T> {
        SequenceArg::Inline(self)
    }
}

impl<Root> IntoSequenceArg<Root> for TypedExternalBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    type Root = Root;

    fn into_sequence_arg(self) -> SequenceArg<Self::Root, Root> {
        SequenceArg::External(DeferredExternal {
            name: self.name,
            selector: SelectorPath::default(),
            resolve: resolve_external_value::<Root>,
            marker: PhantomData,
        })
    }
}

impl<Root, Current> IntoSequenceArg<Current> for TypedSelectedExternalBinding<Root, Current>
where
    Root: DeserializeOwned + Serialize + Selectable,
    Current: DeserializeOwned + Serialize,
{
    type Root = Root;

    fn into_sequence_arg(self) -> SequenceArg<Self::Root, Current> {
        SequenceArg::External(DeferredExternal {
            name: self.source.name,
            selector: self.selector.into_path(),
            resolve: resolve_typed_external_value::<Root, Current>,
            marker: PhantomData,
        })
    }
}

impl<Root, Current> IntoSequenceArg<Current> for SequenceArg<Root, Current> {
    type Root = Root;

    fn into_sequence_arg(self) -> SequenceArg<Self::Root, Current> {
        self
    }
}

pub fn into_sequence_arg<T, A>(arg: A) -> SequenceArg<A::Root, T>
where
    A: IntoSequenceArg<T>,
{
    arg.into_sequence_arg()
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

impl<Root, Current> IntoResolvedArg<Current> for SequenceArg<Root, Current>
where
    Current: Serialize,
{
    fn into_resolved_arg(self) -> raster_core::Result<ResolvedArg<Current>> {
        match self {
            SequenceArg::Inline(value) => Ok(ResolvedArg::inline(value)),
            SequenceArg::External(binding) => {
                let value = (binding.resolve)(ExternalSelection::with_selector(
                    binding.name,
                    binding.selector,
                ))?;
                Ok(ResolvedArg::external(value))
            }
        }
    }
}

pub fn into_resolved_arg<T, A>(arg: A) -> raster_core::Result<ResolvedArg<T>>
where
    A: IntoResolvedArg<T>,
{
    arg.into_resolved_arg()
}

pub fn sequence_arg_trace<Root, T>(
    arg: &SequenceArg<Root, T>,
) -> raster_core::Result<(FnInputValue, Option<TraceExternalData>)>
where
    T: Serialize,
{
    match arg {
        SequenceArg::Inline(value) => Ok((
            FnInputValue::Inline(raster_core::postcard::to_allocvec(value).unwrap_or_default()),
            None,
        )),
        SequenceArg::External(binding) => {
            let resolved = (binding.resolve)(ExternalSelection::with_selector(
                binding.name.clone(),
                binding.selector.clone(),
            ))?;
            Ok((
                FnInputValue::ExternalBinding,
                Some(TraceExternalData {
                    name: resolved.name,
                    commitment: resolved
                        .commitment
                        .map(|value| value.into_bytes())
                        .unwrap_or_default(),
                    tree_root: resolved.selected.proof.root_hash.clone(),
                    selector: resolved.selector,
                    selected: resolved.selected,
                }),
            ))
        }
    }
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

#[cfg(feature = "std")]
pub fn encode_raster_value<T: Serialize>(
    value: &T,
) -> raster_core::Result<(Vec<u8>, Vec<u8>, String)> {
    raster_runtime::encode_raster_value(value)
}

#[cfg(feature = "std")]
pub fn write_raster_files<T: Serialize>(
    value: &T,
    data_path: &std::path::Path,
    index_path: &std::path::Path,
) -> raster_core::Result<String> {
    raster_runtime::write_raster_files(value, data_path, index_path)
}
