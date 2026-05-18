use raster_core::input::{
    ExternalArg, ExternalSelection, ListProofDirection, ListProofSibling, Merklized, SchemaField,
    SchemaNode, Selectable, SelectedPayload, SelectionProof, SelectionProofStep, SelectorPath,
    SelectorSegment, StructProofSibling,
};
use raster_core::{Error, Result};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

use crate::external_storage::{ExternalStorageManager, ResolvedExternalData};

fn load_external_storage() -> Result<Option<ExternalStorageManager>> {
    ExternalStorageManager::from_cli_args()
}

fn dynamic_selected_payload<T: Serialize>(
    name: &str,
    value: &T,
    selector: &SelectorPath,
) -> Result<SelectedPayload> {
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
    resolved: ResolvedExternalData,
    selected: SelectedPayload,
    value: T,
) -> ExternalArg<T> {
    ExternalArg::new(
        name,
        selector,
        Some(resolved.commitment().to_string()),
        selected,
        value,
    )
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
            "Failed to encode selected value as canonical JSON bytes: {}",
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
            "Selector index '{}' was not found in external input",
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

pub fn resolve_external_value<T: DeserializeOwned + Serialize>(
    reference: ExternalSelection,
) -> Result<ExternalArg<T>> {
    let storage = load_external_storage()?.ok_or_else(|| {
        Error::Other(
            "External input resolution requires CLI input context from --input and --input-manifest"
                .into(),
        )
    })?;

    let resolved = storage.resolve(reference.name())?;
    if reference.selector().is_empty() {
        let value = resolved.deserialize()?;
        let selected = dynamic_selected_payload(reference.name(), &value, reference.selector())?;
        return Ok(external_value_from_parts(
            reference.name(),
            reference.selector().clone(),
            resolved,
            selected,
            value,
        ));
    }

    Err(Error::Other(format!(
        "External selector for '{}' requires typed_external<Root>(...) with postcard path inputs",
        reference.name()
    )))
}

pub fn resolve_typed_external_value<Root, T>(reference: ExternalSelection) -> Result<ExternalArg<T>>
where
    Root: DeserializeOwned + Serialize + Selectable + Merklized,
    T: DeserializeOwned + Serialize,
{
    let storage = load_external_storage()?.ok_or_else(|| {
        Error::Other(
            "External input resolution requires CLI input context from --input and --input-manifest"
                .into(),
        )
    })?;

    let resolved = storage.resolve(reference.name())?;
    let root: Root = resolved.deserialize()?;
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
    use crate::external_storage::{sha256_hex, ExternalStorageManager};
    use raster_core::input::{
        verify_selection_proof, Merklized, SchemaField, SchemaNode, Selectable,
    };
    use serde::{Deserialize, Serialize};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::vec;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Flight {
        id: u32,
        seats: u16,
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

    fn storage_manager(input_path: &Path, manifest_path: &Path) -> ExternalStorageManager {
        ExternalStorageManager::from_input_args(input_path.to_str(), manifest_path.to_str())
            .unwrap()
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
    fn resolves_file_backed_seed_through_external_value_path() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let bytes = raster_core::postcard::to_allocvec(&123u64).unwrap();
        fs::write(dir.join("seed.bin"), &bytes).unwrap();
        let hash = sha256_hex(&bytes);
        let (input_path, manifest_path) = write_external_documents(
            &dir,
            &hash,
            r#"{"seed":{"path":"seed.bin","load_preference":"read"}}"#,
            r#"{"seed":{"type":"sha256","commitment":"{hash}"}}"#,
        );

        let storage = storage_manager(&input_path, &manifest_path);
        let resolved = storage.resolve("seed").unwrap();
        let value: u64 = resolved.deserialize().unwrap();
        let selected = dynamic_selected_payload("seed", &value, &SelectorPath::default()).unwrap();

        assert_eq!(resolved.bytes(), bytes.as_slice());
        assert_eq!(resolved.commitment(), hash);
        assert_eq!(value, 123);
        assert_eq!(
            selected.bytes,
            canonical_json_bytes(&serde_json::json!(123)).unwrap()
        );
        assert!(verify_selection_proof(&selected.bytes, &selected.proof));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn whole_value_dynamic_selection_produces_verifiable_payload() {
        let selected = dynamic_selected_payload("seed", &123u64, &SelectorPath::default()).unwrap();

        assert_eq!(
            selected.bytes,
            canonical_json_bytes(&serde_json::json!(123)).unwrap()
        );
        assert!(selected.proof.path.is_empty());
        assert!(verify_selection_proof(&selected.bytes, &selected.proof));
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
            r#"{"personal_data_bin":{"path":"personal_data.bin","load_preference":"mmap"}}"#,
            r#"{"personal_data_bin":{"type":"sha256","commitment":"{hash}"}}"#,
        );

        let storage = storage_manager(&input_path, &manifest_path);
        let resolved = storage.resolve("personal_data_bin").unwrap();
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

        let selected = typed_selected_payload::<PersonalData>(
            "personal_data",
            &root,
            &SelectorPath::default(),
        )
        .unwrap();

        assert!(selected.proof.path.is_empty());
        assert!(verify_selection_proof(&selected.bytes, &selected.proof));
    }
}
