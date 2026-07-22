use memmap2::Mmap;
use std::sync::Arc;

use crate::raster_index::RasterIndex;

#[derive(Debug, Clone)]
pub(crate) enum SourceFile {
    Read(Arc<[u8]>),
    Mmap(Arc<Mmap>),
}

impl SourceFile {
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Read(bytes) => bytes,
            Self::Mmap(map) => map,
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

    pub(crate) fn raster_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Raster { data_file, .. } => Some(data_file.bytes()),
            Self::Postcard { .. } => None,
        }
    }
}
