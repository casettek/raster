use memmap2::Mmap;
use raster_core::input::{
    ExternalEncoding, ExternalInputManifestEntry, ExternalLoadPreference, InputDocument,
    InputDocumentEntry, InputManifestDocument, InputManifestEntry,
};
use raster_core::{Error, Result};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::format;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::string::String;
use std::sync::{Arc, Mutex, OnceLock};
use std::vec::Vec;

use crate::raster_index::RasterIndex;

#[derive(Debug, Clone)]
pub(crate) enum ExternalFile {
    Read(Arc<[u8]>),
    Mmap(Arc<Mmap>),
}

impl ExternalFile {
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Read(bytes) => bytes,
            Self::Mmap(map) => map,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SourceKey {
    encoding: ExternalEncoding,
    path: PathBuf,
    index_path: Option<PathBuf>,
    commitment: String,
    load_preference: ExternalLoadPreference,
}

#[derive(Debug, Clone)]
struct ExternalFileRegistry {
    input_document: InputDocument,
    manifest_document: InputManifestDocument,
    base_dir: PathBuf,
}

impl ExternalFileRegistry {
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
        let (input_document, base_dir) = Self::parse_json_source(raw_input, "input")?;
        let (manifest_document, _manifest_base_dir) =
            Self::parse_json_source(raw_manifest, "input manifest")?;

        Ok(Self {
            input_document,
            manifest_document,
            base_dir,
        })
    }

    fn get_input_entry(&self, name: &str) -> Option<&InputDocumentEntry> {
        self.input_document.get(name)
    }

    fn get_manifest_entry(&self, name: &str) -> Option<&InputManifestEntry> {
        self.manifest_document.get(name)
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ResolvedExternalData {
    Postcard {
        commitment: String,
        file: ExternalFile,
    },
    Raster {
        commitment: String,
        data_file: ExternalFile,
        _index_file: ExternalFile,
        index: Arc<RasterIndex>,
    },
}

impl ResolvedExternalData {
    pub(crate) fn commitment(&self) -> &str {
        match self {
            Self::Postcard { commitment, .. } | Self::Raster { commitment, .. } => commitment,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Postcard { file, .. } => file.bytes(),
            Self::Raster { data_file, .. } => data_file.bytes(),
        }
    }

    pub(crate) fn deserialize<T: DeserializeOwned>(&self) -> Result<T> {
        match self {
            Self::Postcard { file, .. } => {
                raster_core::postcard::from_bytes(file.bytes()).map_err(|e| {
                    Error::Serialization(format!(
                        "Failed to deserialize external data from postcard bytes: {}",
                        e
                    ))
                })
            }
            Self::Raster { .. } => Err(Error::Other(
                "Raster-encoded external data must be resolved through selection-tree payloads"
                    .into(),
            )),
        }
    }

    pub(crate) fn raster_index(&self) -> Option<&RasterIndex> {
        match self {
            Self::Raster { index, .. } => Some(index.as_ref()),
            Self::Postcard { .. } => None,
        }
    }

    pub(crate) fn raster_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Raster { data_file, .. } => Some(data_file.bytes()),
            Self::Postcard { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ExternalStorageManager {
    registry: ExternalFileRegistry,
    cache: Arc<Mutex<HashMap<SourceKey, ResolvedExternalData>>>,
}

static CLI_EXTERNAL_STORAGE: OnceLock<Option<ExternalStorageManager>> = OnceLock::new();

impl ExternalStorageManager {
    pub(crate) fn from_input_args(
        raw_input: Option<&str>,
        raw_manifest: Option<&str>,
    ) -> Result<Self> {
        Ok(Self {
            registry: ExternalFileRegistry::from_input_args(raw_input, raw_manifest)?,
            cache: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub(crate) fn from_cli_args() -> Result<Option<Self>> {
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
            return Ok(None);
        }

        Ok(Some(Self::from_input_args(
            raw_input.as_deref(),
            raw_manifest.as_deref(),
        )?))
    }

    pub(crate) fn cached_from_cli_args() -> Result<Option<Self>> {
        if let Some(storage) = CLI_EXTERNAL_STORAGE.get() {
            return Ok(storage.clone());
        }

        let storage = Self::from_cli_args()?;
        let _ = CLI_EXTERNAL_STORAGE.set(storage);
        Ok(CLI_EXTERNAL_STORAGE.get().cloned().unwrap_or(None))
    }

    pub(crate) fn resolve(&self, name: &str) -> Result<ResolvedExternalData> {
        let input_entry = self.read_input_entry(name)?;
        let manifest_entry = self.read_manifest_entry(name)?;
        let expected_commitment = manifest_entry
            .as_sha256_commitment()
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::Serialization(format!(
                    "Expected public manifest entry '{}' to use {{\"type\": \"sha256\", \"commitment\": \"...\"}}",
                    name
                ))
            })?;
        let path = self.registry.base_dir.join(input_entry.path());

        match manifest_entry.encoding() {
            ExternalEncoding::Postcard => self.resolve_postcard_file(
                name,
                &path,
                expected_commitment,
                input_entry.load_preference(),
            ),
            ExternalEncoding::Raster => {
                let index_path = input_entry.index_path().ok_or_else(|| {
                    Error::Other(format!(
                        "External input '{}' uses raster encoding but is missing `index_path` in input.json",
                        name
                    ))
                })?;
                self.resolve_raster_files(
                    name,
                    &path,
                    &self.registry.base_dir.join(index_path),
                    expected_commitment,
                    input_entry.load_preference(),
                )
            }
        }
    }

    pub(crate) fn is_raster_encoded(&self, name: &str) -> Result<bool> {
        Ok(matches!(
            self.read_manifest_entry(name)?.encoding(),
            ExternalEncoding::Raster
        ))
    }

    fn read_input_entry(&self, name: &str) -> Result<&InputDocumentEntry> {
        self.registry.get_input_entry(name).ok_or_else(|| {
            Error::Other(format!(
                "Missing external input '{}'. Expected a top-level input document field.",
                name
            ))
        })
    }

    fn read_manifest_entry(&self, name: &str) -> Result<&InputManifestEntry> {
        self.registry.get_manifest_entry(name).ok_or_else(|| {
            Error::Other(format!(
                "Missing public manifest entry for external input '{}'. Expected a top-level field in input_manifest.json.",
                name
            ))
        })
    }

    fn resolve_postcard_file(
        &self,
        name: &str,
        path: &Path,
        expected_commitment: String,
        load_preference: ExternalLoadPreference,
    ) -> Result<ResolvedExternalData> {
        let canonical_path = fs::canonicalize(path).map_err(|e| {
            Error::Other(format!(
                "Failed to resolve external input '{}' path '{}': {}",
                name,
                path.display(),
                e
            ))
        })?;
        let key = SourceKey {
            encoding: ExternalEncoding::Postcard,
            path: canonical_path.clone(),
            index_path: None,
            commitment: normalize_hash(&expected_commitment),
            load_preference,
        };

        if let Some(resolved) = self.cache.lock().unwrap().get(&key).cloned() {
            return Ok(resolved);
        }

        let storage = match load_preference {
            ExternalLoadPreference::Read => read_file(name, &canonical_path)?,
            ExternalLoadPreference::Mmap => {
                mmap_file(name, &canonical_path).or_else(|_| read_file(name, &canonical_path))?
            }
        };
        verify_input_commitment(name, storage.bytes(), &expected_commitment)?;
        let resolved = ResolvedExternalData::Postcard {
            commitment: expected_commitment,
            file: storage,
        };

        let mut guard = self.cache.lock().unwrap();
        Ok(guard.entry(key).or_insert_with(|| resolved.clone()).clone())
    }

    fn resolve_raster_files(
        &self,
        name: &str,
        data_path: &Path,
        index_path: &Path,
        expected_commitment: ExternalInputManifestEntry,
        load_preference: ExternalLoadPreference,
    ) -> Result<ResolvedExternalData> {
        let canonical_data_path = fs::canonicalize(data_path).map_err(|e| {
            Error::Other(format!(
                "Failed to resolve raster input '{}' path '{}': {}",
                name,
                data_path.display(),
                e
            ))
        })?;
        let canonical_index_path = fs::canonicalize(index_path).map_err(|e| {
            Error::Other(format!(
                "Failed to resolve raster index '{}' path '{}': {}",
                name,
                index_path.display(),
                e
            ))
        })?;
        let key = SourceKey {
            encoding: ExternalEncoding::Raster,
            path: canonical_data_path.clone(),
            index_path: Some(canonical_index_path.clone()),
            commitment: normalize_hash(&expected_commitment),
            load_preference,
        };

        if let Some(resolved) = self.cache.lock().unwrap().get(&key).cloned() {
            return Ok(resolved);
        }

        let data_file = match load_preference {
            ExternalLoadPreference::Read => read_file(name, &canonical_data_path)?,
            ExternalLoadPreference::Mmap => mmap_file(name, &canonical_data_path)
                .or_else(|_| read_file(name, &canonical_data_path))?,
        };
        let index_file = match load_preference {
            ExternalLoadPreference::Read => read_file(name, &canonical_index_path)?,
            ExternalLoadPreference::Mmap => mmap_file(name, &canonical_index_path)
                .or_else(|_| read_file(name, &canonical_index_path))?,
        };
        let index = Arc::new(RasterIndex::from_bytes(index_file.bytes())?);
        let actual_commitment = index.root_commitment_hex();
        if normalize_hash(&expected_commitment) != normalize_hash(&actual_commitment) {
            return Err(Error::Other(format!(
                "Raster input '{}' failed integrity check. Expected root commitment {}, got {}",
                name, expected_commitment, actual_commitment
            )));
        }

        let resolved = ResolvedExternalData::Raster {
            commitment: expected_commitment,
            data_file,
            _index_file: index_file,
            index,
        };

        let mut guard = self.cache.lock().unwrap();
        Ok(guard.entry(key).or_insert_with(|| resolved.clone()).clone())
    }
}

fn read_file(name: &str, path: &Path) -> Result<ExternalFile> {
    let bytes = fs::read(path).map_err(|e| {
        Error::Other(format!(
            "Failed to read external input '{}' from '{}': {}",
            name,
            path.display(),
            e
        ))
    })?;
    Ok(ExternalFile::Read(Arc::<[u8]>::from(bytes)))
}

fn mmap_file(name: &str, path: &Path) -> Result<ExternalFile> {
    let file = File::open(path).map_err(|e| {
        Error::Other(format!(
            "Failed to open external input '{}' from '{}': {}",
            name,
            path.display(),
            e
        ))
    })?;
    // The mapping is read-only and remains owned by the resolved external input.
    let map = unsafe {
        Mmap::map(&file).map_err(|e| {
            Error::Other(format!(
                "Failed to memory-map external input '{}' from '{}': {}",
                name,
                path.display(),
                e
            ))
        })?
    };
    Ok(ExternalFile::Mmap(Arc::new(map)))
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

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster_index::{RasterIndex, RasterNode, RasterNodeKind};
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    static UNIQUE_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let counter = UNIQUE_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "raster-external-storage-test-{}-{}",
            nanos, counter
        ))
    }

    fn write_external_documents(
        dir: &Path,
        input_body: &str,
        manifest_body: &str,
    ) -> (PathBuf, PathBuf) {
        let input_path = dir.join("input.json");
        fs::write(&input_path, input_body).unwrap();

        let manifest_path = dir.join("input_manifest.json");
        fs::write(&manifest_path, manifest_body).unwrap();

        (input_path, manifest_path)
    }

    fn selection_hash(parts: &[&[u8]]) -> Vec<u8> {
        let mut hasher = Sha256::new();
        for part in parts {
            hasher.update(part);
        }
        hasher.finalize().to_vec()
    }

    fn hex_string(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push_str(&format!("{:02x}", byte));
        }
        out
    }

    fn leaf_payload_u64(value: u64) -> Vec<u8> {
        let mut payload = vec![0x00];
        payload.extend_from_slice(&(8u64).to_le_bytes());
        payload.extend_from_slice(&value.to_le_bytes());
        payload
    }

    #[test]
    fn resolves_relative_external_paths_with_per_entry_backings() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("payload.bin");
        let bytes = raster_core::postcard::to_allocvec(&123u64).unwrap();
        fs::write(&path, &bytes).unwrap();
        let hash = sha256_hex(&bytes);
        let (input_path, manifest_path) = write_external_documents(
            &dir,
            r#"{
                "payload_read": { "path": "payload.bin", "load_preference": "read" },
                "payload_mmap": { "path": "payload.bin", "load_preference": "mmap" }
            }"#,
            &format!(
                concat!(
                    "{{",
                    "\"payload_read\":{{\"type\":\"sha256\",\"commitment\":\"{}\"}},",
                    "\"payload_mmap\":{{\"type\":\"sha256\",\"commitment\":\"{}\"}}",
                    "}}"
                ),
                hash, hash
            ),
        );
        let manager =
            ExternalStorageManager::from_input_args(input_path.to_str(), manifest_path.to_str())
                .unwrap();

        let read = manager.resolve("payload_read").unwrap();
        let mapped = manager.resolve("payload_mmap").unwrap();

        assert_eq!(read.bytes(), bytes.as_slice());
        assert_eq!(mapped.bytes(), bytes.as_slice());
        assert!(matches!(
            read,
            ResolvedExternalData::Postcard {
                file: ExternalFile::Read(_),
                ..
            }
        ));
        assert!(matches!(
            mapped,
            ResolvedExternalData::Postcard {
                file: ExternalFile::Mmap(_),
                ..
            }
        ));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn caches_resolved_external_bytes_by_source_identity() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let data_path = dir.join("flights.bin");
        let initial_bytes = raster_core::postcard::to_allocvec(&vec![7u64, 180u64]).unwrap();
        fs::write(&data_path, &initial_bytes).unwrap();
        let hash = sha256_hex(&initial_bytes);
        let (input_path, manifest_path) = write_external_documents(
            &dir,
            r#"{"flight_data_cached":{"path":"flights.bin","load_preference":"read"}}"#,
            &format!(
                r#"{{"flight_data_cached":{{"type":"sha256","commitment":"{}"}}}}"#,
                hash
            ),
        );
        let manager =
            ExternalStorageManager::from_input_args(input_path.to_str(), manifest_path.to_str())
                .unwrap();

        let first = manager.resolve("flight_data_cached").unwrap();

        let changed_bytes = raster_core::postcard::to_allocvec(&vec![9u64, 42u64]).unwrap();
        fs::write(&data_path, &changed_bytes).unwrap();

        let second = manager.resolve("flight_data_cached").unwrap();

        assert_eq!(first.bytes(), initial_bytes.as_slice());
        assert_eq!(second.bytes(), initial_bytes.as_slice());
        assert_eq!(first.commitment(), hash);
        assert_eq!(second.commitment(), hash);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_external_inputs_with_wrong_manifest_commitment() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let data_path = dir.join("flights.bin");
        let bytes = raster_core::postcard::to_allocvec(&vec![7u64, 180u64]).unwrap();
        fs::write(&data_path, &bytes).unwrap();
        let (input_path, manifest_path) = write_external_documents(
            &dir,
            r#"{"flight_data_bad_manifest":{"path":"flights.bin","load_preference":"mmap"}}"#,
            r#"{"flight_data_bad_manifest":{"type":"sha256","commitment":"deadbeef"}}"#,
        );
        let manager =
            ExternalStorageManager::from_input_args(input_path.to_str(), manifest_path.to_str())
                .unwrap();

        let err = manager
            .resolve("flight_data_bad_manifest")
            .expect_err("hash mismatch");

        assert!(err
            .to_string()
            .contains("External input 'flight_data_bad_manifest' failed integrity check"));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolves_raster_external_inputs_against_index_root_commitment() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let payload = leaf_payload_u64(123);
        let root_hash = selection_hash(&[b"leaf", &123u64.to_le_bytes()]);
        let index = RasterIndex::new(
            0,
            root_hash.clone(),
            vec![RasterNode {
                offset: 0,
                len: payload.len() as u64,
                root_hash: root_hash.clone(),
                kind: RasterNodeKind::Leaf {
                    type_name: "u64".into(),
                },
            }],
        );
        let data_path = dir.join("payload.rastered");
        let index_path = dir.join("payload.rindex");
        fs::write(&data_path, &payload).unwrap();
        fs::write(&index_path, index.encode().unwrap()).unwrap();

        let (input_path, manifest_path) = write_external_documents(
            &dir,
            r#"{"payload":{"path":"payload.rastered","index_path":"payload.rindex","load_preference":"mmap"}}"#,
            &format!(
                r#"{{"payload":{{"type":"sha256","encoding":"raster","commitment":"{}"}}}}"#,
                hex_string(&root_hash)
            ),
        );
        let manager =
            ExternalStorageManager::from_input_args(input_path.to_str(), manifest_path.to_str())
                .unwrap();

        let resolved = manager.resolve("payload").unwrap();

        assert_eq!(resolved.bytes(), payload.as_slice());
        assert_eq!(resolved.commitment(), hex_string(&root_hash));
        assert!(matches!(resolved, ResolvedExternalData::Raster { .. }));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cloned_managers_share_cached_raster_indexes() {
        let dir = unique_dir();
        fs::create_dir_all(&dir).unwrap();

        let payload = leaf_payload_u64(123);
        let root_hash = selection_hash(&[b"leaf", &123u64.to_le_bytes()]);
        let index = RasterIndex::new(
            0,
            root_hash.clone(),
            vec![RasterNode {
                offset: 0,
                len: payload.len() as u64,
                root_hash: root_hash.clone(),
                kind: RasterNodeKind::Leaf {
                    type_name: "u64".into(),
                },
            }],
        );
        let data_path = dir.join("payload.rastered");
        let index_path = dir.join("payload.rindex");
        fs::write(&data_path, &payload).unwrap();
        fs::write(&index_path, index.encode().unwrap()).unwrap();

        let (input_path, manifest_path) = write_external_documents(
            &dir,
            r#"{"payload_cached":{"path":"payload.rastered","index_path":"payload.rindex","load_preference":"mmap"}}"#,
            &format!(
                r#"{{"payload_cached":{{"type":"sha256","encoding":"raster","commitment":"{}"}}}}"#,
                hex_string(&root_hash)
            ),
        );
        let manager =
            ExternalStorageManager::from_input_args(input_path.to_str(), manifest_path.to_str())
                .unwrap();

        let first = manager.resolve("payload_cached").unwrap();
        fs::write(&index_path, b"not a raster index").unwrap();
        let second = manager.clone().resolve("payload_cached").unwrap();

        match (&first, &second) {
            (
                ResolvedExternalData::Raster {
                    index: first_index, ..
                },
                ResolvedExternalData::Raster {
                    index: second_index,
                    ..
                },
            ) => assert!(Arc::ptr_eq(first_index, second_index)),
            _ => panic!("expected cached raster data"),
        }
        assert_eq!(second.bytes(), payload.as_slice());

        fs::remove_dir_all(&dir).unwrap();
    }
}
