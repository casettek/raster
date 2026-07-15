mod document;
mod file;
mod resolved;

#[cfg(test)]
pub(crate) use file::sha256_hex;
pub(crate) use file::FileInputSourceResolver;
pub(crate) use resolved::{ResolvedSourceData, SourceFile};

use raster_core::input::ExternalEncoding;
use raster_core::Result;

/// Everything authorized storage loads need from outside the process: what
/// the public manifest declares about a source, and the source's bytes.
///
/// `FileInputSourceResolver` is the production implementation; tests can
/// provide fixture-backed resolvers without touching the filesystem.
pub(crate) trait SourceResolver: Send + Sync {
    /// The declared `(encoding, commitment)` for a named source, read from
    /// the manifest without touching the source's bytes.
    fn manifest_commitment_metadata(&self, name: &str) -> Result<(ExternalEncoding, String)>;

    fn resolve(&self, name: &str) -> Result<ResolvedSourceData>;
}
