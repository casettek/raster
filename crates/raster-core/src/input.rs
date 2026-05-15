//! External input marker and resolved value types.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::marker::PhantomData;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

#[cfg(feature = "std")]
use std::collections::BTreeMap;

/// A lightweight reference to a named external input.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ExternalRef {
    pub name: String,
    #[serde(default)]
    pub selector: SelectorPath,
}

impl ExternalRef {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            selector: SelectorPath::default(),
        }
    }

    pub fn with_selector(name: impl Into<String>, selector: SelectorPath) -> Self {
        Self {
            name: name.into(),
            selector,
        }
    }
}

/// A structured path describing a selected sub-value inside an external input.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SelectorPath {
    #[serde(default)]
    pub segments: Vec<SelectorSegment>,
}

impl SelectorPath {
    pub fn new(segments: Vec<SelectorSegment>) -> Self {
        Self { segments }
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SelectorSegment {
    Field(String),
    Index(u64),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SchemaNode {
    Leaf {
        type_name: String,
    },
    Struct {
        type_name: String,
        fields: Vec<SchemaField>,
    },
    List {
        type_name: String,
        element: Box<SchemaNode>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SchemaField {
    pub name: String,
    pub label: String,
    pub schema: Box<SchemaNode>,
}

impl SchemaField {
    pub fn new(name: impl Into<String>, label: impl Into<String>, schema: SchemaNode) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            schema: Box::new(schema),
        }
    }
}

pub trait Selectable {
    fn schema() -> SchemaNode;
}

pub trait Merklized: Selectable + Serialize {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct StructProofSibling {
    pub label: String,
    pub hash: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ListProofDirection {
    Left,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ListProofSibling {
    pub direction: ListProofDirection,
    pub hash: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SelectionProofStep {
    Struct {
        label: String,
        siblings: Vec<StructProofSibling>,
    },
    List {
        index: u64,
        len: u64,
        siblings: Vec<ListProofSibling>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SelectionProof {
    pub path: SelectorPath,
    pub root_hash: Vec<u8>,
    pub steps: Vec<SelectionProofStep>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SelectedPayload {
    #[serde(default)]
    pub bytes: Vec<u8>,
    #[serde(default)]
    pub proof: SelectionProof,
}

fn selection_hash(parts: &[&[u8]]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().to_vec()
}

pub fn verify_selection_proof(selected_bytes: &[u8], proof: &SelectionProof) -> bool {
    let mut current_hash = selection_hash(&[b"leaf", selected_bytes]);
    for step in proof.steps.iter().rev() {
        current_hash = match step {
            SelectionProofStep::Struct { label, siblings } => {
                let mut parts: Vec<Vec<u8>> = Vec::with_capacity(siblings.len() + 2);
                parts.push(b"struct".to_vec());
                let mut inserted = false;
                for sibling in siblings {
                    if !inserted && label <= &sibling.label {
                        parts.push(label.as_bytes().to_vec());
                        parts.push(current_hash.clone());
                        inserted = true;
                    }
                    parts.push(sibling.label.as_bytes().to_vec());
                    parts.push(sibling.hash.clone());
                }
                if !inserted {
                    parts.push(label.as_bytes().to_vec());
                    parts.push(current_hash.clone());
                }
                let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
                selection_hash(&refs)
            }
            SelectionProofStep::List {
                index: _,
                len,
                siblings,
            } => {
                let mut hash = current_hash;
                for sibling in siblings {
                    hash = match sibling.direction {
                        ListProofDirection::Left => {
                            selection_hash(&[b"list-node", sibling.hash.as_slice(), hash.as_slice()])
                        }
                        ListProofDirection::Right => {
                            selection_hash(&[b"list-node", hash.as_slice(), sibling.hash.as_slice()])
                        }
                    };
                }
                selection_hash(&[b"list-root", &len.to_le_bytes(), hash.as_slice()])
            }
        };
    }
    current_hash == proof.root_hash
}

impl From<&str> for SelectorSegment {
    fn from(value: &str) -> Self {
        Self::Field(value.into())
    }
}

impl From<String> for SelectorSegment {
    fn from(value: String) -> Self {
        Self::Field(value)
    }
}

impl From<usize> for SelectorSegment {
    fn from(value: usize) -> Self {
        Self::Index(value as u64)
    }
}

impl From<u64> for SelectorSegment {
    fn from(value: u64) -> Self {
        Self::Index(value)
    }
}

impl From<u32> for SelectorSegment {
    fn from(value: u32) -> Self {
        Self::Index(value as u64)
    }
}

impl From<u16> for SelectorSegment {
    fn from(value: u16) -> Self {
        Self::Index(value as u64)
    }
}

impl From<u8> for SelectorSegment {
    fn from(value: u8) -> Self {
        Self::Index(value as u64)
    }
}

impl From<i32> for SelectorSegment {
    fn from(value: i32) -> Self {
        Self::Index(value as u64)
    }
}

/// A caller-owned external selection passed through `external!(...)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExternalSelection {
    pub reference: ExternalRef,
}

impl ExternalSelection {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            reference: ExternalRef::new(name),
        }
    }

    pub fn with_selector(name: impl Into<String>, selector: SelectorPath) -> Self {
        Self {
            reference: ExternalRef::with_selector(name, selector),
        }
    }

    pub fn name(&self) -> &str {
        &self.reference.name
    }

    pub fn selector(&self) -> &SelectorPath {
        &self.reference.selector
    }

    pub fn into_ref(self) -> ExternalRef {
        self.reference
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

    pub fn selector(&self) -> &SelectorPath {
        &self.reference.selector
    }

    pub fn into_ref(self) -> ExternalRef {
        self.reference
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ExternalArgInfo {
    pub name: String,
    #[serde(default)]
    pub selector: SelectorPath,
    #[serde(default)]
    pub commitment: Option<String>,
    #[serde(default)]
    pub bytes: Vec<u8>,
    #[serde(default)]
    pub selected: SelectedPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ArgKind {
    Inline,
    External(ExternalArgInfo),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ResolvedArg<T> {
    pub value: T,
    pub kind: ArgKind,
}

impl<T> ResolvedArg<T> {
    pub fn inline(value: T) -> Self {
        Self {
            value,
            kind: ArgKind::Inline,
        }
    }

    pub fn external(value: T, info: ExternalArgInfo) -> Self {
        Self {
            value,
            kind: ArgKind::External(info),
        }
    }

    pub fn kind(&self) -> &ArgKind {
        &self.kind
    }

    pub fn into_inner(self) -> T {
        self.value
    }

    pub fn external_info(&self) -> Option<&ExternalArgInfo> {
        match &self.kind {
            ArgKind::Inline => None,
            ArgKind::External(info) => Some(info),
        }
    }
}

/// A resolved external input carrying both identity metadata and the typed value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ExternalValue<T> {
    pub name: String,
    #[serde(default)]
    pub selector: SelectorPath,
    #[serde(default)]
    pub commitment: Option<String>,
    #[serde(default)]
    pub bytes: Vec<u8>,
    #[serde(default)]
    pub selected: SelectedPayload,
    pub value: T,
}

impl<T> ExternalValue<T> {
    pub fn new(
        name: impl Into<String>,
        selector: SelectorPath,
        commitment: Option<String>,
        bytes: Vec<u8>,
        selected: SelectedPayload,
        value: T,
    ) -> Self {
        Self {
            name: name.into(),
            selector,
            commitment,
            bytes,
            selected,
            value,
        }
    }

    pub fn into_inner(self) -> T {
        self.value
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn selected(&self) -> &SelectedPayload {
        &self.selected
    }
}

macro_rules! impl_leaf_schema {
    ($($ty:ty => $name:expr),+ $(,)?) => {
        $(
            impl Selectable for $ty {
                fn schema() -> SchemaNode {
                    SchemaNode::Leaf {
                        type_name: $name.into(),
                    }
                }
            }

            impl Merklized for $ty {}
        )+
    };
}

impl_leaf_schema!(
    bool => "bool",
    String => "String",
    usize => "usize",
    u64 => "u64",
    u32 => "u32",
    u16 => "u16",
    u8 => "u8",
    i64 => "i64",
    i32 => "i32",
    i16 => "i16",
    i8 => "i8"
);

impl<T> Selectable for Vec<T>
where
    T: Selectable,
{
    fn schema() -> SchemaNode {
        SchemaNode::List {
            type_name: "Vec".into(),
            element: Box::new(T::schema()),
        }
    }
}

impl<T> Merklized for Vec<T> where T: Merklized {}

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
