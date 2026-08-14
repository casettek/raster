use memmap2::Mmap;
use raster_core::{Error, Result};
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use crate::raster_index::RasterIndex;

#[derive(Debug, Clone)]
pub(crate) enum SourceFile {
    /// Retained handle. The whole file is read only if [`Self::bytes`] is
    /// called; selections should use [`Self::read_range`].
    Read {
        path: PathBuf,
        file: Arc<Mutex<File>>,
        cache: Arc<OnceLock<Arc<[u8]>>>,
    },
    Mmap(Arc<Mmap>),
    /// In-memory fixture bytes (tests and postcard values already in RAM).
    Memory(Arc<[u8]>),
}

impl SourceFile {
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Read { path, file, cache } => cache
                .get_or_init(|| {
                    let mut guard = file.lock().expect("source file lock");
                    use std::io::{Read, Seek, SeekFrom};
                    let _ = guard.seek(SeekFrom::Start(0));
                    let mut bytes = Vec::new();
                    guard.read_to_end(&mut bytes).unwrap_or_else(|e| {
                        panic!("Failed to read '{}': {e}", path.display())
                    });
                    Arc::<[u8]>::from(bytes)
                })
                .as_ref(),
            Self::Mmap(map) => map,
            Self::Memory(bytes) => bytes,
        }
    }

    pub(crate) fn read_range(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let start = offset as usize;
        let take = len as usize;
        match self {
            Self::Mmap(map) => {
                let end = start
                    .checked_add(take)
                    .ok_or_else(|| Error::Other("ranged read overflow".into()))?;
                let slice = map.get(start..end).ok_or_else(|| {
                    Error::Other("ranged read is outside the mapped file".into())
                })?;
                Ok(slice.to_vec())
            }
            Self::Memory(bytes) => {
                let end = start
                    .checked_add(take)
                    .ok_or_else(|| Error::Other("ranged read overflow".into()))?;
                let slice = bytes.get(start..end).ok_or_else(|| {
                    Error::Other("ranged read is outside the in-memory source".into())
                })?;
                Ok(slice.to_vec())
            }
            Self::Read { file, path, .. } => {
                let guard = file.lock().expect("source file lock");
                let mut buf = vec![0u8; take];
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileExt;
                    guard.read_exact_at(&mut buf, offset).map_err(|e| {
                        Error::Other(format!(
                            "Failed to read {} bytes at {} from '{}': {e}",
                            take,
                            offset,
                            path.display()
                        ))
                    })?;
                }
                #[cfg(not(unix))]
                {
                    use std::io::{Read, Seek, SeekFrom};
                    drop(guard);
                    let mut guard = file.lock().expect("source file lock");
                    guard.seek(SeekFrom::Start(offset)).map_err(|e| {
                        Error::Other(format!(
                            "Failed to seek '{}' to {}: {e}",
                            path.display(),
                            offset
                        ))
                    })?;
                    guard.read_exact(&mut buf).map_err(|e| {
                        Error::Other(format!(
                            "Failed to read {} bytes from '{}': {e}",
                            take,
                            path.display()
                        ))
                    })?;
                }
                Ok(buf)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn is_fully_buffered(&self) -> bool {
        match self {
            Self::Read { cache, .. } => cache.get().is_some(),
            Self::Mmap(_) | Self::Memory(_) => true,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ResolvedSourceData {
    Postcard {
        commitment: String,
        file: SourceFile,
    },
    Raster {
        commitment: String,
        data_file: SourceFile,
        _index_file: SourceFile,
        index: Arc<RasterIndex>,
    },
}

impl ResolvedSourceData {
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

    pub(crate) fn raster_index(&self) -> Option<&RasterIndex> {
        match self {
            Self::Raster { index, .. } => Some(index.as_ref()),
            Self::Postcard { .. } => None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn raster_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Raster { data_file, .. } => Some(data_file.bytes()),
            Self::Postcard { .. } => None,
        }
    }

    pub(crate) fn raster_data_file(&self) -> Option<&SourceFile> {
        match self {
            Self::Raster { data_file, .. } => Some(data_file),
            Self::Postcard { .. } => None,
        }
    }
}
