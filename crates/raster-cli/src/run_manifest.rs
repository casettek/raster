//! `run.json`: what a later command needs to work from a run directory alone.
//!
//! Written by an authenticated `run` whose program exited successfully, next to
//! its trace. So its presence says the run completed and the trace is whole;
//! `--no-auth` runs record no trace and write none. See
//! `docs/proposals/tile-cost-estimator.md` §6 step 3.

use std::path::{Path, PathBuf};

use raster_core::cfs::ControlFlowSchema;
use raster_core::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::TraceFormat;

pub const RUN_MANIFEST_FILE: &str = "run.json";
pub const RUN_MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunManifest {
    pub version: u32,
    pub run_id: String,
    /// The project the run built and executed. Absolute.
    pub project_root: PathBuf,
    /// The `--input` the run was given, made absolute. Replaying an
    /// entry-argument binding needs the same input context.
    pub input: Option<PathBuf>,
    /// The `--input-manifest` the run was given, made absolute.
    pub input_manifest: Option<PathBuf>,
    pub trace_format: TraceFormat,
    /// Hex sha256 of the CFS's JSON. A reader rebuilding the CFS from the
    /// project compares against it to notice that the project changed since
    /// the run. Not the program commitment: that needs `program.bin` or
    /// `Raster.lock`, which a plain `run` may not have.
    pub cfs_digest: String,
}

impl RunManifest {
    pub fn new(
        run_id: &str,
        project_root: &Path,
        input: Option<&str>,
        input_manifest: Option<&str>,
        trace_format: TraceFormat,
        cfs: &ControlFlowSchema,
    ) -> Result<Self> {
        Ok(Self {
            version: RUN_MANIFEST_VERSION,
            run_id: run_id.to_string(),
            project_root: absolute(Path::new(project_root))?,
            input: input.map(|path| absolute(Path::new(path))).transpose()?,
            input_manifest: input_manifest
                .map(|path| absolute(Path::new(path)))
                .transpose()?,
            trace_format,
            cfs_digest: cfs_digest(cfs)?,
        })
    }

    pub fn write(&self, run_dir: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(run_dir.join(RUN_MANIFEST_FILE), json)?;
        Ok(())
    }

    pub fn read(run_dir: &Path) -> Result<Self> {
        let path = run_dir.join(RUN_MANIFEST_FILE);
        let bytes = std::fs::read(&path).map_err(|error| {
            Error::Other(format!(
                "{} not found ({error}). It is written by an authenticated `cargo raster run` \
                 whose program completed; --no-auth and failed runs have none.",
                path.display()
            ))
        })?;
        let manifest: Self = serde_json::from_slice(&bytes)
            .map_err(|error| Error::Other(format!("{} is malformed: {error}", path.display())))?;
        if manifest.version != RUN_MANIFEST_VERSION {
            return Err(Error::Other(format!(
                "{} has version {}, expected {RUN_MANIFEST_VERSION}",
                path.display(),
                manifest.version
            )));
        }
        Ok(manifest)
    }

    pub fn trace_path(&self, run_dir: &Path) -> PathBuf {
        run_dir.join(self.trace_format.trace_file_name())
    }
}

/// Hex sha256 of the CFS's JSON. The CFS holds only vectors, no unordered
/// maps, so its JSON is deterministic.
pub fn cfs_digest(cfs: &ControlFlowSchema) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(cfs)?)))
}

fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path)
        .map_err(|error| Error::Other(format!("Cannot resolve {}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::cfs::TileDef;

    fn cfs() -> ControlFlowSchema {
        ControlFlowSchema {
            version: "1.0".into(),
            project: "p".into(),
            encoding: "postcard".into(),
            tiles: vec![TileDef::iter("a", 1, 1)],
            sequences: vec![],
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("raster-run-manifest-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips_through_the_run_directory() {
        let dir = scratch("round-trip");
        let manifest = RunManifest::new(
            "run-1",
            Path::new("/project"),
            Some("input.json"),
            None,
            TraceFormat::Json,
            &cfs(),
        )
        .unwrap();
        // Relative inputs are stored absolute.
        assert!(manifest.input.as_ref().unwrap().is_absolute());
        assert!(manifest.input.as_ref().unwrap().ends_with("input.json"));
        manifest.write(&dir).unwrap();
        assert_eq!(RunManifest::read(&dir).unwrap(), manifest);
        assert_eq!(manifest.trace_path(&dir), dir.join("trace.ndjson"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_or_foreign_manifest_is_an_error_that_says_why() {
        let dir = scratch("missing");
        let missing = RunManifest::read(&dir).unwrap_err().to_string();
        assert!(missing.contains("run.json not found"), "{missing}");

        let mut manifest = RunManifest::new(
            "r",
            Path::new("/p"),
            None,
            None,
            TraceFormat::Binary,
            &cfs(),
        )
        .unwrap();
        manifest.version = RUN_MANIFEST_VERSION + 1;
        manifest.write(&dir).unwrap();
        let foreign = RunManifest::read(&dir).unwrap_err().to_string();
        assert!(foreign.contains("version"), "{foreign}");

        std::fs::write(dir.join(RUN_MANIFEST_FILE), b"{").unwrap();
        let malformed = RunManifest::read(&dir).unwrap_err().to_string();
        assert!(malformed.contains("malformed"), "{malformed}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_cfs_digest_is_stable_and_follows_the_cfs() {
        assert_eq!(cfs_digest(&cfs()).unwrap(), cfs_digest(&cfs()).unwrap());
        let mut changed = cfs();
        changed.tiles.push(TileDef::iter("b", 1, 1));
        assert_ne!(cfs_digest(&cfs()).unwrap(), cfs_digest(&changed).unwrap());
    }
}
