use raster_core::input::{
    External, ExternalInputManifestEntry, ExternalInputPathEntry, ExternalValue, InputDocument,
    InputDocumentEntry, InputManifestDocument, InputManifestEntry,
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
struct ResolvedExternal {
    commitment: String,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
struct ExternalInputSources {
    input_document: InputDocument,
    manifest_document: InputManifestDocument,
    input_base_dir: PathBuf,
}

static RESOLVED_EXTERNALS: OnceLock<Mutex<HashMap<String, ResolvedExternal>>> = OnceLock::new();

fn external_cache() -> &'static Mutex<HashMap<String, ResolvedExternal>> {
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

        entry.as_external_path().map(str::to_owned).ok_or_else(|| {
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
            .as_external_commitment()
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::Serialization(format!(
                    "Expected public manifest entry '{}' to use {{\"external_commitment\": \"...\"}}",
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

fn resolve_cached_external(name: &str, sources: &ExternalInputSources) -> Result<ResolvedExternal> {
    if let Some(resolved) = external_cache().lock().unwrap().get(name).cloned() {
        return Ok(resolved);
    }

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

    let expected_commitment = sources.read_external_commitment_entry(name)?;
    let actual_hash = sha256_hex(&bytes);
    if normalize_hash(&expected_commitment) != actual_hash {
        return Err(Error::Other(format!(
            "External input '{}' failed integrity check. Expected SHA256 {}, got {}",
            name, expected_commitment, actual_hash
        )));
    }

    let resolved = ResolvedExternal {
        commitment: expected_commitment,
        bytes,
    };

    let mut guard = external_cache().lock().unwrap();
    Ok(guard
        .entry(name.to_string())
        .or_insert_with(|| resolved.clone())
        .clone())
}

fn deserialize_external_value<T: DeserializeOwned>(
    name: &str,
    resolved: ResolvedExternal,
) -> Result<ExternalValue<T>> {
    let value = raster_core::postcard::from_bytes(&resolved.bytes).map_err(|e| {
        Error::Serialization(format!(
            "Failed to deserialize external input '{}' from postcard bytes: {}",
            name, e
        ))
    })?;

    Ok(ExternalValue::new(
        name,
        Some(resolved.commitment),
        resolved.bytes,
        value,
    ))
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

/// Parse the private program input from `--input` and deserialize the full value.
pub fn parse_program_input<T: DeserializeOwned + core::fmt::Debug>() -> Option<T> {
    parse_program_input_value(None)
}

/// Parse either a named top-level field from the private `input.json` document,
/// or the full document when `name` is `None`.
pub fn parse_program_input_value<T: DeserializeOwned + core::fmt::Debug>(
    name: Option<&str>,
) -> Option<T> {
    let sources = load_external_input_sources().ok()??;

    if let Some(name) = name {
        if let Some(value) = sources.project_input_value(name) {
            return serde_json::from_value(value).ok();
        }
    }

    let value = serde_json::from_value(sources.project_input_document()).ok();
    println!("[debug] external value: {:?}", value);

    value
}

pub fn resolve_external_value<T: DeserializeOwned + Serialize>(
    reference: External<T>,
    expected_name: &str,
) -> Result<ExternalValue<T>> {
    if reference.name() != expected_name {
        return Err(Error::Other(format!(
            "External input mismatch: tile expected '{}', but call site provided '{}'",
            expected_name,
            reference.name()
        )));
    }

    let sources = load_external_input_sources()?.ok_or_else(|| {
        Error::Other(
            "External input resolution requires CLI input context from --input and --input-manifest"
                .into(),
        )
    })?;

    let resolved = resolve_cached_external(expected_name, &sources)?;
    deserialize_external_value(expected_name, resolved)
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
            r#"{"flight_data":{"path":"flights.bin"}}"#,
            r#"{"flight_data":{"external_commitment":"{hash}"}}"#,
        );

        let sources =
            ExternalInputSources::from_input_args(input_path.to_str(), manifest_path.to_str())
                .unwrap();
        let entry = sources.read_external_path_entry("flight_data").unwrap();
        assert_eq!(entry, "flights.bin");
        assert_eq!(
            sources
                .read_external_commitment_entry("flight_data")
                .unwrap(),
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
            r#"{"flight_data":{"path":"flights.bin"}}"#,
            r#"{"flight_data":{"external_commitment":"{hash}"}}"#,
        );

        let sources =
            ExternalInputSources::from_input_args(input_path.to_str(), manifest_path.to_str())
                .unwrap();
        let first = resolve_cached_external("flight_data", &sources).unwrap();

        let changed_bytes =
            raster_core::postcard::to_allocvec(&vec![Flight { id: 9, seats: 42 }]).unwrap();
        fs::write(&data_path, &changed_bytes).unwrap();

        let second = resolve_cached_external("flight_data", &sources).unwrap();

        assert_eq!(first.bytes, bytes);
        assert_eq!(second.bytes, bytes);
        assert_eq!(first.commitment, hash);
        assert_eq!(second.commitment, hash);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_external_inputs_with_wrong_manifest_commitment() {
        clear_external_cache();
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let data_path = dir.join("flights.bin");
        let flights = vec![Flight { id: 7, seats: 180 }];
        let bytes = raster_core::postcard::to_allocvec(&flights).unwrap();
        fs::write(&data_path, &bytes).unwrap();

        let (input_path, manifest_path) = write_external_documents(
            &dir,
            "deadbeef",
            r#"{"flight_data":{"path":"flights.bin"}}"#,
            r#"{"flight_data":{"external_commitment":"{hash}"}}"#,
        );

        let sources =
            ExternalInputSources::from_input_args(input_path.to_str(), manifest_path.to_str())
                .unwrap();
        let err = resolve_cached_external("flight_data", &sources).expect_err("hash mismatch");

        assert!(err
            .to_string()
            .contains("External input 'flight_data' failed integrity check"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_program_input_value_returns_none_without_cli_context() {
        let parsed = parse_program_input_value::<serde_json::Value>(Some("missing"));

        assert!(parsed.is_none());
    }

    #[test]
    fn resolve_external_value_errors_without_cli_context() {
        let err = resolve_external_value::<Flight>(External::new("flight_data"), "flight_data")
            .expect_err("missing CLI context should fail");

        assert_eq!(
            err.to_string(),
            "External input resolution requires CLI input context from --input and --input-manifest"
        );
    }
}
