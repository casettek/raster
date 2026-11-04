//! CLI ownership, binding and validation for bounded selective profiling.
use crate::{
    run_manifest::{cfs_digest, RunManifest},
    TraceFormat,
};
use raster_compiler::Project;
use raster_core::{cfs::ControlFlowSchema, Error, Result};
use raster_runtime::selective_profile::{Config, Manifest};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
};
#[derive(Default)]
pub struct Options {
    pub timing: bool,
    pub top_tiles: usize,
    pub calls_per_tile: usize,
    pub capture: Option<PathBuf>,
    pub max_bytes: u64,
}
impl Options {
    pub fn enabled(&self) -> bool {
        self.timing || self.capture.is_some()
    }
    pub fn validate(
        &self,
        no_auth: bool,
        commit: Option<&str>,
        audit: Option<&str>,
        save: bool,
        fraud: bool,
    ) -> Result<()> {
        if !self.enabled() {
            return Ok(());
        }
        if no_auth
            || commit.is_none()
            || commit.is_some_and(|p| p.starts_with("fraud_"))
            || audit.is_some()
            || save
            || fraud
            || (self.timing && self.capture.is_some())
            || self.top_tiles == 0
            || self.calls_per_tile == 0
            || self.max_bytes == 0
        {
            return Err(Error::Other("selective profiling requires honest authenticated --commit, positive limits, and no --save-trace or --audit".into()));
        }
        Ok(())
    }
}
pub fn file_hash(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}
fn argument_hash(value: Option<&str>) -> Result<serde_json::Value> {
    Ok(match value {
        Some(s) if Path::new(s).is_file() => serde_json::json!(file_hash(Path::new(s))?),
        Some(s) => serde_json::json!(raster_runtime::selective_profile::hash(s.as_bytes())),
        None => serde_json::Value::Null,
    })
}
pub(crate) fn output_hash(path: &Path) -> Result<serde_json::Value> {
    match file_hash(path) {
        Ok(hash) => Ok(serde_json::json!(hash)),
        Err(_) if !path.exists() => Ok(serde_json::Value::Null),
        Err(e) => Err(e),
    }
}
pub(crate) fn identity(
    project: &Project,
    cfs: &ControlFlowSchema,
    input: Option<&str>,
    manifest: Option<&str>,
) -> Result<serde_json::Value> {
    Ok(
        serde_json::json!({"cfs_digest":cfs_digest(cfs)?,"lock_sha256":file_hash(&project.root_dir.join("Raster.lock"))?,"input_sha256":argument_hash(input)?,"input_manifest_sha256":argument_hash(manifest)?}),
    )
}
pub fn read(path: &Path) -> Result<Manifest> {
    serde_json::from_slice(&std::fs::read(path)?).map_err(|e| Error::Other(e.to_string()))
}
fn write(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    std::fs::write(
        path,
        serde_json::to_vec_pretty(value).map_err(|e| Error::Other(e.to_string()))?,
    )?;
    Ok(())
}
pub fn prepare(
    options: &Options,
    dir: &Path,
    project: &Project,
    cfs: &ControlFlowSchema,
    input: Option<&str>,
    manifest: Option<&str>,
) -> Result<Option<PathBuf>> {
    if !options.enabled() {
        return Ok(None);
    }
    let current = identity(project, cfs, input, manifest)?;
    let selection = options.capture.as_ref().map(|p| read(p)).transpose()?;
    if let Some(s) = &selection {
        s.validate()?;
        if s.version != 1
            || s.kind != "native-selection"
            || !s.complete
            || s.binding["identity"] != current
            || s.step_count == 0
        {
            return Err(Error::Other("capture selection identity mismatch".into()));
        }
        let mut indices = std::collections::BTreeSet::new();
        for c in &s.calls {
            if c.exec_index == 0 || c.exec_index > s.step_count || !indices.insert(c.exec_index) {
                return Err(Error::Other("invalid selected invocation".into()));
            }
        }
    }
    let config = Config {
        timing: options.timing,
        top_tiles: selection
            .as_ref()
            .map_or(options.top_tiles, |s| s.top_tiles),
        calls_per_tile: selection
            .as_ref()
            .map_or(options.calls_per_tile, |s| s.calls_per_tile),
        max_bytes: options.max_bytes,
        directory: std::path::absolute(dir)?,
        selection,
    };
    let path = dir.join("selective-config.json");
    write(&path, &config)?;
    write(&dir.join("selective-identity.json"), &current)?;
    Ok(Some(path))
}
pub fn finalize(
    options: &Options,
    dir: &Path,
    run_id: &str,
    project: &Project,
    cfs: &ControlFlowSchema,
    input: Option<&str>,
    manifest: Option<&str>,
    commit: &str,
    format: TraceFormat,
) -> Result<()> {
    let current = identity(project, cfs, input, manifest)?;
    let initial: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("selective-identity.json"))?)
            .map_err(|e| Error::Other(e.to_string()))?;
    if initial != current {
        return Err(Error::Other(
            "profiling identity changed during native execution".into(),
        ));
    }
    let value = raster_prover::commitment_file::CommitmentFile::read_memory(Path::new(commit))
        .map_err(|e| Error::Other(e.to_string()))?;
    let canonical = postcard::to_allocvec(&value).map_err(|e| Error::Other(e.to_string()))?;
    let binding = serde_json::json!({"identity":current,"commitment_sha256":raster_runtime::selective_profile::hash(&canonical),"output_sha256":output_hash(&dir.join("output.bin"))?,"output_manifest_sha256":output_hash(&dir.join("output_manifest.json"))?});
    let mut collected = read(&dir.join("profile-samples.pending.json"))?;
    if let Some(path) = &options.capture {
        let wanted = read(path)?;
        if wanted.binding != binding {
            return Err(Error::Other(
                "capture output or commitment differs from native timing pass".into(),
            ));
        }
    }
    if !collected.complete || collected.step_count != value.len() as u64 {
        return Err(Error::Other("incomplete selective profile".into()));
    }
    collected.binding = binding;
    let name = if options.timing {
        "native-selection.json"
    } else {
        "selected-capture.json"
    };
    write(&dir.join(name), &collected)?;
    let mut run = RunManifest::new(run_id, &project.root_dir, input, manifest, format, cfs)?;
    run.version = 2;
    run.replay_source = Some(name.into());
    run.write(dir)?;
    println!("Selective profile manifest: {}", dir.join(name).display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selective_modes_refuse_raw_traces_and_unauthenticated_runs() {
        let options = Options {
            timing: true,
            top_tiles: 3,
            calls_per_tile: 1,
            max_bytes: 100,
            ..Default::default()
        };
        assert!(options
            .validate(false, Some("commit.bin"), None, false, false)
            .is_ok());
        assert!(options
            .validate(false, Some("fraud_commit.bin"), None, false, false)
            .is_err());
        assert!(options
            .validate(true, Some("commit.bin"), None, false, false)
            .is_err());
        assert!(options
            .validate(false, Some("commit.bin"), None, true, false)
            .is_err());
    }
}
