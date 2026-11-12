//! A child can write only a pending commitment. The successful parent publishes it.
use raster_core::{Error, Result};
use raster_prover::commitment_file::CommitmentFile;
use std::path::{Path, PathBuf};
pub(crate) struct PendingCommitment {
    pending: tempfile::TempPath,
    destination: PathBuf,
}
impl PendingCommitment {
    pub fn new(destination: &Path) -> Result<Self> {
        let destination = std::path::absolute(destination).map_err(Error::Io)?;
        let parent = destination.parent().expect("absolute commitment parent");
        let pending = tempfile::Builder::new()
            .prefix(".raster-commit-")
            .tempfile_in(parent)
            .map_err(Error::Io)?
            .into_temp_path();
        Ok(Self {
            pending,
            destination,
        })
    }
    pub fn path(&self) -> &Path {
        &self.pending
    }
    pub fn publish(self) -> Result<()> {
        CommitmentFile::read_memory(&self.pending).map_err(Error::Io)?;
        self.pending
            .persist(&self.destination)
            .map_err(|error| Error::Io(error.error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_or_abandoned_pending_output_preserves_existing_commitment() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("commit.bin");
        std::fs::write(&destination, b"previous commitment").unwrap();
        let pending = PendingCommitment::new(&destination).unwrap();
        let path = pending.path().to_owned();
        std::fs::write(&path, b"partial").unwrap();
        assert!(pending.publish().is_err());
        assert!(!path.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"previous commitment");
        let pending = PendingCommitment::new(&destination).unwrap();
        let path = pending.path().to_owned();
        drop(pending); // unsuccessful or cancelled child never calls publish
        assert!(!path.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"previous commitment");
    }
}
