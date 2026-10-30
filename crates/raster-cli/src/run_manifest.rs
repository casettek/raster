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
    /// The `--input` the run was given, as the input reader takes it: a file
    /// path, made absolute, or inline JSON, kept verbatim. Replaying an
    /// entry-argument binding needs the same input context.
    pub input: Option<String>,
    /// The `--input-manifest` the run was given, stored like `input`.
    pub input_manifest: Option<String>,
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
        let project_root = absolute(project_root)?;
        Ok(Self {
            version: RUN_MANIFEST_VERSION,
            run_id: run_id.to_string(),
            input: input
                .map(|raw| input_argument(&project_root, raw))
                .transpose()?,
            input_manifest: input_manifest
                .map(|raw| input_argument(&project_root, raw))
                .transpose()?,
            project_root,
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

/// Store an `--input`/`--input-manifest` argument so that it reads back the
/// same way. The input reader (`raster-runtime`, `source/document.rs`) takes an
/// argument that names a file as that file, and anything else as inline JSON.
/// The run's program reads it from the project root, so a relative path is
/// resolved there. Inline JSON must stay verbatim: made "absolute", it would
/// no longer parse.
fn input_argument(project_root: &Path, raw: &str) -> Result<String> {
    let path = project_root.join(raw);
    if !path.is_file() {
        return Ok(raw.to_string());
    }
    let path = absolute(&path)?;
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| Error::Other(format!("Input path is not UTF-8: {}", path.display())))
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
        std::fs::write(dir.join("input.json"), b"{}").unwrap();
        let manifest = RunManifest::new(
            "run-1",
            &dir,
            Some("input.json"),
            None,
            TraceFormat::Json,
            &cfs(),
        )
        .unwrap();
        manifest.write(&dir).unwrap();
        assert_eq!(RunManifest::read(&dir).unwrap(), manifest);
        assert_eq!(manifest.trace_path(&dir), dir.join("trace.ndjson"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn input_arguments_read_back_the_way_the_run_read_them() {
        let project = scratch("inputs");
        std::fs::write(project.join("input.json"), b"{}").unwrap();
        let inline = "{\n  \"seed\": { \"path\": \"seed.rastered\" }\n}";
        let manifest = RunManifest::new(
            "run-1",
            &project,
            Some("input.json"),
            Some(inline),
            TraceFormat::Binary,
            &cfs(),
        )
        .unwrap();
        // A file, named relative to the project root, is stored absolute.
        let input = PathBuf::from(manifest.input.as_deref().unwrap());
        assert!(input.is_absolute());
        assert_eq!(input, project.join("input.json"));
        // Inline JSON is stored verbatim, so it still parses.
        assert_eq!(manifest.input_manifest.as_deref(), Some(inline));

        // A name that is no file is what the reader parses as JSON: kept as is.
        let missing = RunManifest::new(
            "run-2",
            &project,
            Some("missing.json"),
            None,
            TraceFormat::Binary,
            &cfs(),
        )
        .unwrap();
        assert_eq!(missing.input.as_deref(), Some("missing.json"));
        std::fs::remove_dir_all(project).unwrap();
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
