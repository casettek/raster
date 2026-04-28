use serde::{de::DeserializeOwned, Serialize};
use alloc::vec::Vec;

pub use raster_core::input::{
    ArgKind, External, ExternalArgInfo, ExternalRef, ExternalSelection, ExternalValue,
    ResolvedArg, SelectorPath, SelectorSegment,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExternalArg {
    selection: ExternalSelection,
}

impl ExternalArg {
    pub fn new(name: &str) -> Self {
        Self {
            selection: ExternalSelection::new(name),
        }
    }

    pub fn with_selector(name: &str, selector: SelectorPath) -> Self {
        Self {
            selection: ExternalSelection::with_selector(name, selector),
        }
    }

    pub fn into_selection(self) -> ExternalSelection {
        self.selection
    }
}

pub fn external(name: &str) -> ExternalArg {
    ExternalArg::new(name)
}

pub fn external_with_selector(name: &str, selector: SelectorPath) -> ExternalArg {
    ExternalArg::with_selector(name, selector)
}

pub fn select(segments: Vec<SelectorSegment>) -> SelectorPath {
    SelectorPath::new(segments)
}

pub trait IntoResolvedArg<T> {
    fn into_resolved_arg(self, expected_external_name: Option<&str>) -> raster_core::Result<ResolvedArg<T>>;
}

impl<T> IntoResolvedArg<T> for T
where
    T: Serialize,
{
    fn into_resolved_arg(
        self,
        expected_external_name: Option<&str>,
    ) -> raster_core::Result<ResolvedArg<T>> {
        if let Some(expected_external_name) = expected_external_name {
            return Err(raster_core::Error::Other(alloc::format!(
                "Expected external input '{}', but received an inline argument",
                expected_external_name
            )));
        }

        Ok(ResolvedArg::inline(self))
    }
}

impl<T> IntoResolvedArg<T> for ExternalArg
where
    T: DeserializeOwned + Serialize,
{
    fn into_resolved_arg(
        self,
        expected_external_name: Option<&str>,
    ) -> raster_core::Result<ResolvedArg<T>> {
        let selection = self.into_selection();
        let value = resolve_external_value::<T>(selection, expected_external_name)?;
        Ok(ResolvedArg::external(
            value.value,
            ExternalArgInfo {
                name: value.name,
                selector: value.selector,
                commitment: value.commitment,
                bytes: value.bytes,
            },
        ))
    }
}

impl<T> IntoResolvedArg<T> for External<T>
where
    T: DeserializeOwned + Serialize,
{
    fn into_resolved_arg(
        self,
        expected_external_name: Option<&str>,
    ) -> raster_core::Result<ResolvedArg<T>> {
        ExternalArg {
            selection: ExternalSelection {
                reference: self.into_ref(),
            },
        }
        .into_resolved_arg(expected_external_name)
    }
}

pub fn into_resolved_arg<T, A>(
    arg: A,
    expected_external_name: Option<&str>,
) -> raster_core::Result<ResolvedArg<T>>
where
    A: IntoResolvedArg<T>,
{
    arg.into_resolved_arg(expected_external_name)
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
    expected_name: Option<&str>,
) -> raster_core::Result<raster_core::input::ExternalValue<T>> {
    #[cfg(feature = "std")]
    {
        return raster_runtime::resolve_external_value(reference, expected_name);
    }

    #[cfg(not(feature = "std"))]
    {
        let _ = reference;
        let _ = expected_name;
        Err(raster_core::Error::Other(alloc::format!(
            "External input resolution requires the `std` feature"
        )))
    }
}
