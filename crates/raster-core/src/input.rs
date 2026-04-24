//! External input marker and resolved value types.

use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;
use serde::{Deserialize, Deserializer, Serialize};

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
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum InputDocumentEntry {
    Path { path: ExternalInputPathEntry },
    Inline(serde_json::Value),
}

#[cfg(feature = "std")]
impl InputDocumentEntry {
    pub fn as_path(&self) -> Option<&str> {
        match self {
            Self::Path { path } => Some(path.as_str()),
            Self::Inline(_) => None,
        }
    }

    pub fn as_inline_value(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Path { .. } => None,
            Self::Inline(value) => Some(value),
        }
    }

    pub fn to_json_value(&self) -> serde_json::Value {
        match self {
            Self::Path { path } => serde_json::Value::String(path.clone()),
            Self::Inline(value) => value.clone(),
        }
    }
}

#[cfg(feature = "std")]
impl<'de> Deserialize<'de> for InputDocumentEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::Object(map) => {
                if map.len() == 1 {
                    if let Some(serde_json::Value::String(path)) = map.get("path") {
                        return Ok(Self::Path { path: path.clone() });
                    }
                }
                Ok(Self::Inline(serde_json::Value::Object(map)))
            }
            other => Ok(Self::Inline(other)),
        }
    }
}

#[cfg(feature = "std")]
pub type InputDocument = BTreeMap<String, InputDocumentEntry>;

/// A public JSON manifest document that describes the commitments for externals.
///
/// Each top-level field is a structured commitment entry encoded as:
/// `{ "type": "sha256", "commitment": "..." }`
#[cfg(feature = "std")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputCommitmentType {
    Sha256,
}

#[cfg(feature = "std")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InputManifestEntry {
    #[serde(rename = "type")]
    pub commitment_type: InputCommitmentType,
    pub commitment: ExternalInputManifestEntry,
}

#[cfg(feature = "std")]
impl InputManifestEntry {
    pub fn as_sha256_commitment(&self) -> Option<&str> {
        match self.commitment_type {
            InputCommitmentType::Sha256 => Some(self.commitment.as_str()),
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
            document.get("payload").and_then(InputDocumentEntry::as_path),
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
    fn parses_structured_manifest_entries() {
        let document: InputManifestDocument = serde_json::from_str(
            r#"{
                "payload": {
                    "type": "sha256",
                    "commitment": "abc123"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            document
                .get("payload")
                .and_then(InputManifestEntry::as_sha256_commitment),
            Some("abc123")
        );
    }
}
