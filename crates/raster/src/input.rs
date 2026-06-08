use alloc::string::String;
use alloc::vec::Vec;
use alloc::rc::Rc;
use core::marker::PhantomData;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

pub use raster_core::input::{
    verify_selection_proof, ExternalValue, ExternalEncoding, ExternalRef, ExternalSelection,
    InternalValue, InternalRef, ListProofDirection, ListProofSibling, AuthValue, SchemaField,
    SchemaNode, Selectable, SelectedPayload, SelectionProof, SelectionProofStep, SelectorPath,
    SelectorSegment,
};
use raster_core::trace::{
    ExternalData as TraceExternalData, FnInputValue, InternalData as TraceInternalData,
};

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct TypedExternalBinding<Root> {
    name: String,
    marker: PhantomData<fn() -> Root>,
}

#[derive(Debug)]
pub struct TypedInternalBinding<Root> {
    reference: InternalRef,
    resolve: fn(InternalRef) -> raster_core::Result<InternalValue<Root>>,
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

impl<Root> TypedInternalBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    pub fn new(reference: InternalRef) -> Self {
        Self {
            reference,
            resolve: resolve_internal_value::<Root>,
            marker: PhantomData,
        }
    }

    #[doc(hidden)]
    pub fn with_resolver(
        reference: InternalRef,
        resolve: fn(InternalRef) -> raster_core::Result<InternalValue<Root>>,
    ) -> Self {
        Self {
            reference,
            resolve,
            marker: PhantomData,
        }
    }

    pub fn reference(&self) -> &InternalRef {
        &self.reference
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

pub fn typed_internal<Root>(reference: InternalRef) -> TypedInternalBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    TypedInternalBinding::new(reference)
}

#[doc(hidden)]
pub fn typed_internal_with_resolver<Root>(
    reference: InternalRef,
    resolve: fn(InternalRef) -> raster_core::Result<InternalValue<Root>>,
) -> TypedInternalBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    TypedInternalBinding::with_resolver(reference, resolve)
}

pub fn typed_selector_path<Root, Selected>(
    path: SelectorPath,
) -> TypedSelectorPath<Root, Selected> {
    TypedSelectorPath::new(path)
}

type ExternalResolveFn<Current> =
    Rc<dyn Fn(ExternalSelection) -> raster_core::Result<ExternalValue<Current>>>;

#[doc(hidden)]
pub struct DeferredAuthExternal<Current> {
    name: String,
    selector: SelectorPath,
    resolve: ExternalResolveFn<Current>,
}

#[doc(hidden)]
#[derive(Debug)]
pub struct DeferredAuthInternal<Current> {
    reference: InternalRef,
    resolve: fn(InternalRef) -> raster_core::Result<InternalValue<Current>>,
    marker: PhantomData<fn() -> Current>,
}

pub enum AuthRef<Current> {
    Inline(Current),
    External(DeferredAuthExternal<Current>),
    Internal(DeferredAuthInternal<Current>),
}

impl<Current> AuthRef<Current> {
    pub fn reference(&self) -> &InternalRef {
        match self {
            Self::Internal(binding) => &binding.reference,
            Self::Inline(_) | Self::External(_) => {
                panic!("AuthRef::reference() is only available for internal bindings")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AuthRefTrace {
    pub value: FnInputValue,
    pub external: Option<TraceExternalData>,
    pub internal: Option<TraceInternalData>,
}

impl<Root> Clone for TypedExternalBinding<Root> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            marker: PhantomData,
        }
    }
}

impl<Root> Clone for TypedInternalBinding<Root> {
    fn clone(&self) -> Self {
        Self {
            reference: self.reference.clone(),
            resolve: self.resolve,
            marker: PhantomData,
        }
    }
}

impl<Root> PartialEq for TypedInternalBinding<Root> {
    fn eq(&self, other: &Self) -> bool {
        self.reference == other.reference
    }
}

impl<Root> Eq for TypedInternalBinding<Root> {}

impl<Root> core::hash::Hash for TypedInternalBinding<Root> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.reference.hash(state);
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

impl<Current> Clone for DeferredAuthExternal<Current> {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            selector: self.selector.clone(),
            resolve: self.resolve.clone(),
        }
    }
}

impl<Current> core::fmt::Debug for DeferredAuthExternal<Current> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DeferredAuthExternal")
            .field("name", &self.name)
            .field("selector", &self.selector)
            .finish()
    }
}

impl<Current> Clone for DeferredAuthInternal<Current> {
    fn clone(&self) -> Self {
        Self {
            reference: self.reference.clone(),
            resolve: self.resolve,
            marker: PhantomData,
        }
    }
}

impl<Current> Clone for AuthRef<Current>
where
    Current: Clone,
{
    fn clone(&self) -> Self {
        match self {
            Self::Inline(value) => Self::Inline(value.clone()),
            Self::External(binding) => Self::External(binding.clone()),
            Self::Internal(binding) => Self::Internal(binding.clone()),
        }
    }
}

impl<Current> core::fmt::Debug for AuthRef<Current>
where
    Current: core::fmt::Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Inline(value) => f.debug_tuple("Inline").field(value).finish(),
            Self::External(binding) => f.debug_tuple("External").field(binding).finish(),
            Self::Internal(binding) => f.debug_tuple("Internal").field(binding).finish(),
        }
    }
}

pub trait IntoAuthRef<Current> {
    fn into_auth_ref(self) -> AuthRef<Current>;
}

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

impl<Root> SelectSource for TypedExternalBinding<Root>
where
    Root: DeserializeOwned + Serialize + Selectable + 'static,
{
    type Root = Root;
    type Current = Root;
    type Selected<Selected> = AuthRef<Selected>;

    fn select<Selected>(
        self,
        selector: TypedSelectorPath<Self::Current, Selected>,
    ) -> Self::Selected<Selected>
    where
        Selected: DeserializeOwned + Serialize,
    {
        let name = self.name;
        let selector = selector.into_path();
        AuthRef::External(DeferredAuthExternal {
            name: name.clone(),
            selector: selector.clone(),
            resolve: Rc::new(move |reference| resolve_typed_external_value::<Root, Selected>(reference)),
        })
    }
}

impl<Current> SelectSource for AuthRef<Current>
where
    Current: DeserializeOwned + Serialize + Selectable + 'static,
{
    type Root = Current;
    type Current = Current;
    type Selected<Selected> = AuthRef<Selected>;

    fn select<Selected>(
        self,
        selector: TypedSelectorPath<Self::Current, Selected>,
    ) -> Self::Selected<Selected>
    where
        Selected: DeserializeOwned + Serialize,
    {
        match self {
            AuthRef::Inline(_) => {
                panic!(
                    "select! on inline or internal sequence values is not supported; only external bindings can be refined inside sequences"
                )
            }
            AuthRef::Internal(_) => {
                panic!("select! on internal sequence bindings is not supported")
            }
            AuthRef::External(binding) => {
                let relative_selector = selector.into_path();
                let full_selector =
                    compose_selector_paths(binding.selector.clone(), relative_selector.clone());
                let current_name = binding.name.clone();
                let current_selector = binding.selector.clone();
                let resolve_current = binding.resolve.clone();
                AuthRef::External(DeferredAuthExternal {
                    name: current_name.clone(),
                    selector: full_selector.clone(),
                    resolve: Rc::new(move |_| {
                        let current = resolve_current(ExternalSelection::with_selector(
                            current_name.clone(),
                            current_selector.clone(),
                        ))?;
                        select_external_value::<Current, Selected>(
                            &current,
                            &relative_selector,
                            &full_selector,
                        )
                    }),
                })
            }
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

impl<T> IntoAuthRef<T> for T
where
    T: Serialize,
{
    fn into_auth_ref(self) -> AuthRef<T> {
        AuthRef::Inline(self)
    }
}

impl<Root> IntoAuthRef<Root> for TypedExternalBinding<Root>
where
    Root: DeserializeOwned + Serialize + 'static,
{
    fn into_auth_ref(self) -> AuthRef<Root> {
        let name = self.name;
        AuthRef::External(DeferredAuthExternal {
            name: name.clone(),
            selector: SelectorPath::default(),
            resolve: Rc::new(move |reference| resolve_external_value::<Root>(reference)),
        })
    }
}

impl<Root> IntoAuthRef<Root> for TypedInternalBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    fn into_auth_ref(self) -> AuthRef<Root> {
        AuthRef::Internal(DeferredAuthInternal {
            reference: self.reference,
            resolve: self.resolve,
            marker: PhantomData,
        })
    }
}

impl<Current> IntoAuthRef<Current> for AuthRef<Current> {
    fn into_auth_ref(self) -> AuthRef<Current> {
        self
    }
}

pub fn into_auth_ref<T, A>(arg: A) -> AuthRef<T>
where
    A: IntoAuthRef<T>,
{
    arg.into_auth_ref()
}

pub trait IntoAuthValue<T> {
    fn into_auth_value(self) -> raster_core::Result<AuthValue<T>>;
}

impl<T> IntoAuthValue<T> for T
where
    T: Serialize,
{
    fn into_auth_value(self) -> raster_core::Result<AuthValue<T>> {
        Ok(AuthValue::inline(self))
    }
}

impl<Root> IntoAuthValue<Root> for TypedExternalBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    fn into_auth_value(self) -> raster_core::Result<AuthValue<Root>> {
        let value = resolve_external_value::<Root>(self.into_selection())?;
        Ok(AuthValue::external(value))
    }
}

impl<Root> IntoAuthValue<Root> for TypedInternalBinding<Root>
where
    Root: DeserializeOwned + Serialize,
{
    fn into_auth_value(self) -> raster_core::Result<AuthValue<Root>> {
        let value = (self.resolve)(self.reference)?;
        Ok(AuthValue::internal(value))
    }
}

impl<Current> IntoAuthValue<Current> for AuthRef<Current>
where
    Current: Serialize,
{
    fn into_auth_value(self) -> raster_core::Result<AuthValue<Current>> {
        match self {
            AuthRef::Inline(value) => Ok(AuthValue::inline(value)),
            AuthRef::External(binding) => {
                let value = (binding.resolve.as_ref())(ExternalSelection::with_selector(
                    binding.name,
                    binding.selector,
                ))?;
                Ok(AuthValue::external(value))
            }
            AuthRef::Internal(binding) => {
                let value = (binding.resolve)(binding.reference)?;
                Ok(AuthValue::internal(value))
            }
        }
    }
}

pub fn into_auth_value<T, A>(arg: A) -> raster_core::Result<AuthValue<T>>
where
    A: IntoAuthValue<T>,
{
    arg.into_auth_value()
}

pub fn auth_ref_trace<T>(arg: &AuthRef<T>) -> raster_core::Result<AuthRefTrace>
where
    T: Serialize + DeserializeOwned,
{
    match arg {
        AuthRef::Inline(value) => Ok(AuthRefTrace {
            value: FnInputValue::Inline(raster_core::postcard::to_allocvec(value).unwrap_or_default()),
            external: None,
            internal: None,
        }),
        AuthRef::External(binding) => {
            let resolved = (binding.resolve.as_ref())(ExternalSelection::with_selector(
                binding.name.clone(),
                binding.selector.clone(),
            ))?;
            Ok(AuthRefTrace {
                value: FnInputValue::ExternalBinding,
                external: Some(TraceExternalData {
                    name: resolved.name,
                    commitment: resolved
                        .commitment
                        .map(|value| value.into_bytes())
                        .unwrap_or_default(),
                    tree_root: resolved.selected.proof.root_hash.clone(),
                    selector: resolved.selector,
                    selected: resolved.selected,
                }),
                internal: None,
            })
        }
        AuthRef::Internal(binding) => {
            let resolved = (binding.resolve)(binding.reference.clone())?;
            Ok(AuthRefTrace {
                value: FnInputValue::InternalBinding,
                external: None,
                internal: Some(TraceInternalData {
                    coordinates: resolved.reference.coordinates,
                    commitment: resolved.reference.commitment,
                }),
            })
        }
    }
}

pub fn auth_ref_result_trace<T>(
    result: &core::result::Result<AuthRef<T>, String>,
) -> raster_core::Result<core::result::Result<AuthRefTrace, String>>
where
    T: Serialize + DeserializeOwned,
{
    match result {
        Ok(value) => Ok(Ok(auth_ref_trace(value)?)),
        Err(error) => Ok(Err(error.clone())),
    }
}

pub fn select_external_value<Root, T>(
    value: &ExternalValue<Root>,
    selector: &SelectorPath,
    full_selector: &SelectorPath,
) -> raster_core::Result<ExternalValue<T>>
where
    Root: DeserializeOwned + Serialize + Selectable,
    T: DeserializeOwned + Serialize,
{
    #[cfg(feature = "std")]
    {
        return raster_runtime::select_external_arg::<Root, T>(value, selector, full_selector);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = value;
        let _ = selector;
        let _ = full_selector;
        Err(raster_core::Error::Other(format!(
            "External selection refinement requires the `std` feature"
        )))
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

pub fn resolve_internal_value<T: DeserializeOwned + Serialize>(
    reference: InternalRef,
) -> raster_core::Result<raster_core::input::InternalValue<T>> {
    #[cfg(feature = "std")]
    {
        return raster_runtime::resolve_internal_value(&reference);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = reference;
        Err(raster_core::Error::Other(alloc::format!(
            "Internal input resolution requires the `std` feature"
        )))
    }
}

pub fn resolve_internal_ok_value<T: DeserializeOwned + Serialize>(
    reference: InternalRef,
) -> raster_core::Result<raster_core::input::InternalValue<T>> {
    #[cfg(feature = "std")]
    {
        return raster_runtime::resolve_internal_ok_value(&reference);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = reference;
        Err(raster_core::Error::Other(alloc::format!(
            "Result-backed internal input resolution requires the `std` feature"
        )))
    }
}

#[cfg(feature = "std")]
pub fn store_internal_value<T: Serialize>(value: &T) -> raster_core::Result<InternalRef> {
    raster_runtime::store_internal_value(value)
}

pub fn materialize_auth_return<T, A>(value: A) -> T
where
    T: DeserializeOwned + Serialize,
    A: IntoAuthValue<T>,
{
    into_auth_value::<T, _>(value)
        .unwrap_or_else(|error| panic!("Failed to materialize Raster auth return: {}", error))
        .into_inner()
}

pub fn materialize_auth_result<T, A>(
    value: core::result::Result<A, String>,
) -> core::result::Result<T, String>
where
    T: DeserializeOwned + Serialize,
    A: IntoAuthValue<T>,
{
    value.map(|arg| {
        into_auth_value::<T, A>(arg)
            .unwrap_or_else(|error| panic!("Failed to materialize Raster auth result: {}", error))
            .into_inner()
    })
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
