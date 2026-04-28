use raster_core::input::{
    ExternalInputManifestEntry, ExternalInputPathEntry, ExternalSelection, ExternalValue,
    InputDocument, InputDocumentEntry, InputManifestDocument, InputManifestEntry, SelectorPath,
    SelectorSegment,
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
    File { commitment: String, bytes: Vec<u8> },
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

fn deserialize_external_value<T: DeserializeOwned>(
    name: &str,
    selector: SelectorPath,
    resolved: ResolvedExternalInput,
) -> Result<ExternalValue<T>> {
    let commitment = resolved.commitment().to_string();
    let bytes = resolved.bytes().to_vec();
    let value = match resolved {
        ResolvedExternalInput::File { bytes, .. } => {
            raster_core::postcard::from_bytes(&bytes).map_err(|e| {
                Error::Serialization(format!(
                    "Failed to deserialize external input '{}' from postcard bytes: {}",
                    name, e
                ))
            })?
        }
        ResolvedExternalInput::InlineJson { value, .. } => {
            serde_json::from_value(value).map_err(|e| {
                Error::Serialization(format!(
                    "Failed to deserialize inline external input '{}' from JSON value: {}",
                    name, e
                ))
            })?
        }
    };

    Ok(ExternalValue::new(
        name,
        selector,
        Some(commitment),
        bytes,
        value,
    ))
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
    expected_name: Option<&str>,
) -> Result<ExternalValue<T>> {
    if let Some(expected_name) = expected_name {
        if reference.name() != expected_name {
            return Err(Error::Other(format!(
                "External input mismatch: tile expected '{}', but call site provided '{}'",
                expected_name,
                reference.name()
            )));
        }
    }

    let sources = load_external_input_sources()?.ok_or_else(|| {
        Error::Other(
            "External input resolution requires CLI input context from --input and --input-manifest"
                .into(),
        )
    })?;

    let resolved = resolve_cached_external(reference.name(), &sources)?;
    if reference.selector().is_empty() {
        return deserialize_external_value(reference.name(), reference.selector().clone(), resolved);
    }

    match sources.get_input_entry(reference.name()) {
        Some(InputDocumentEntry::Inline(value)) => {
            let selected_value = select_json_value(value, reference.selector())?;
            let selected: T = serde_json::from_value(selected_value).map_err(|e| {
                Error::Serialization(format!(
                    "Failed to deserialize selected external input '{}' from JSON value: {}",
                    reference.name(),
                    e
                ))
            })?;
            Ok(ExternalValue::new(
                reference.name(),
                reference.selector().clone(),
                Some(resolved.commitment().to_string()),
                resolved.bytes().to_vec(),
                selected,
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

#[cfg(test)]
mod tests {
    use super::*;
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
        let value =
            deserialize_external_value::<u64>("seed", SelectorPath::default(), resolved.clone())
                .unwrap();

        assert_eq!(resolved.bytes(), inline_bytes.as_slice());
        assert_eq!(resolved.commitment(), hash);
        assert_eq!(value.value, 123);
        assert_eq!(value.bytes, inline_bytes);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_program_input_value_returns_none_without_cli_context() {
        let parsed = parse_program_input_value::<serde_json::Value>(Some("missing"));

        assert!(parsed.is_none());
    }

    #[test]
    fn resolve_external_value_errors_without_cli_context() {
        let err =
            resolve_external_value::<Flight>(ExternalSelection::new("flight_data"), Some("flight_data"))
                .expect_err("missing CLI context should fail");

        assert_eq!(
            err.to_string(),
            "External input resolution requires CLI input context from --input and --input-manifest"
        );
    }
}
