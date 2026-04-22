use serde::{de::DeserializeOwned, Serialize};

pub use raster_core::input::{External, ExternalRef};

pub fn external<T>(name: &str) -> External<T> {
    External::new(name)
}

pub fn parse_program_input_value<T: DeserializeOwned + core::fmt::Debug>(
    name: Option<&str>,
) -> Option<T> {
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
    reference: External<T>,
    expected_name: &str,
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
