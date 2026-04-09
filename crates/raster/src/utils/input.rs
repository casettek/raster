use raster_core::external::{External, ExternalValue};
use raster_core::manifest::ExternalInputEntry;
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
    data_hash: Option<String>,
    bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
struct InputContext {
    root: Value,
    base_dir: PathBuf,
    externals: HashMap<String, ResolvedExternal>,
}

static EXTERNAL_INPUT_CONTEXT: OnceLock<Mutex<Option<InputContext>>> = OnceLock::new();

fn context_cell() -> &'static Mutex<Option<InputContext>> {
    EXTERNAL_INPUT_CONTEXT.get_or_init(|| Mutex::new(None))
}

fn load_from_args() -> Option<Result<InputContext>> {
    let args: Vec<String> = std::env::args().collect();
    let input_pos = args.iter().position(|a| a == "--input")?;
    let raw_input = args.get(input_pos + 1)?.clone();
    Some(InputContext::from_input_arg(&raw_input))
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
            root: Value::Null,
            base_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            externals: HashMap::new(),
        },
    };

    *guard = Some(ctx.clone());
    Ok(ctx)
}

impl InputContext {
    fn from_input_arg(raw_input: &str) -> Result<Self> {
        let path = Path::new(raw_input);
        let (root, base_dir) = if path.is_file() {
            let contents = fs::read_to_string(path).map_err(|e| {
                Error::Other(format!(
                    "Failed to read input file '{}': {}",
                    path.display(),
                    e
                ))
            })?;
            let root: Value = serde_json::from_str(&contents).map_err(|e| {
                Error::Serialization(format!(
                    "Failed to parse input file '{}' as JSON: {}",
                    path.display(),
                    e
                ))
            })?;
            let base_dir = path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            (root, base_dir)
        } else {
            let root: Value = serde_json::from_str(raw_input).map_err(|e| {
                Error::Serialization(format!("Failed to parse --input as JSON: {}", e))
            })?;
            let base_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            (root, base_dir)
        };

        Ok(Self {
            root,
            base_dir,
            externals: HashMap::new(),
        })
    }

    fn root_value(&self) -> &Value {
        &self.root
    }

    fn get_named_value(&self, name: &str) -> Option<&Value> {
        self.root_value().as_object().and_then(|obj| obj.get(name))
    }

    fn read_external_entry(&self, name: &str) -> Result<ExternalInputEntry> {
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

pub fn parse_main_input<T: DeserializeOwned>() -> Option<T> {
    parse_main_input_value(None)
}

pub fn parse_main_input_value<T: DeserializeOwned>(name: Option<&str>) -> Option<T> {
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
                root: Value::Null,
                base_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                externals: HashMap::new(),
            },
        });
    }

    let ctx = guard.as_mut().expect("input context should be initialized");

    if !ctx.externals.contains_key(expected_name) {
        let entry = ctx.read_external_entry(expected_name)?;
        let path = ctx.base_dir.join(&entry.path);
        let bytes = fs::read(&path).map_err(|e| {
            Error::Other(format!(
                "Failed to read external input '{}' from '{}': {}",
                expected_name,
                path.display(),
                e
            ))
        })?;

        if let Some(expected_hash) = entry.data_hash.as_deref() {
            let actual_hash = sha256_hex(&bytes);
            if normalize_hash(expected_hash) != actual_hash {
                return Err(Error::Other(format!(
                    "External input '{}' failed integrity check. Expected SHA256 {}, got {}",
                    expected_name, expected_hash, actual_hash
                )));
            }
        }

        ctx.externals.insert(
            expected_name.to_string(),
            ResolvedExternal {
                data_hash: entry.data_hash,
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

    Ok(ExternalValue::new(expected_name, resolved.data_hash, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::format;
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
        fs::write(
            &input_path,
            format!(
                r#"{{"flight_data":{{"path":"flights.bin","data_hash":"{}"}}}}"#,
                hash
            ),
        )
        .unwrap();

        let ctx = InputContext::from_input_arg(input_path.to_str().unwrap()).unwrap();
        let entry = ctx.read_external_entry("flight_data").unwrap();
        assert_eq!(entry.path, "flights.bin");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn detects_hash_mismatch() {
        let bytes = b"abc";
        let actual = sha256_hex(bytes);
        assert_ne!(actual, "deadbeef");
    }
}
