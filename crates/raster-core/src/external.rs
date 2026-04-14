//! External input marker and resolved value types.

use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;
use serde::{Deserialize, Serialize};

/// A lightweight reference to a named external input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ExternalRef {
    pub name: String,
}

impl ExternalRef {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// A typed external input reference used at user-facing function boundaries.
///
/// The generic parameter ties the reference to the payload type that will be
/// resolved later, allowing Raster to enforce serde trait bounds on external
/// inputs through the type system.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct External<T> {
    pub reference: ExternalRef,
    marker: PhantomData<fn() -> T>,
}

impl<T> External<T> {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            reference: ExternalRef::new(name),
            marker: PhantomData,
        }
    }

    pub fn name(&self) -> &str {
        &self.reference.name
    }

    pub fn into_ref(self) -> ExternalRef {
        self.reference
    }
}

/// A resolved external input carrying both identity metadata and the typed value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ExternalValue<T> {
    pub name: String,
    #[serde(default)]
    pub commitment: Option<String>,
    #[serde(default)]
    pub bytes: Vec<u8>,
    pub value: T,
}

impl<T> ExternalValue<T> {
    pub fn new(
        name: impl Into<String>,
        commitment: Option<String>,
        bytes: Vec<u8>,
        value: T,
    ) -> Self {
        Self {
            name: name.into(),
            commitment,
            bytes,
            value,
        }
    }

    pub fn into_inner(self) -> T {
        self.value
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
