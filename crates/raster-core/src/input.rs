//! External input marker and resolved value types.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};
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
    pub bytes: Vec<u8>,
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
                        ListProofDirection::Left => selection_hash(&[
                            b"list-node",
                            sibling.hash.as_slice(),
                            hash.as_slice(),
                        ]),
                        ListProofDirection::Right => selection_hash(&[
                            b"list-node",
                            hash.as_slice(),
                            sibling.hash.as_slice(),
                        ]),
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ResolvedArg<T> {
    External(ExternalArg<T>),
    Inline(T),
}

impl<T> ResolvedArg<T> {
    pub fn inline(value: T) -> Self {
        Self::Inline(value)
    }

    pub fn external(value: ExternalArg<T>) -> Self {
        Self::External(value)
    }

    pub fn into_inner(self) -> T {
        match self {
            Self::External(external) => external.value,
            Self::Inline(value) => value,
        }
    }

    pub fn as_external(&self) -> Option<&ExternalArg<T>> {
        match self {
            Self::External(external) => Some(external),
            Self::Inline(_) => None,
        }
    }
}

/// A resolved external input carrying both identity metadata and the typed value.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ExternalArg<T> {
    pub name: String,
    pub selector: SelectorPath,
    pub commitment: Option<String>,
    pub selected: SelectedPayload,
    pub value: T,
}

impl<T> ExternalArg<T> {
    pub fn new(
        name: impl Into<String>,
        selector: SelectorPath,
        commitment: Option<String>,
        selected: SelectedPayload,
        value: T,
    ) -> Self {
        Self {
            name: name.into(),
            selector,
            commitment,
            selected,
            value,
        }
    }

    pub fn into_inner(self) -> T {
        self.value
    }

    pub fn bytes(&self) -> &[u8] {
        &self.selected.bytes
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

/// How the runtime should back a resolved external file in memory.
#[cfg(feature = "std")]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ExternalLoadPreference {
    Read,
    Mmap,
}

/// A public external input commitment declared inside `input_manifest.json`.
pub type ExternalInputManifestEntry = String;

/// A private input document that binds external names to serialized files.
///
/// Each top-level field must be an external path entry encoded as
/// `{ "path": "...", "load_preference": "read|mmap" }`. The referenced file is
/// decoded by the runtime using Raster's Postcard tile ABI codec.
#[cfg(feature = "std")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InputDocumentEntry {
    pub path: ExternalInputPathEntry,
    pub load_preference: ExternalLoadPreference,
}

#[cfg(feature = "std")]
impl InputDocumentEntry {
    pub fn path(&self) -> &str {
        self.path.as_str()
    }

    pub fn load_preference(&self) -> ExternalLoadPreference {
        self.load_preference
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
    use alloc::string::ToString;

    #[test]
    fn parses_external_input_entries_with_per_file_load_preference() {
        let document: InputDocument = serde_json::from_str(
            r#"{
                "payload": {
                    "path": "payload.bin",
                    "load_preference": "mmap"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            document.get("payload").map(InputDocumentEntry::path),
            Some("payload.bin")
        );
        assert_eq!(
            document
                .get("payload")
                .map(InputDocumentEntry::load_preference),
            Some(ExternalLoadPreference::Mmap)
        );
    }

    #[test]
    fn rejects_input_document_entries_without_load_preference() {
        let err = serde_json::from_str::<InputDocument>(
            r#"{
                "payload": { "path": "payload.bin" }
            }"#,
        )
        .expect_err("load preference is required");

        assert!(err.to_string().contains("missing field `load_preference`"));
    }

    #[test]
    fn rejects_input_document_entries_with_unknown_fields() {
        let err = serde_json::from_str::<InputDocument>(
            r#"{
                "payload": {
                    "path": "payload.bin",
                    "load_preference": "read",
                    "unexpected": true
                }
            }"#,
        )
        .expect_err("unknown input fields should be rejected");

        assert!(err.to_string().contains("unknown field `unexpected`"));
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

    #[test]
    fn resolved_arg_helpers_preserve_inline_values() {
        let arg = ResolvedArg::inline(7u64);

        assert!(arg.as_external().is_none());
        assert_eq!(arg.into_inner(), 7);
    }

    #[test]
    fn resolved_arg_helpers_preserve_external_metadata() {
        let selected = SelectedPayload {
            bytes: alloc::vec![1, 2, 3],
            proof: SelectionProof {
                path: SelectorPath::default(),
                root_hash: alloc::vec![4, 5, 6],
                steps: alloc::vec![],
            },
        };
        let arg = ResolvedArg::external(ExternalArg::new(
            "payload",
            SelectorPath::default(),
            Some("abc123".to_string()),
            selected.clone(),
            9u64,
        ));

        let external = arg.as_external().expect("expected external metadata");
        assert_eq!(external.name, "payload");
        assert_eq!(external.commitment.as_deref(), Some("abc123"));
        assert_eq!(external.selected, selected);
        assert_eq!(arg.into_inner(), 9);
    }
}
