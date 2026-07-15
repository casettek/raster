use raster_core::input::{
    InputDocument, InputDocumentEntry, InputManifestDocument, InputManifestEntry,
};
use raster_core::{Error, Result};
use serde::de::DeserializeOwned;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(super) struct FileInputRegistry {
    input_document: InputDocument,
    manifest_document: InputManifestDocument,
    base_dir: PathBuf,
}

impl FileInputRegistry {
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

    pub(super) fn from_input_args(
        raw_input: Option<&str>,
        raw_manifest: Option<&str>,
    ) -> Result<Self> {
        let (input_document, base_dir) = Self::parse_json_source(raw_input, "input")?;
        let (manifest_document, _manifest_base_dir) =
            Self::parse_json_source(raw_manifest, "input manifest")?;

        Ok(Self {
            input_document,
            manifest_document,
            base_dir,
        })
    }

    pub(super) fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub(super) fn get_input_entry(&self, name: &str) -> Option<&InputDocumentEntry> {
        self.input_document.get(name)
    }

    pub(super) fn get_manifest_entry(&self, name: &str) -> Option<&InputManifestEntry> {
        self.manifest_document.get(name)
    }
}
