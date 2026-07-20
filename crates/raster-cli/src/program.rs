//! Host-side assembly of a raster program's identity artifacts.
//!
//! Turns a project (source + optional `Raster.toml`) into a
//! [`ProgramDefinition`] (see `docs/proposals/program-identity.md`), writes the
//! canonical `program.bin` frame and the `Raster.lock` claim, and verifies a
//! reassembled definition against a previously written lock (drift check).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use raster_compiler::tile::TileDiscovery;
use raster_compiler::Project;
use raster_core::cfs::ControlFlowSchema;
use raster_core::input::ExternalEncoding;
use raster_core::program::{ImageId, InterfaceDecl, ProgramDefinition, ProgramManifest};
use raster_core::{Error, Result};
use raster_prover::replay::Replayer;
use serde::{Deserialize, Serialize};

/// `Raster.toml` document shape (authored program interface).
#[derive(Debug, Deserialize)]
struct RasterToml {
    program: ProgramSection,
    #[serde(default)]
    inputs: BTreeMap<String, TomlInterface>,
    #[serde(default)]
    output: Option<TomlInterface>,
}

#[derive(Debug, Deserialize)]
struct ProgramSection {
    name: String,
    #[serde(default = "default_version")]
    version: String,
}

fn default_version() -> String {
    "0.0.0".to_string()
}

#[derive(Debug, Deserialize)]
struct TomlInterface {
    #[serde(rename = "type")]
    type_path: String,
    #[serde(default)]
    encoding: ExternalEncoding,
}

impl From<TomlInterface> for InterfaceDecl {
    fn from(t: TomlInterface) -> Self {
        InterfaceDecl {
            type_path: t.type_path,
            encoding: t.encoding,
        }
    }
}

/// The derived lock: the identity claim tying `program_commitment` to a source
/// revision, plus the per-tile image ids and build toolchain. Cargo.lock
/// semantics — deterministic, checked in.
#[derive(Debug, Serialize, Deserialize)]
pub struct RasterLock {
    pub format: u32,
    pub program_commitment: String,
    pub tiles: BTreeMap<String, LockTile>,
    pub toolchain: LockToolchain,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LockTile {
    pub image_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LockToolchain {
    pub risc0: String,
    pub build_mode: String,
}

/// Path of `Raster.toml` at the project root.
fn raster_toml_path(project: &Project) -> PathBuf {
    project.root_dir.join("Raster.toml")
}

/// Path of `Raster.lock` at the project root.
pub fn raster_lock_path(project: &Project) -> PathBuf {
    project.root_dir.join("Raster.lock")
}

/// Load `main`'s declared interface: parse `Raster.toml` if present, otherwise
/// synthesize it from the CFS + main's signature (types from the AST). Either
/// way the result is enforced against the CFS by [`ProgramDefinition::assemble`].
pub fn load_or_synthesize_manifest(
    project: &Project,
    cfs: &ControlFlowSchema,
) -> Result<ProgramManifest> {
    let path = raster_toml_path(project);
    if path.exists() {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Other(format!("failed to read {}: {e}", path.display())))?;
        let doc: RasterToml = toml::from_str(&text)
            .map_err(|e| Error::Other(format!("failed to parse {}: {e}", path.display())))?;
        Ok(ProgramManifest {
            name: doc.program.name,
            version: doc.program.version,
            inputs: doc.inputs.into_iter().map(|(k, v)| (k, v.into())).collect(),
            output: doc.output.map(Into::into),
        })
    } else {
        Ok(synthesize_manifest(project, cfs))
    }
}

/// Build a manifest from the CFS + main's AST signature: real parameter/return
/// type paths, `raster` encoding. Used when no `Raster.toml` is authored.
fn synthesize_manifest(project: &Project, cfs: &ControlFlowSchema) -> ProgramManifest {
    let main_seq = cfs.sequences.iter().find(|s| s.id == "main");
    let entry_arguments = main_seq
        .map(|s| s.entry_arguments.clone())
        .unwrap_or_default();
    let produces_output = main_seq.map(|s| s.produces_output).unwrap_or(false);

    let main_ast = project.ast.functions.iter().find(|f| f.name == "main");
    let type_of = |name: &str| -> String {
        main_ast
            .and_then(|f| {
                f.input_names
                    .iter()
                    .position(|n| n == name)
                    .and_then(|i| f.inputs.get(i).cloned())
            })
            .unwrap_or_default()
    };

    let inputs = entry_arguments
        .into_iter()
        .map(|name| {
            let type_path = type_of(&name);
            (
                name,
                InterfaceDecl {
                    type_path,
                    encoding: ExternalEncoding::Raster,
                },
            )
        })
        .collect();

    let output = produces_output.then(|| InterfaceDecl {
        type_path: main_ast
            .and_then(|f| f.output.clone())
            .unwrap_or_default(),
        encoding: ExternalEncoding::Raster,
    });

    ProgramManifest {
        name: cfs.project.clone(),
        version: "0.0.0".to_string(),
        inputs,
        output,
    }
}

/// Compile every CFS tile to its image id (the program's tile registry).
fn build_registry(cfs: &ControlFlowSchema, replayer: &Replayer) -> Result<BTreeMap<String, ImageId>> {
    let mut tiles = BTreeMap::new();
    for tile in &cfs.tiles {
        let image_id = replayer.tile_image_id(&tile.id)?;
        tiles.insert(tile.id.clone(), image_id);
    }
    Ok(tiles)
}

/// Assemble the full `ProgramDefinition` for a project: CFS + tile registry +
/// manifest, validated by [`ProgramDefinition::assemble`].
pub fn assemble_program(
    project: &Project,
    cfs: &ControlFlowSchema,
    replayer: &Replayer,
) -> Result<ProgramDefinition> {
    let manifest = load_or_synthesize_manifest(project, cfs)?;
    let tiles = build_registry(cfs, replayer)?;
    ProgramDefinition::assemble(manifest, cfs.clone(), tiles)
}

/// Source hash a tile's replay guest is keyed on, for the lock (best-effort).
fn tile_source_hash(project: &Project, tile_id: &str) -> Option<String> {
    let discovery = TileDiscovery::new(project);
    discovery.get(tile_id).and_then(|t| t.to_content_hash())
}

/// Write `program.bin` (canonical frame) and `Raster.lock` for a definition.
/// Returns the two paths.
pub fn write_program_artifacts(
    project: &Project,
    program: &ProgramDefinition,
    output_dir: &Path,
) -> Result<(PathBuf, PathBuf)> {
    std::fs::create_dir_all(output_dir)
        .map_err(|e| Error::Other(format!("failed to create {}: {e}", output_dir.display())))?;

    let bin_path = output_dir.join("program.bin");
    std::fs::write(&bin_path, program.canonical_bytes())
        .map_err(|e| Error::Other(format!("failed to write {}: {e}", bin_path.display())))?;

    let lock = RasterLock {
        format: 1,
        program_commitment: hex::encode(program.commitment()),
        tiles: program
            .tiles
            .iter()
            .map(|(id, image_id)| {
                (
                    id.clone(),
                    LockTile {
                        image_id: hex::encode(image_id),
                        source_hash: tile_source_hash(project, id),
                    },
                )
            })
            .collect(),
        toolchain: LockToolchain {
            // The risc0 toolchain that produced these image ids. Recorded so a
            // verifier can detect a mismatch; exact provenance/reproducibility
            // (docker builds) is future work (see program-identity.md).
            risc0: option_env!("RISC0_VERSION")
                .unwrap_or("risc0-zkvm-1.2")
                .to_string(),
            // Local builds are dev-grade; docker reproducible builds are future
            // work (see program-identity.md).
            build_mode: "local".to_string(),
        },
    };
    let lock_path = raster_lock_path(project);
    let lock_json = serde_json::to_string_pretty(&lock)
        .map_err(|e| Error::Other(format!("failed to serialize Raster.lock: {e}")))?;
    std::fs::write(&lock_path, lock_json + "\n")
        .map_err(|e| Error::Other(format!("failed to write {}: {e}", lock_path.display())))?;

    Ok((bin_path, lock_path))
}

/// Reassemble the program and, if a `Raster.lock` exists, require the recomputed
/// `program_commitment` to equal the lock's (the stale-lock drift check). Returns
/// the reassembled definition.
pub fn reassemble_and_verify(
    project: &Project,
    cfs: &ControlFlowSchema,
    replayer: &Replayer,
) -> Result<ProgramDefinition> {
    let program = assemble_program(project, cfs, replayer)?;
    check_lock_drift(&program, &raster_lock_path(project))?;
    Ok(program)
}

/// If `lock_path` exists, require the program's recomputed commitment to equal
/// the lock's (the stale-lock drift check). Absent lock is not an error.
fn check_lock_drift(program: &ProgramDefinition, lock_path: &Path) -> Result<()> {
    if !lock_path.exists() {
        return Ok(());
    }
    let text = std::fs::read_to_string(lock_path)
        .map_err(|e| Error::Other(format!("failed to read {}: {e}", lock_path.display())))?;
    let lock: RasterLock = serde_json::from_str(&text)
        .map_err(|e| Error::Other(format!("failed to parse {}: {e}", lock_path.display())))?;
    let recomputed = hex::encode(program.commitment());
    if recomputed != lock.program_commitment {
        return Err(Error::Other(format!(
            "program identity drift: reassembled commitment {recomputed} != Raster.lock {} — run `cargo raster build`",
            lock.program_commitment
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster_core::cfs::{SequenceChildItem, SequenceDef, TileDef, TileItem};

    fn cfs_with_main(entry_arguments: Vec<String>, produces_output: bool) -> ControlFlowSchema {
        ControlFlowSchema {
            version: "1.0".to_string(),
            project: "demo".to_string(),
            encoding: "postcard".to_string(),
            tiles: vec![TileDef::iter("greet", 1, 1)],
            sequences: vec![SequenceDef {
                id: "main".to_string(),
                input_sources: vec![],
                items: vec![SequenceChildItem::Tile(TileItem {
                    id: "greet".to_string(),
                    sources: vec![],
                })],
                entry_arguments,
                produces_output,
            }],
        }
    }

    fn program_from(cfs: &ControlFlowSchema, manifest: ProgramManifest) -> ProgramDefinition {
        let tiles = cfs
            .tiles
            .iter()
            .map(|t| (t.id.clone(), [1u8; 32]))
            .collect();
        ProgramDefinition::assemble(manifest, cfs.clone(), tiles).expect("assemble")
    }

    #[test]
    fn parses_raster_toml() {
        let dir = std::env::temp_dir().join(format!("raster-toml-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("Raster.toml"),
            r#"
[program]
name = "myprog"
version = "1.2.3"

[inputs.seed]
type = "u64"
encoding = "raster"

[output]
type = "String"
encoding = "raster"
"#,
        )
        .unwrap();
        let text = std::fs::read_to_string(dir.join("Raster.toml")).unwrap();
        let doc: RasterToml = toml::from_str(&text).unwrap();
        assert_eq!(doc.program.name, "myprog");
        assert_eq!(doc.program.version, "1.2.3");
        assert_eq!(doc.inputs.get("seed").unwrap().type_path, "u64");
        assert_eq!(doc.output.as_ref().unwrap().type_path, "String");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lock_round_trips_and_detects_drift() {
        let cfs = cfs_with_main(vec!["seed".to_string()], true);
        let manifest = ProgramManifest {
            name: "demo".to_string(),
            version: "0.1.0".to_string(),
            inputs: [(
                "seed".to_string(),
                InterfaceDecl {
                    type_path: "u64".to_string(),
                    encoding: ExternalEncoding::Raster,
                },
            )]
            .into_iter()
            .collect(),
            output: Some(InterfaceDecl {
                type_path: "String".to_string(),
                encoding: ExternalEncoding::Raster,
            }),
        };
        let program = program_from(&cfs, manifest);

        let dir = std::env::temp_dir().join(format!("raster-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock_path = dir.join("Raster.lock");

        // Write a lock matching the program, then check drift passes.
        let lock = RasterLock {
            format: 1,
            program_commitment: hex::encode(program.commitment()),
            tiles: Default::default(),
            toolchain: LockToolchain {
                risc0: "test".to_string(),
                build_mode: "local".to_string(),
            },
        };
        std::fs::write(&lock_path, serde_json::to_string_pretty(&lock).unwrap()).unwrap();
        assert!(check_lock_drift(&program, &lock_path).is_ok());

        // A lock with a different commitment must be detected as drift.
        let stale = RasterLock {
            program_commitment: hex::encode([9u8; 32]),
            ..lock
        };
        std::fs::write(&lock_path, serde_json::to_string_pretty(&stale).unwrap()).unwrap();
        let err = check_lock_drift(&program, &lock_path).unwrap_err();
        assert!(err.to_string().contains("drift"), "got: {err}");

        // No lock file → no error.
        std::fs::remove_file(&lock_path).unwrap();
        assert!(check_lock_drift(&program, &lock_path).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }
}
