use raster_core::input::{
    ExternalInputManifestEntry, ExternalInputPathEntry, ExternalSelection, ExternalValue,
    InputDocument, InputDocumentEntry, InputManifestDocument, InputManifestEntry,
    ListProofDirection, ListProofSibling, Merklized, SchemaField, SchemaNode, Selectable,
    SelectedPayload, SelectionProof, SelectionProofStep, SelectorPath, SelectorSegment,
    StructProofSibling,
};
use raster_core::{Error, Result};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::format;
use std::fs;
use std::path::{Path, PathBuf};
use std::string::{String, ToString};
use std::sync::{Mutex, OnceLock};
use std::vec::Vec;

#[derive(Debug, Clone)]
enum ResolvedExternalInput {
    File {
        commitment: String,
        bytes: Vec<u8>,
    },
    InlineJson {
        commitment: String,
        bytes: Vec<u8>,
        value: Value,
    },
}

impl ResolvedExternalInput {
    fn commitment(&self) -> &str {
        match self {
            Self::File { commitment, .. } | Self::InlineJson { commitment, .. } => commitment,
        }
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Self::File { bytes, .. } | Self::InlineJson { bytes, .. } => bytes,
        }
    }
}

#[derive(Debug, Clone)]
struct ExternalInputSources {
    input_document: InputDocument,
    manifest_document: InputManifestDocument,
    input_base_dir: PathBuf,
}

static RESOLVED_EXTERNALS: OnceLock<Mutex<HashMap<String, ResolvedExternalInput>>> =
    OnceLock::new();

fn external_cache() -> &'static Mutex<HashMap<String, ResolvedExternalInput>> {
    RESOLVED_EXTERNALS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn load_external_input_sources_from_args() -> Option<Result<ExternalInputSources>> {
    let args: Vec<String> = std::env::args().collect();
    let raw_input = args
        .iter()
        .position(|a| a == "--input")
        .and_then(|pos| args.get(pos + 1))
        .cloned();
    let raw_manifest = args
        .iter()
        .position(|a| a == "--input-manifest")
        .and_then(|pos| args.get(pos + 1))
        .cloned();
    if raw_input.is_none() && raw_manifest.is_none() {
        return None;
    }
    Some(ExternalInputSources::from_input_args(
        raw_input.as_deref(),
        raw_manifest.as_deref(),
    ))
}

fn load_external_input_sources() -> Result<Option<ExternalInputSources>> {
    match load_external_input_sources_from_args() {
        Some(result) => Ok(Some(result?)),
        None => Ok(None),
    }
}

impl ExternalInputSources {
    fn parse_json_source<T>(raw_input: Option<&str>, label: &str) -> Result<(T, PathBuf)>
    where
        T: DeserializeOwned + Default,
    {
        let Some(raw_input) = raw_input else {
            return Ok((
                T::default(),
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ));
        };
        let path = Path::new(raw_input);
        if path.is_file() {
            let contents = fs::read_to_string(path).map_err(|e| {
                Error::Other(format!(
                    "Failed to read {} file '{}': {}",
                    label,
                    path.display(),
                    e
                ))
            })?;
            let root = serde_json::from_str(&contents).map_err(|e| {
                Error::Serialization(format!(
                    "Failed to parse {} file '{}' as JSON: {}",
                    label,
                    path.display(),
                    e
                ))
            })?;
            let base_dir = path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            Ok((root, base_dir))
        } else {
            let root = serde_json::from_str(raw_input).map_err(|e| {
                Error::Serialization(format!("Failed to parse {} argument as JSON: {}", label, e))
            })?;
            Ok((
                root,
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ))
        }
    }

    fn from_input_args(raw_input: Option<&str>, raw_manifest: Option<&str>) -> Result<Self> {
        let (input_document, input_base_dir) = Self::parse_json_source(raw_input, "input")?;
        let (manifest_document, _manifest_base_dir) =
            Self::parse_json_source(raw_manifest, "input manifest")?;

        Ok(Self {
            input_document,
            manifest_document,
            input_base_dir,
        })
    }

    fn get_input_entry(&self, name: &str) -> Option<&InputDocumentEntry> {
        self.input_document.get(name)
    }

    fn get_manifest_entry(&self, name: &str) -> Option<&InputManifestEntry> {
        self.manifest_document.get(name)
    }

    fn read_external_path_entry(&self, name: &str) -> Result<ExternalInputPathEntry> {
        let entry = self.get_input_entry(name).ok_or_else(|| {
            Error::Other(format!(
                "Missing external input '{}'. Expected a top-level input document field.",
                name
            ))
        })?;

        entry.as_path().map(str::to_owned).ok_or_else(|| {
            Error::Serialization(format!(
                "Expected external input '{}' to use {{\"path\": \"...\"}}",
                name
            ))
        })
    }

    fn read_external_commitment_entry(&self, name: &str) -> Result<ExternalInputManifestEntry> {
        let entry = self.get_manifest_entry(name).ok_or_else(|| {
            Error::Other(format!(
                "Missing public manifest entry for external input '{}'. Expected a top-level field in input_manifest.json.",
                name
            ))
        })?;

        entry
            .as_sha256_commitment()
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::Serialization(format!(
                    "Expected public manifest entry '{}' to use {{\"type\": \"sha256\", \"commitment\": \"...\"}}",
                    name
                ))
            })
    }

    fn project_input_value(&self, name: &str) -> Option<Value> {
        self.get_input_entry(name)
            .map(InputDocumentEntry::to_json_value)
    }

    fn project_input_document(&self) -> Value {
        let projected = self
            .input_document
            .iter()
            .map(|(name, entry)| (name.clone(), entry.to_json_value()))
            .collect::<serde_json::Map<String, Value>>();

        Value::Object(projected)
    }
}

fn resolve_cached_external(
    name: &str,
    sources: &ExternalInputSources,
) -> Result<ResolvedExternalInput> {
    if let Some(resolved) = external_cache().lock().unwrap().get(name).cloned() {
        return Ok(resolved);
    }

    let expected_commitment = sources.read_external_commitment_entry(name)?;
    let resolved = match sources.get_input_entry(name).ok_or_else(|| {
        Error::Other(format!(
            "Missing external input '{}'. Expected a top-level input document field.",
            name
        ))
    })? {
        InputDocumentEntry::Path { .. } => {
            let entry = sources.read_external_path_entry(name)?;
            let path = sources.input_base_dir.join(&entry);
            let bytes = fs::read(&path).map_err(|e| {
                Error::Other(format!(
                    "Failed to read external input '{}' from '{}': {}",
                    name,
                    path.display(),
                    e
                ))
            })?;
            verify_input_commitment(name, &bytes, &expected_commitment)?;
            ResolvedExternalInput::File {
                commitment: expected_commitment,
                bytes,
            }
        }
        InputDocumentEntry::Inline(value) => {
            let bytes = canonical_json_bytes(value)?;
            verify_input_commitment(name, &bytes, &expected_commitment)?;
            ResolvedExternalInput::InlineJson {
                commitment: expected_commitment,
                bytes,
                value: value.clone(),
            }
        }
    };

    let mut guard = external_cache().lock().unwrap();
    Ok(guard
        .entry(name.to_string())
        .or_insert_with(|| resolved.clone())
        .clone())
}

fn deserialize_resolved_value<T: DeserializeOwned>(
    name: &str,
    resolved: &ResolvedExternalInput,
) -> Result<T> {
    match resolved {
        ResolvedExternalInput::File { bytes, .. } => raster_core::postcard::from_bytes(bytes)
            .map_err(|e| {
                Error::Serialization(format!(
                    "Failed to deserialize external input '{}' from postcard bytes: {}",
                    name, e
                ))
            }),
        ResolvedExternalInput::InlineJson { value, .. } => {
            serde_json::from_value(value.clone()).map_err(|e| {
                Error::Serialization(format!(
                    "Failed to deserialize inline external input '{}' from JSON value: {}",
                    name, e
                ))
            })
        }
    }
}

fn dynamic_selected_payload<T: Serialize>(name: &str, value: &T, selector: &SelectorPath) -> Result<SelectedPayload> {
    let root_value = serde_json::to_value(value).map_err(|e| {
        Error::Serialization(format!(
            "Failed to project external input '{}' into JSON for selection proof: {}",
            name, e
        ))
    })?;
    prove_dynamic_selection(&root_value, selector)
}

fn typed_selected_payload<Root: Serialize + Selectable>(
    name: &str,
    value: &Root,
    selector: &SelectorPath,
) -> Result<SelectedPayload> {
    let root_value = serde_json::to_value(value).map_err(|e| {
        Error::Serialization(format!(
            "Failed to project external input '{}' into JSON for merkle selection: {}",
            name, e
        ))
    })?;
    let proven = prove_selection(&Root::schema(), &root_value, &selector.segments)?;

    Ok(SelectedPayload {
        bytes: proven.selected_bytes,
        proof: SelectionProof {
            path: selector.clone(),
            root_hash: proven.root_hash,
            steps: proven.steps,
        },
    })
}

fn external_value_from_parts<T>(
    name: &str,
    selector: SelectorPath,
    resolved: ResolvedExternalInput,
    selected: SelectedPayload,
    value: T,
) -> ExternalValue<T> {
    ExternalValue::new(
        name,
        selector,
        Some(resolved.commitment().to_string()),
        resolved.bytes().to_vec(),
        selected,
        value,
    )
}

fn select_json_value(root: &Value, selector: &SelectorPath) -> Result<Value> {
    let mut current = root;

    for segment in &selector.segments {
        current = match segment {
            SelectorSegment::Field(field) => current.get(field).ok_or_else(|| {
                Error::Other(format!(
                    "Selector field '{}' was not found in inline external input",
                    field
                ))
            })?,
            SelectorSegment::Index(index) => current
                .as_array()
                .and_then(|items| items.get(*index as usize))
                .ok_or_else(|| {
                    Error::Other(format!(
                        "Selector index '{}' was not found in inline external input",
                        index
                    ))
                })?,
        };
    }

    Ok(current.clone())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

fn normalize_hash(hash: &str) -> String {
    hash.trim().to_ascii_lowercase()
}

fn canonicalize_json_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));

            let mut out = serde_json::Map::new();
            for (key, value) in entries {
                out.insert(key.clone(), canonicalize_json_value(value));
            }

            Value::Object(out)
        }
        Value::Array(values) => Value::Array(values.iter().map(canonicalize_json_value).collect()),
        other => other.clone(),
    }
}

fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>> {
    serde_json::to_vec(&canonicalize_json_value(value)).map_err(|e| {
        Error::Serialization(format!(
            "Failed to encode inline input as canonical JSON bytes: {}",
            e
        ))
    })
}

fn schema_label(field: &SchemaField) -> String {
    if field.label.is_empty() {
        field.name.clone()
    } else {
        field.label.clone()
    }
}

fn selection_hash(parts: &[&[u8]]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().to_vec()
}

fn hash_leaf(value: &Value) -> Result<Vec<u8>> {
    let bytes = canonical_json_bytes(value)?;
    Ok(selection_hash(&[b"leaf", bytes.as_slice()]))
}

fn hash_struct(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut entries = entries.to_vec();
    entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    let mut parts: Vec<Vec<u8>> = Vec::with_capacity(entries.len() * 2 + 1);
    parts.push(b"struct".to_vec());
    for (label, hash) in entries {
        parts.push(label.into_bytes());
        parts.push(hash);
    }
    let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    selection_hash(&refs)
}

fn list_root_from_hashes(hashes: &[Vec<u8>]) -> Vec<u8> {
    let len = hashes.len() as u64;
    if hashes.is_empty() {
        return selection_hash(&[b"list-root", &len.to_le_bytes(), b"empty"]);
    }

    let mut level = hashes.to_vec();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            let last = level.last().cloned().unwrap();
            level.push(last);
        }

        let mut next = Vec::with_capacity(level.len() / 2);
        for pair in level.chunks(2) {
            next.push(selection_hash(&[
                b"list-node",
                pair[0].as_slice(),
                pair[1].as_slice(),
            ]));
        }
        level = next;
    }

    selection_hash(&[b"list-root", &len.to_le_bytes(), level[0].as_slice()])
}

fn list_root_and_proof(
    hashes: &[Vec<u8>],
    index: usize,
) -> Result<(Vec<u8>, Vec<ListProofSibling>)> {
    if index >= hashes.len() {
        return Err(Error::Other(format!(
            "Selector index '{}' was not found in inline external input",
            index
        )));
    }

    let len = hashes.len() as u64;
    if hashes.is_empty() {
        return Ok((
            selection_hash(&[b"list-root", &len.to_le_bytes(), b"empty"]),
            Vec::new(),
        ));
    }

    let mut siblings = Vec::new();
    let mut idx = index;
    let mut level = hashes.to_vec();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            let last = level.last().cloned().unwrap();
            level.push(last);
        }

        let sibling_index = if idx % 2 == 0 { idx + 1 } else { idx - 1 };
        siblings.push(ListProofSibling {
            direction: if idx % 2 == 0 {
                ListProofDirection::Right
            } else {
                ListProofDirection::Left
            },
            hash: level[sibling_index].clone(),
        });

        let mut next = Vec::with_capacity(level.len() / 2);
        for pair in level.chunks(2) {
            next.push(selection_hash(&[
                b"list-node",
                pair[0].as_slice(),
                pair[1].as_slice(),
            ]));
        }
        idx /= 2;
        level = next;
    }

    Ok((
        selection_hash(&[b"list-root", &len.to_le_bytes(), level[0].as_slice()]),
        siblings,
    ))
}

fn infer_schema(value: &Value) -> SchemaNode {
    match value {
        Value::Object(map) => {
            let mut fields: Vec<_> = map
                .iter()
                .map(|(name, child)| {
                    SchemaField::new(name.clone(), name.clone(), infer_schema(child))
                })
                .collect();
            fields.sort_by(|left, right| left.name.cmp(&right.name));
            SchemaNode::Struct {
                type_name: "DynamicObject".into(),
                fields,
            }
        }
        Value::Array(values) => {
            let element = values
                .first()
                .map(infer_schema)
                .unwrap_or(SchemaNode::Leaf {
                    type_name: "DynamicLeaf".into(),
                });
            SchemaNode::List {
                type_name: "DynamicList".into(),
                element: Box::new(element),
            }
        }
        _ => SchemaNode::Leaf {
            type_name: "DynamicLeaf".into(),
        },
    }
}

struct ProvenSelection {
    selected_value: Value,
    selected_bytes: Vec<u8>,
    selected_hash: Vec<u8>,
    root_hash: Vec<u8>,
    steps: Vec<SelectionProofStep>,
}

fn hash_schema_value(schema: &SchemaNode, value: &Value) -> Result<Vec<u8>> {
    match schema {
        SchemaNode::Leaf { .. } => hash_leaf(value),
        SchemaNode::Struct { fields, .. } => {
            let object = value.as_object().ok_or_else(|| {
                Error::Serialization(
                    "Expected object value while hashing schema-driven struct".into(),
                )
            })?;
            let mut entries = Vec::with_capacity(fields.len());
            for field in fields {
                let child = object.get(&field.name).ok_or_else(|| {
                    Error::Serialization(format!(
                        "Missing field '{}' in schema-driven value",
                        field.name
                    ))
                })?;
                entries.push((
                    schema_label(field),
                    hash_schema_value(&field.schema, child)?,
                ));
            }
            Ok(hash_struct(&entries))
        }
        SchemaNode::List { element, .. } => {
            let array = value.as_array().ok_or_else(|| {
                Error::Serialization("Expected array value while hashing schema-driven list".into())
            })?;
            let mut hashes = Vec::with_capacity(array.len());
            for child in array {
                hashes.push(hash_schema_value(element, child)?);
            }
            Ok(list_root_from_hashes(&hashes))
        }
    }
}

fn prove_selection(
    schema: &SchemaNode,
    value: &Value,
    segments: &[SelectorSegment],
) -> Result<ProvenSelection> {
    if segments.is_empty() {
        let selected_bytes = canonical_json_bytes(value)?;
        let selected_hash = hash_leaf(value)?;
        return Ok(ProvenSelection {
            selected_value: value.clone(),
            selected_bytes,
            selected_hash: selected_hash.clone(),
            root_hash: selected_hash,
            steps: Vec::new(),
        });
    }

    match (&segments[0], schema) {
        (SelectorSegment::Field(field_name), SchemaNode::Struct { fields, .. }) => {
            let object = value.as_object().ok_or_else(|| {
                Error::Serialization("Expected object value while resolving selected field".into())
            })?;
            let target_field = fields
                .iter()
                .find(|field| field.name == *field_name)
                .ok_or_else(|| {
                    Error::Other(format!("Selector field '{}' was not found", field_name))
                })?;
            let child_value = object.get(field_name).ok_or_else(|| {
                Error::Other(format!("Selector field '{}' was not found", field_name))
            })?;
            let child = prove_selection(&target_field.schema, child_value, &segments[1..])?;
            let target_label = schema_label(target_field);
            let mut siblings = Vec::new();
            let mut entries = Vec::with_capacity(fields.len());
            for field in fields {
                let label = schema_label(field);
                if field.name == *field_name {
                    entries.push((label.clone(), child.root_hash.clone()));
                } else {
                    let sibling_value = object.get(&field.name).ok_or_else(|| {
                        Error::Serialization(format!(
                            "Missing field '{}' in schema-driven value",
                            field.name
                        ))
                    })?;
                    let sibling_hash = hash_schema_value(&field.schema, sibling_value)?;
                    siblings.push(StructProofSibling {
                        label: label.clone(),
                        hash: sibling_hash.clone(),
                    });
                    entries.push((label, sibling_hash));
                }
            }
            siblings.sort_by(|left, right| left.label.cmp(&right.label));

            let mut steps = Vec::with_capacity(child.steps.len() + 1);
            steps.push(SelectionProofStep::Struct {
                label: target_label,
                siblings,
            });
            steps.extend(child.steps);

            Ok(ProvenSelection {
                selected_value: child.selected_value,
                selected_bytes: child.selected_bytes,
                selected_hash: child.selected_hash,
                root_hash: hash_struct(&entries),
                steps,
            })
        }
        (SelectorSegment::Index(index), SchemaNode::List { element, .. }) => {
            let array = value.as_array().ok_or_else(|| {
                Error::Serialization("Expected array value while resolving selected index".into())
            })?;
            let idx = *index as usize;
            let child_value = array
                .get(idx)
                .ok_or_else(|| Error::Other(format!("Selector index '{}' was not found", index)))?;
            let child = prove_selection(element, child_value, &segments[1..])?;
            let mut hashes = Vec::with_capacity(array.len());
            for (position, item) in array.iter().enumerate() {
                if position == idx {
                    hashes.push(child.root_hash.clone());
                } else {
                    hashes.push(hash_schema_value(element, item)?);
                }
            }
            let (root_hash, siblings) = list_root_and_proof(&hashes, idx)?;
            let mut steps = Vec::with_capacity(child.steps.len() + 1);
            steps.push(SelectionProofStep::List {
                index: *index,
                len: array.len() as u64,
                siblings,
            });
            steps.extend(child.steps);
            Ok(ProvenSelection {
                selected_value: child.selected_value,
                selected_bytes: child.selected_bytes,
                selected_hash: child.selected_hash,
                root_hash,
                steps,
            })
        }
        (SelectorSegment::Field(field_name), _) => Err(Error::Other(format!(
            "Selector field '{}' was not found in selected value",
            field_name
        ))),
        (SelectorSegment::Index(index), _) => Err(Error::Other(format!(
            "Selector index '{}' was not found in selected value",
            index
        ))),
    }
}

fn prove_dynamic_selection(root: &Value, selector: &SelectorPath) -> Result<SelectedPayload> {
    let schema = infer_schema(root);
    let selection = prove_selection(&schema, root, &selector.segments)?;
    Ok(SelectedPayload {
        bytes: selection.selected_bytes,
        proof: SelectionProof {
            path: selector.clone(),
            root_hash: selection.root_hash,
            steps: selection.steps,
        },
    })
}

fn verify_input_commitment(name: &str, bytes: &[u8], expected_commitment: &str) -> Result<()> {
    let actual_hash = sha256_hex(bytes);
    if normalize_hash(expected_commitment) != actual_hash {
        return Err(Error::Other(format!(
            "External input '{}' failed integrity check. Expected SHA256 {}, got {}",
            name, expected_commitment, actual_hash
        )));
    }

    Ok(())
}

/// Parse the private program input from `--input` and deserialize the full value.
pub fn parse_program_input<T: DeserializeOwned>() -> Option<T> {
    parse_program_input_value(None)
}

/// Parse either a named top-level field from the private `input.json` document,
/// or the full document when `name` is `None`.
pub fn parse_program_input_value<T: DeserializeOwned>(name: Option<&str>) -> Option<T> {
    let sources = load_external_input_sources().ok()??;

    if let Some(name) = name {
        if let Some(value) = sources.project_input_value(name) {
            return serde_json::from_value(value).ok();
        }
    }

    serde_json::from_value(sources.project_input_document()).ok()
}

pub fn resolve_external_value<T: DeserializeOwned + Serialize>(
    reference: ExternalSelection,
) -> Result<ExternalValue<T>> {
    let sources = load_external_input_sources()?.ok_or_else(|| {
        Error::Other(
            "External input resolution requires CLI input context from --input and --input-manifest"
                .into(),
        )
    })?;

    let resolved = resolve_cached_external(reference.name(), &sources)?;
    if reference.selector().is_empty() {
        let value = deserialize_resolved_value(reference.name(), &resolved)?;
        let selected = dynamic_selected_payload(reference.name(), &value, reference.selector())?;
        return Ok(external_value_from_parts(
            reference.name(),
            reference.selector().clone(),
            resolved,
            selected,
            value,
        ));
    }

    match sources.get_input_entry(reference.name()) {
        Some(InputDocumentEntry::Inline(value)) => {
            let selected_value = select_json_value(value, reference.selector())?;
            let selected_payload = prove_dynamic_selection(value, reference.selector())?;
            let selected_value: T = serde_json::from_value(selected_value).map_err(|e| {
                Error::Serialization(format!(
                    "Failed to deserialize selected external input '{}' from JSON value: {}",
                    reference.name(),
                    e
                ))
            })?;
            Ok(external_value_from_parts(
                reference.name(),
                reference.selector().clone(),
                resolved,
                selected_payload,
                selected_value,
            ))
        }
        Some(InputDocumentEntry::Path { .. }) => Err(Error::Other(format!(
            "External selector for '{}' currently requires an inline JSON input source",
            reference.name()
        ))),
        None => Err(Error::Other(format!(
            "Missing external input '{}'. Expected a top-level input document field.",
            reference.name()
        ))),
    }
}

pub fn resolve_typed_external_value<Root, T>(
    reference: ExternalSelection,
) -> Result<ExternalValue<T>>
where
    Root: DeserializeOwned + Serialize + Selectable + Merklized,
    T: DeserializeOwned + Serialize,
{
    let sources = load_external_input_sources()?.ok_or_else(|| {
        Error::Other(
            "External input resolution requires CLI input context from --input and --input-manifest"
                .into(),
        )
    })?;

    let resolved = resolve_cached_external(reference.name(), &sources)?;
    let root: Root = deserialize_resolved_value(reference.name(), &resolved)?;
    let root_value = serde_json::to_value(&root).map_err(|e| {
        Error::Serialization(format!(
            "Failed to project external input '{}' into JSON for merkle selection: {}",
            reference.name(),
            e
        ))
    })?;
    let proven = prove_selection(&Root::schema(), &root_value, &reference.selector().segments)?;
    let typed_selected: T = serde_json::from_value(proven.selected_value.clone()).map_err(|e| {
        Error::Serialization(format!(
            "Failed to deserialize selected external input '{}' from schema-driven value: {}",
            reference.name(),
            e
        ))
    })?;
    let selected = typed_selected_payload(reference.name(), &root, reference.selector())?;

    Ok(external_value_from_parts(
        reference.name(),
        reference.selector().clone(),
        resolved,
        selected,
        typed_selected,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::input::{
        verify_selection_proof, Merklized, SchemaField, SchemaNode, Selectable,
    };
    use serde::{Deserialize, Serialize};
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::vec;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Flight {
        id: u32,
        seats: u16,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct MixedProgramInput {
        count: u32,
        flight_data: String,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
    struct Address {
        lines: Vec<String>,
        indexes: Vec<u32>,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
    struct PersonalData {
        age: usize,
        name: String,
        addresses: Vec<Address>,
    }

    impl Selectable for Address {
        fn schema() -> SchemaNode {
            SchemaNode::Struct {
                type_name: "Address".into(),
                fields: vec![
                    SchemaField::new("lines", "lines", <Vec<String> as Selectable>::schema()),
                    SchemaField::new("indexes", "indexes", <Vec<u32> as Selectable>::schema()),
                ],
            }
        }
    }

    impl Merklized for Address {}

    impl Selectable for PersonalData {
        fn schema() -> SchemaNode {
            SchemaNode::Struct {
                type_name: "PersonalData".into(),
                fields: vec![
                    SchemaField::new("age", "age", <usize as Selectable>::schema()),
                    SchemaField::new("name", "name", <String as Selectable>::schema()),
                    SchemaField::new(
                        "addresses",
                        "addresses",
                        <Vec<Address> as Selectable>::schema(),
                    ),
                ],
            }
        }
    }

    impl Merklized for PersonalData {}

    fn unique_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("raster-input-test-{}", nanos))
    }

    fn clear_external_cache() {
        external_cache().lock().unwrap().clear();
    }

    fn write_external_documents(
        dir: &Path,
        hash: &str,
        input_body: &str,
        manifest_body: &str,
    ) -> (PathBuf, PathBuf) {
        let input_path = dir.join("input.json");
        fs::write(&input_path, input_body).unwrap();

        let manifest_path = dir.join("input_manifest.json");
        fs::write(&manifest_path, manifest_body.replace("{hash}", hash)).unwrap();

        (input_path, manifest_path)
    }

    #[test]
    fn reads_file_backed_input_and_resolves_relative_external() {
        clear_external_cache();
        let input_name = "flight_data_relative";
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let data_path = dir.join("flights.bin");
        let flights = vec![Flight { id: 7, seats: 180 }];
        let bytes = raster_core::postcard::to_allocvec(&flights).unwrap();
        fs::write(&data_path, &bytes).unwrap();
        let hash = sha256_hex(&bytes);

        let (input_path, manifest_path) = write_external_documents(
            &dir,
            &hash,
            r#"{"flight_data_relative":{"path":"flights.bin"}}"#,
            r#"{"flight_data_relative":{"type":"sha256","commitment":"{hash}"}}"#,
        );

        let sources =
            ExternalInputSources::from_input_args(input_path.to_str(), manifest_path.to_str())
                .unwrap();
        let entry = sources.read_external_path_entry(input_name).unwrap();
        assert_eq!(entry, "flights.bin");
        assert_eq!(
            sources.read_external_commitment_entry(input_name).unwrap(),
            hash
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn projects_mixed_input_document_back_to_plain_json() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let input_path = dir.join("input.json");
        fs::write(
            &input_path,
            r#"{
                "count": 7,
                "flight_data": { "path": "flights.bin" }
            }"#,
        )
        .unwrap();

        let sources = ExternalInputSources::from_input_args(input_path.to_str(), None).unwrap();
        let parsed: MixedProgramInput =
            serde_json::from_value(sources.project_input_document()).unwrap();

        assert_eq!(
            parsed,
            MixedProgramInput {
                count: 7,
                flight_data: "flights.bin".to_string(),
            }
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn caches_resolved_external_bytes_by_name() {
        clear_external_cache();
        let input_name = "flight_data_cached";
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let data_path = dir.join("flights.bin");
        let flights = vec![Flight { id: 7, seats: 180 }];
        let bytes = raster_core::postcard::to_allocvec(&flights).unwrap();
        fs::write(&data_path, &bytes).unwrap();
        let hash = sha256_hex(&bytes);

        let (input_path, manifest_path) = write_external_documents(
            &dir,
            &hash,
            r#"{"flight_data_cached":{"path":"flights.bin"}}"#,
            r#"{"flight_data_cached":{"type":"sha256","commitment":"{hash}"}}"#,
        );

        let sources =
            ExternalInputSources::from_input_args(input_path.to_str(), manifest_path.to_str())
                .unwrap();
        let first = resolve_cached_external(input_name, &sources).unwrap();

        let changed_bytes =
            raster_core::postcard::to_allocvec(&vec![Flight { id: 9, seats: 42 }]).unwrap();
        fs::write(&data_path, &changed_bytes).unwrap();

        let second = resolve_cached_external(input_name, &sources).unwrap();

        assert_eq!(first.bytes(), bytes.as_slice());
        assert_eq!(second.bytes(), bytes.as_slice());
        assert_eq!(first.commitment(), hash);
        assert_eq!(second.commitment(), hash);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_external_inputs_with_wrong_manifest_commitment() {
        clear_external_cache();
        let input_name = "flight_data_bad_manifest";
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let data_path = dir.join("flights.bin");
        let flights = vec![Flight { id: 7, seats: 180 }];
        let bytes = raster_core::postcard::to_allocvec(&flights).unwrap();
        fs::write(&data_path, &bytes).unwrap();

        let (input_path, manifest_path) = write_external_documents(
            &dir,
            "deadbeef",
            r#"{"flight_data_bad_manifest":{"path":"flights.bin"}}"#,
            r#"{"flight_data_bad_manifest":{"type":"sha256","commitment":"{hash}"}}"#,
        );

        let sources =
            ExternalInputSources::from_input_args(input_path.to_str(), manifest_path.to_str())
                .unwrap();
        let err = resolve_cached_external(input_name, &sources).expect_err("hash mismatch");

        assert!(err
            .to_string()
            .contains("External input 'flight_data_bad_manifest' failed integrity check"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolves_inline_inputs_through_external_value_path() {
        clear_external_cache();
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let inline_bytes = canonical_json_bytes(&serde_json::json!(123)).unwrap();
        let hash = sha256_hex(&inline_bytes);
        let (input_path, manifest_path) = write_external_documents(
            &dir,
            &hash,
            r#"{"seed":123}"#,
            r#"{"seed":{"type":"sha256","commitment":"{hash}"}}"#,
        );

        let sources =
            ExternalInputSources::from_input_args(input_path.to_str(), manifest_path.to_str())
                .unwrap();
        let resolved = resolve_cached_external("seed", &sources).unwrap();
        let value = deserialize_resolved_value::<u64>("seed", &resolved).unwrap();
        let selected = dynamic_selected_payload("seed", &value, &SelectorPath::default()).unwrap();

        assert_eq!(resolved.bytes(), inline_bytes.as_slice());
        assert_eq!(resolved.commitment(), hash);
        assert_eq!(value, 123);
        assert_eq!(selected.bytes, inline_bytes);
        assert!(verify_selection_proof(&selected.bytes, &selected.proof));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn whole_value_dynamic_selection_produces_verifiable_payload() {
        let selected = dynamic_selected_payload("seed", &123u64, &SelectorPath::default()).unwrap();

        assert_eq!(selected.bytes, canonical_json_bytes(&serde_json::json!(123)).unwrap());
        assert!(selected.proof.path.is_empty());
        assert!(verify_selection_proof(&selected.bytes, &selected.proof));
    }

    #[test]
    fn parse_program_input_value_returns_none_without_cli_context() {
        let parsed = parse_program_input_value::<serde_json::Value>(Some("missing"));

        assert!(parsed.is_none());
    }

    #[test]
    fn resolve_external_value_errors_without_cli_context() {
        let err = resolve_external_value::<Flight>(ExternalSelection::new("flight_data"))
            .expect_err("missing CLI context should fail");

        assert_eq!(
            err.to_string(),
            "External input resolution requires CLI input context from --input and --input-manifest"
        );
    }

    #[test]
    fn resolves_typed_nested_selection_with_merkle_proof() {
        clear_external_cache();
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let data = PersonalData {
            age: 25,
            name: "John".to_string(),
            addresses: vec![Address {
                lines: vec!["221B Baker Street".to_string(), "Flat B".to_string()],
                indexes: vec![7, 42],
            }],
        };
        let bytes = raster_core::postcard::to_allocvec(&data).unwrap();
        fs::write(dir.join("personal_data.bin"), &bytes).unwrap();
        let hash = sha256_hex(&bytes);
        let (input_path, manifest_path) = write_external_documents(
            &dir,
            &hash,
            r#"{"personal_data_bin":{"path":"personal_data.bin"}}"#,
            r#"{"personal_data_bin":{"type":"sha256","commitment":"{hash}"}}"#,
        );

        let sources =
            ExternalInputSources::from_input_args(input_path.to_str(), manifest_path.to_str())
                .unwrap();
        let resolved = resolve_cached_external("personal_data_bin", &sources).unwrap();
        let root: PersonalData = raster_core::postcard::from_bytes(resolved.bytes()).unwrap();
        let root_value = serde_json::to_value(&root).unwrap();
        let selector = SelectorPath::new(vec![
            SelectorSegment::from("addresses"),
            SelectorSegment::from(0usize),
            SelectorSegment::from("lines"),
            SelectorSegment::from(1usize),
        ]);
        let proven =
            prove_selection(&PersonalData::schema(), &root_value, &selector.segments).unwrap();

        let selected = SelectedPayload {
            bytes: proven.selected_bytes.clone(),
            proof: SelectionProof {
                path: selector,
                root_hash: proven.root_hash.clone(),
                steps: proven.steps.clone(),
            },
        };

        assert_eq!(
            serde_json::from_value::<String>(proven.selected_value).unwrap(),
            "Flat B"
        );
        assert!(verify_selection_proof(&selected.bytes, &selected.proof));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn whole_value_typed_selection_produces_verifiable_payload() {
        let root = PersonalData {
            age: 25,
            name: "John".to_string(),
            addresses: vec![Address {
                lines: vec!["221B Baker Street".to_string()],
                indexes: vec![7],
            }],
        };

        let selected =
            typed_selected_payload::<PersonalData>("personal_data", &root, &SelectorPath::default())
                .unwrap();

        assert!(selected.proof.path.is_empty());
        assert!(verify_selection_proof(&selected.bytes, &selected.proof));
    }
}
