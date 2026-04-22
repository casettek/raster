//! External input marker and resolved value types.

use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;
use serde::{Deserialize, Serialize};

#[cfg(feature = "std")]
use std::collections::BTreeMap;

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

/// A private file-backed external input declared inside `input.json`.
pub type ExternalInputPathEntry = String;

/// A public external input commitment declared inside `input_manifest.json`.
pub type ExternalInputManifestEntry = String;

/// A private JSON input document used by the native whole-program runner.
///
/// Each top-level field may be either:
/// - an inline JSON value, or
/// - an external path entry encoded as `{ "path": "..." }`
#[cfg(feature = "std")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum InputDocumentEntry {
    ExternalPath { path: ExternalInputPathEntry },
    Inline(serde_json::Value),
}

#[cfg(feature = "std")]
impl InputDocumentEntry {
    pub fn as_external_path(&self) -> Option<&str> {
        match self {
            Self::ExternalPath { path } => Some(path.as_str()),
            Self::Inline(_) => None,
        }
    }

    pub fn to_json_value(&self) -> serde_json::Value {
        match self {
            Self::ExternalPath { path } => serde_json::Value::String(path.clone()),
            Self::Inline(value) => value.clone(),
        }
    }
}

#[cfg(feature = "std")]
pub type InputDocument = BTreeMap<String, InputDocumentEntry>;

/// A public JSON manifest document that describes the commitments for externals.
///
/// Each top-level field may be either:
/// - an inline JSON value, or
/// - an external commitment entry encoded as `{ "external_commitment": "..." }`
#[cfg(feature = "std")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum InputManifestEntry {
    ExternalCommitment {
        external_commitment: ExternalInputManifestEntry,
    },
    Inline(serde_json::Value),
}

#[cfg(feature = "std")]
impl InputManifestEntry {
    pub fn as_external_commitment(&self) -> Option<&str> {
        match self {
            Self::ExternalCommitment {
                external_commitment,
            } => Some(external_commitment.as_str()),
            Self::Inline(_) => None,
        }
    }
}

#[cfg(feature = "std")]
pub type InputManifestDocument = BTreeMap<String, InputManifestEntry>;

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;

    #[test]
    fn parses_mixed_input_document_entries() {
        let document: InputDocument = serde_json::from_str(
            r#"{
                "count": 7,
                "user": "alice",
                "payload": { "path": "payload.bin" }
            }"#,
        )
        .unwrap();

        assert_eq!(
            document.get("payload").and_then(InputDocumentEntry::as_external_path),
            Some("payload.bin")
        );
        assert_eq!(
            document.get("count").map(InputDocumentEntry::to_json_value),
            Some(serde_json::json!(7))
        );
        assert_eq!(
            document.get("user").map(InputDocumentEntry::to_json_value),
            Some(serde_json::json!("alice"))
        );
    }

    #[test]
    fn parses_mixed_manifest_document_entries() {
        let document: InputManifestDocument = serde_json::from_str(
            r#"{
                "payload": {
                    "external_commitment": "abc123"
                },
                "inline_value": 7
            }"#,
        )
        .unwrap();

        assert_eq!(
            document
                .get("payload")
                .and_then(InputManifestEntry::as_external_commitment),
            Some("abc123")
        );
        assert!(matches!(
            document.get("inline_value"),
            Some(InputManifestEntry::Inline(value)) if value == &serde_json::json!(7)
        ));
    }
}
