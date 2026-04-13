use raster_core::external::{External, ExternalValue};
use raster_core::manifest::{ExternalInputManifestEntry, ExternalInputPathEntry};
use raster_core::{Error, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
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
struct InputContext {
    private_root: Value,
    manifest_root: Value,
    private_base_dir: PathBuf,
    externals: HashMap<String, ResolvedExternal>,
}

static EXTERNAL_INPUT_CONTEXT: OnceLock<Mutex<Option<InputContext>>> = OnceLock::new();

fn context_cell() -> &'static Mutex<Option<InputContext>> {
    EXTERNAL_INPUT_CONTEXT.get_or_init(|| Mutex::new(None))
}

fn load_from_args() -> Option<Result<InputContext>> {
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
    Some(InputContext::from_input_args(
        raw_input.as_deref(),
        raw_manifest.as_deref(),
    ))
}

fn ensure_context() -> Result<InputContext> {
    let cell = context_cell();
    let mut guard = cell.lock().unwrap();

    if let Some(ctx) = guard.as_ref() {
        return Ok(ctx.clone());
    }

    let ctx = match load_from_args() {
        Some(result) => result?,
        None => InputContext {
            private_root: Value::Null,
            manifest_root: Value::Null,
            private_base_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            externals: HashMap::new(),
        },
    };

    *guard = Some(ctx.clone());
    Ok(ctx)
}

impl InputContext {
    fn parse_json_source(raw_input: Option<&str>, label: &str) -> Result<(Value, PathBuf)> {
        let Some(raw_input) = raw_input else {
            return Ok((
                Value::Null,
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
        let (private_root, private_base_dir) = Self::parse_json_source(raw_input, "input")?;
        let (manifest_root, _manifest_base_dir) =
            Self::parse_json_source(raw_manifest, "input manifest")?;

        Ok(Self {
            private_root,
            manifest_root,
            private_base_dir,
            externals: HashMap::new(),
        })
    }

    fn root_value(&self) -> &Value {
        &self.private_root
    }

    fn get_named_value(&self, name: &str) -> Option<&Value> {
        self.root_value().as_object().and_then(|obj| obj.get(name))
    }

    fn get_manifest_value(&self, name: &str) -> Option<&Value> {
        self.manifest_root.as_object().and_then(|obj| obj.get(name))
    }

    fn read_external_entry(&self, name: &str) -> Result<ExternalInputPathEntry> {
        let raw = self.get_named_value(name).ok_or_else(|| {
            Error::Other(format!(
                "Missing external input '{}'. Expected a top-level input document field.",
                name
            ))
        })?;

        serde_json::from_value(raw.clone()).map_err(|e| {
            Error::Serialization(format!(
                "Failed to parse external input '{}' descriptor: {}",
                name, e
            ))
        })
    }

    fn read_manifest_entry(&self, name: &str) -> Result<ExternalInputManifestEntry> {
        let raw = self.get_manifest_value(name).ok_or_else(|| {
            Error::Other(format!(
                "Missing public manifest entry for external input '{}'. Expected a top-level field in input_manifest.json.",
                name
            ))
        })?;

        serde_json::from_value(raw.clone()).map_err(|e| {
            Error::Serialization(format!(
                "Failed to parse public manifest entry '{}' as a commitment string: {}",
                name, e
            ))
        })
    }
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
pub fn parse_program_input<T: DeserializeOwned>() -> Option<T> {
    parse_program_input_value(None)
}

/// Parse either a named top-level field from the private `input.json` document,
/// or the full document when `name` is `None`.
pub fn parse_program_input_value<T: DeserializeOwned>(name: Option<&str>) -> Option<T> {
    let ctx = ensure_context().ok()?;

    if let Some(name) = name {
        if let Some(value) = ctx.get_named_value(name) {
            return serde_json::from_value(value.clone()).ok();
        }
    }

    serde_json::from_value(ctx.root_value().clone()).ok()
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

    let cell = context_cell();
    let mut guard = cell.lock().unwrap();
    if guard.is_none() {
        *guard = Some(match load_from_args() {
            Some(result) => result?,
            None => InputContext {
                private_root: Value::Null,
                manifest_root: Value::Null,
                private_base_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                externals: HashMap::new(),
            },
        });
    }

    let ctx = guard.as_mut().expect("input context should be initialized");

    if !ctx.externals.contains_key(expected_name) {
        let entry = ctx.read_external_entry(expected_name)?;
        let path = ctx.private_base_dir.join(&entry);
        let bytes = fs::read(&path).map_err(|e| {
            Error::Other(format!(
                "Failed to read external input '{}' from '{}': {}",
                expected_name,
                path.display(),
                e
            ))
        })?;

        let expected_commitment = ctx.read_manifest_entry(expected_name)?;
        let actual_hash = sha256_hex(&bytes);
        if normalize_hash(&expected_commitment) != actual_hash {
            return Err(Error::Other(format!(
                "External input '{}' failed integrity check. Expected SHA256 {}, got {}",
                expected_name, expected_commitment, actual_hash
            )));
        }

        ctx.externals.insert(
            expected_name.to_string(),
            ResolvedExternal {
                commitment: expected_commitment,
                bytes,
            },
        );
    }

    let resolved = ctx
        .externals
        .get(expected_name)
        .expect("resolved external should be cached")
        .clone();

    let value = raster_core::postcard::from_bytes(&resolved.bytes).map_err(|e| {
        Error::Serialization(format!(
            "Failed to deserialize external input '{}' from postcard bytes: {}",
            expected_name, e
        ))
    })?;

    Ok(ExternalValue::new(
        expected_name,
        Some(resolved.commitment),
        resolved.bytes,
        value,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::vec;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Flight {
        id: u32,
        seats: u16,
    }

    fn unique_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("raster-input-test-{}", nanos))
    }

    #[test]
    fn reads_file_backed_input_and_resolves_relative_external() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let data_path = dir.join("flights.bin");
        let flights = vec![Flight { id: 7, seats: 180 }];
        let bytes = raster_core::postcard::to_allocvec(&flights).unwrap();
        fs::write(&data_path, &bytes).unwrap();
        let hash = sha256_hex(&bytes);

        let input_path = dir.join("input.json");
        fs::write(&input_path, r#"{"flight_data":"flights.bin"}"#).unwrap();

        let manifest_path = dir.join("input_manifest.json");
        fs::write(&manifest_path, format!(r#"{{"flight_data":"{}"}}"#, hash.clone())).unwrap();

        let ctx =
            InputContext::from_input_args(input_path.to_str(), manifest_path.to_str()).unwrap();
        let entry = ctx.read_external_entry("flight_data").unwrap();
        assert_eq!(entry, "flights.bin");
        assert_eq!(ctx.read_manifest_entry("flight_data").unwrap(), hash);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detects_hash_mismatch() {
        let bytes = b"abc";
        let actual = sha256_hex(bytes);
        assert_ne!(actual, "deadbeef");
    }
}
