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
            schema_hash: [0u8; 32],
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
    /// Digest of everything the tile guests compile besides each tile's own
    /// source and the compiler: raster, the project's local crates, dependency
    /// versions and features, the guest template. `run --commit` and
    /// `chain run` recompute it, building nothing, and refuse to commit when
    /// it differs ([`check_lock_before_commit`]). Absent before format 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_inputs: Option<String>,
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
    /// The guest compiler's `rustc -V` line, e.g. `rustc 1.94.1-dev
    /// (06e01cb0d 2026-04-09)`. Tile image ids depend on it, not only on the
    /// sources: a lock built by another compiler drifts. Absent in format-1
    /// locks, and when no risc0 toolchain was installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rustc: Option<String>,
}

/// Current `Raster.lock` format. 2: `toolchain.rustc`. 3: `guest_inputs`.
pub const LOCK_FORMAT: u32 = 3;

/// The project's current guest inputs ([`RasterLock::guest_inputs`]).
fn guest_inputs(project: &Project) -> std::result::Result<String, String> {
    raster_backend_risc0::guest_inputs(&project.output_dir, &project.root_dir)
}

/// The `rustc -V` line of the risc0 Rust toolchain tile guests build with: the
/// same selection `raster-guest-build` makes. `None` when none is installed.
fn guest_rustc() -> Option<String> {
    raster_guest_build::Toolchain::find()
        .ok()?
        .rustc_version()
        .ok()
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
                    schema_hash: [0u8; 32],
                },
            )
        })
        .collect();

    let output = produces_output.then(|| InterfaceDecl {
        type_path: main_ast.and_then(|f| f.output.clone()).unwrap_or_default(),
        encoding: ExternalEncoding::Raster,
        schema_hash: [0u8; 32],
    });

    ProgramManifest {
        name: cfs.project.clone(),
        version: "0.0.0".to_string(),
        inputs,
        output,
    }
}

/// Compile every CFS tile to its image id (the program's tile registry).
fn build_registry(
    cfs: &ControlFlowSchema,
    replayer: &Replayer,
) -> Result<BTreeMap<String, ImageId>> {
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
    let mut manifest = load_or_synthesize_manifest(project, cfs)?;
    raster_compiler::schema_walk::fill_schema_hashes(&project.ast, &mut manifest)?;
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

    let rustc = guest_rustc();
    let lock_path = raster_lock_path(project);
    if let (Ok(previous), Some(current)) = (read_lock(&lock_path), rustc.as_deref()) {
        if let Some(recorded) = previous.toolchain.rustc.as_deref() {
            if recorded != current {
                eprintln!(
                    "warning: {} was built with `{recorded}`; this build uses `{current}`. \
                     Tile image ids depend on the guest compiler, so they may all change.",
                    lock_path.display()
                );
            }
        }
    }
    let inputs = guest_inputs(project)
        .map_err(|e| {
            eprintln!(
                "warning: {} records no guest inputs ({e}); `run --commit` cannot check it",
                lock_path.display()
            )
        })
        .ok();
    let lock = RasterLock {
        format: LOCK_FORMAT,
        guest_inputs: inputs,
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
            // Defaults to the risc0-zkvm this CLI links, so the label follows
            // the dependency instead of going stale on a version bump. Tile
            // guests resolve `risc0-zkvm` on their own, so it can trail a
            // guest's exact patch release.
            risc0: option_env!("RISC0_VERSION")
                .map(str::to_string)
                .unwrap_or_else(|| format!("risc0-zkvm-{}", risc0_zkvm::VERSION)),
            // Local, but path-independent (path-independent-builds.md): equal
            // ids from any checkout path, given the same sources, Cargo.lock
            // and guest compiler (`rustc`).
            build_mode: "local".to_string(),
            rustc,
        },
    };
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

/// Reassemble a program's [`ProgramDefinition`] from checked-in files only — the
/// source CFS, the manifest, and the tile image ids recorded in `Raster.lock` —
/// with no guest recompilation. This is the on-demand regeneration of the
/// `program.bin` frame when its build cache is cold: the light identity/audit
/// path, which needs `Raster.lock` but no risc0 toolchain. The prover path never
/// uses this — it recompiles tiles and drift-checks them ([`reassemble_and_verify`]).
pub fn reassemble_from_lock(
    project: &Project,
    cfs: &ControlFlowSchema,
) -> Result<ProgramDefinition> {
    let mut manifest = load_or_synthesize_manifest(project, cfs)?;
    // As `assemble_program` does: the interface's schema hashes are part of the
    // commitment. Without them (missing here from 2026-08-14 until 2026-11-04)
    // the reassembled commitment differed from the lock's for any program with
    // a typed input or output.
    raster_compiler::schema_walk::fill_schema_hashes(&project.ast, &mut manifest)?;
    let lock = read_lock(&raster_lock_path(project))?;
    let mut tiles = BTreeMap::new();
    for tile in &cfs.tiles {
        let entry = lock.tiles.get(&tile.id).ok_or_else(|| {
            Error::Other(format!(
                "Raster.lock records no image id for tile '{}' — run `cargo raster build --backend risc0`",
                tile.id
            ))
        })?;
        tiles.insert(tile.id.clone(), decode_image_id(&entry.image_id, &tile.id)?);
    }
    ProgramDefinition::assemble(manifest, cfs.clone(), tiles)
}

/// Decode a `Raster.lock` image-id hex string into the 32-byte digest.
fn decode_image_id(hex_str: &str, tile_id: &str) -> Result<ImageId> {
    let bytes = hex::decode(hex_str).map_err(|e| {
        Error::Other(format!(
            "Raster.lock image id for tile '{tile_id}' is not valid hex: {e}"
        ))
    })?;
    let len = bytes.len();
    bytes.try_into().map_err(|_| {
        Error::Other(format!(
            "Raster.lock image id for tile '{tile_id}' is {len} bytes, expected 32"
        ))
    })
}

/// Read and parse `Raster.lock`. A missing lock is an error here (callers that
/// tolerate absence check existence first, e.g. [`check_lock_drift`]).
pub(crate) fn read_lock(lock_path: &Path) -> Result<RasterLock> {
    let text = std::fs::read_to_string(lock_path).map_err(|_| {
        Error::Other(format!(
            "{} not found — run `cargo raster build --backend risc0`",
            lock_path.display()
        ))
    })?;
    serde_json::from_str(&text)
        .map_err(|e| Error::Other(format!("failed to parse {}: {e}", lock_path.display())))
}

/// If `lock_path` exists, require the program's recomputed commitment to equal
/// the lock's (the stale-lock drift check). Absent lock is not an error.
/// The remedy named in these errors is `--backend risc0` on purpose: plain
/// `cargo raster build` defaults to the native backend, and
/// `commands::build` emits `program.bin` + `Raster.lock` **only** under risc0
/// (image ids need compiled guests). Advising the bare command sent a reader
/// in a circle — it reports success, changes nothing, and the same drift
/// error returns.
fn check_lock_drift(program: &ProgramDefinition, lock_path: &Path) -> Result<()> {
    if !lock_path.exists() {
        return Ok(());
    }
    let lock = read_lock(lock_path)?;
    let recomputed = hex::encode(program.commitment());
    match drift_error(&lock, &recomputed, guest_rustc().as_deref()) {
        Some(message) => Err(Error::Other(message)),
        None => Ok(()),
    }
}

/// The drift error for `lock` against the `recomputed` commitment, if they
/// differ. When the lock records a different guest compiler than
/// `current_rustc`, the error says so: that alone moves every tile id, and
/// rebuilding the lock is then the wrong remedy for a verifier.
fn drift_error(lock: &RasterLock, recomputed: &str, current_rustc: Option<&str>) -> Option<String> {
    if recomputed == lock.program_commitment {
        return None;
    }
    let mut message = format!(
        "program identity drift: reassembled commitment {recomputed} != Raster.lock {}",
        lock.program_commitment
    );
    match (lock.toolchain.rustc.as_deref(), current_rustc) {
        (Some(recorded), Some(current)) if recorded != current => message.push_str(&format!(
            ". Raster.lock was built with guest compiler `{recorded}`, this machine has \
             `{current}`, and tile image ids depend on it: install the lock's toolchain \
             (`rzup install rust <version>`) to reproduce it, or run \
             `cargo raster build --backend risc0` to replace it"
        )),
        _ => message.push_str(" — run `cargo raster build --backend risc0`"),
    }
    Some(message)
}

/// Before `run --commit` or `chain run` commits anything: is `Raster.lock`
/// still what building the current sources would write? Checked without
/// building any guest: the program commitment recomputed from the current
/// program and the lock's tile ids, each tile's source hash, the lock's
/// [`guest_inputs`](RasterLock::guest_inputs), and the guest compiler.
///
/// A stale lock is an error. A commitment made from it names a program
/// identity that no one can reproduce, so it could later be neither proven nor
/// defended; today that surfaces only at prove time, as `program identity
/// drift`. No lock means no identity is claimed: nothing to check. When a
/// check cannot run (a lock from before format 3, no risc0 compiler here, a
/// project that does not resolve), it is skipped with a warning.
pub fn check_lock_before_commit(project: &Project, cfs: &ControlFlowSchema) -> Result<()> {
    let lock_path = raster_lock_path(project);
    if !lock_path.exists() {
        return Ok(());
    }
    let lock = read_lock(&lock_path)?;
    let tiles: BTreeMap<String, Option<String>> = cfs
        .tiles
        .iter()
        .map(|t| (t.id.clone(), tile_source_hash(project, &t.id)))
        .collect();
    let recomputed = reassemble_from_lock(project, cfs)
        .ok()
        .map(|program| hex::encode(program.commitment()));
    let inputs = match lock.guest_inputs {
        None => {
            eprintln!(
                "warning: {} (format {}) records no guest inputs, so only part of it can be \
                 checked; `cargo raster build --backend risc0` re-locks it",
                lock_path.display(),
                lock.format
            );
            None
        }
        Some(_) => guest_inputs(project)
            .map_err(|e| {
                eprintln!(
                    "warning: could not check {}'s guest inputs: {e}",
                    lock_path.display()
                )
            })
            .ok(),
    };
    let rustc = guest_rustc();
    let reasons = lock_staleness(
        &lock,
        &tiles,
        recomputed.as_deref(),
        inputs.as_deref(),
        rustc.as_deref(),
    );
    if reasons.is_empty() {
        return Ok(());
    }
    let compiler_only = reasons.len() == 1 && reasons[0].starts_with("the guest compiler");
    Err(Error::Other(format!(
        "{} no longer matches this program, so a commitment made now could not be proven or \
         defended:\n  - {}\n{}",
        lock_path.display(),
        reasons.join("\n  - "),
        if compiler_only {
            "Install the lock's toolchain (`rzup install rust <version>`), or re-lock with \
             `cargo raster build --backend risc0` (new tile ids)"
        } else {
            "Re-lock with `cargo raster build --backend risc0`"
        }
    )))
}

/// Why `lock` does not match the current program: one line per difference,
/// none when it matches. `tiles` maps each current tile to its source hash;
/// `recomputed` is the program commitment from the current program and the
/// lock's tile ids. A `None` input is not compared.
fn lock_staleness(
    lock: &RasterLock,
    tiles: &BTreeMap<String, Option<String>>,
    recomputed: Option<&str>,
    inputs: Option<&str>,
    rustc: Option<&str>,
) -> Vec<String> {
    let mut reasons = Vec::new();
    let mut edited = Vec::new();
    for (tile, source) in tiles {
        match lock.tiles.get(tile) {
            None => reasons.push(format!("tile `{tile}` is new: the lock has no image id for it")),
            Some(entry) => {
                if let (Some(locked), Some(current)) = (entry.source_hash.as_deref(), source) {
                    if locked != current {
                        edited.push(tile.as_str());
                    }
                }
            }
        }
    }
    // Tiles share source files, so one edit can touch many: name a few.
    match edited.as_slice() {
        [] => {}
        few if few.len() <= 3 => {
            reasons.extend(few.iter().map(|tile| format!("tile `{tile}`'s source changed")))
        }
        many => reasons.push(format!(
            "{} tiles' sources changed: `{}`, `{}`, `{}` and {} more",
            many.len(),
            many[0],
            many[1],
            many[2],
            many.len() - 3
        )),
    }
    for tile in lock.tiles.keys().filter(|t| !tiles.contains_key(*t)) {
        reasons.push(format!("tile `{tile}` is in the lock but no longer in the program"));
    }
    if let Some(recomputed) = recomputed {
        if recomputed != lock.program_commitment {
            reasons.push(
                "the program's control flow or interface changed (its commitment no longer \
                 matches the lock's)"
                    .to_string(),
            );
        }
    }
    if let (Some(locked), Some(current)) = (lock.guest_inputs.as_deref(), inputs) {
        if locked != current {
            reasons.push(
                "the guest inputs changed: raster, a local crate, a dependency version or \
                 feature, or the guest template"
                    .to_string(),
            );
        }
    }
    if let (Some(locked), Some(current)) = (lock.toolchain.rustc.as_deref(), rustc) {
        if locked != current {
            reasons.push(format!(
                "the guest compiler changed: the lock has `{locked}`, this machine `{current}`"
            ));
        }
    }
    reasons
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
                returns: None,
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
                    schema_hash: [0u8; 32],
                },
            )]
            .into_iter()
            .collect(),
            output: Some(InterfaceDecl {
                type_path: "String".to_string(),
                encoding: ExternalEncoding::Raster,
                schema_hash: [0u8; 32],
            }),
        };
        let program = program_from(&cfs, manifest);

        let dir = std::env::temp_dir().join(format!("raster-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lock_path = dir.join("Raster.lock");

        // Write a lock matching the program, then check drift passes.
        let lock = RasterLock {
            format: 1,
            guest_inputs: None,
            program_commitment: hex::encode(program.commitment()),
            tiles: Default::default(),
            toolchain: LockToolchain {
                risc0: "test".to_string(),
                build_mode: "local".to_string(),
                rustc: None,
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

    fn lock_with(commitment: [u8; 32], rustc: Option<&str>) -> RasterLock {
        RasterLock {
            format: LOCK_FORMAT,
            guest_inputs: None,
            program_commitment: hex::encode(commitment),
            tiles: Default::default(),
            toolchain: LockToolchain {
                risc0: "test".to_string(),
                build_mode: "local".to_string(),
                rustc: rustc.map(str::to_string),
            },
        }
    }

    #[test]
    fn drift_names_both_guest_compilers_when_they_differ() {
        let recomputed = hex::encode([1u8; 32]);
        let old = "rustc 1.93.0-dev (aaaaaaaaa 2026-01-01)";
        let new = "rustc 1.94.1-dev (06e01cb0d 2026-04-09)";

        assert_eq!(drift_error(&lock_with([1u8; 32], Some(old)), &recomputed, Some(new)), None,
            "an equal commitment is not drift, whatever the compilers");

        let message = drift_error(&lock_with([2u8; 32], Some(old)), &recomputed, Some(new)).unwrap();
        assert!(message.contains(old) && message.contains(new), "{message}");
        assert!(message.contains("rzup install rust"), "{message}");

        for (recorded, current) in [(Some(new), Some(new)), (None, Some(new)), (Some(old), None)] {
            let message = drift_error(&lock_with([2u8; 32], recorded), &recomputed, current).unwrap();
            assert!(message.contains("run `cargo raster build --backend risc0`"), "{message}");
            assert!(!message.contains("guest compiler"), "{message}");
        }
    }

    fn locked(tiles: &[(&str, &str)], inputs: &str, rustc: &str) -> RasterLock {
        let mut lock = lock_with([1u8; 32], Some(rustc));
        lock.guest_inputs = Some(inputs.to_string());
        lock.tiles = tiles
            .iter()
            .map(|(id, source)| {
                (
                    id.to_string(),
                    LockTile {
                        image_id: "00".into(),
                        source_hash: Some(source.to_string()),
                    },
                )
            })
            .collect();
        lock
    }

    fn current(tiles: &[(&str, &str)]) -> BTreeMap<String, Option<String>> {
        tiles
            .iter()
            .map(|(id, source)| (id.to_string(), Some(source.to_string())))
            .collect()
    }

    #[test]
    fn many_edited_tiles_are_summarized() {
        let names: Vec<String> = (0..6).map(|i| format!("t{i}")).collect();
        let old: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "s")).collect();
        let new: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "s-edited")).collect();
        let reasons = lock_staleness(&locked(&old, "in", "rustc 1"), &current(&new), None, None, None);
        assert_eq!(reasons, vec!["6 tiles' sources changed: `t0`, `t1`, `t2` and 3 more".to_string()]);
    }

    #[test]
    fn a_fresh_lock_has_no_staleness() {
        let lock = locked(&[("greet", "s1"), ("exclaim", "s2")], "in", "rustc 1");
        let commitment = hex::encode([1u8; 32]);
        let reasons = lock_staleness(
            &lock,
            &current(&[("greet", "s1"), ("exclaim", "s2")]),
            Some(&commitment),
            Some("in"),
            Some("rustc 1"),
        );
        assert!(reasons.is_empty(), "{reasons:?}");
        // Inputs that could not be computed are not compared.
        assert!(lock_staleness(&lock, &current(&[("greet", "s1"), ("exclaim", "s2")]), None, None, None)
            .is_empty());
    }

    #[test]
    fn each_kind_of_staleness_is_named() {
        let lock = locked(&[("greet", "s1"), ("gone", "s3")], "in", "rustc 1");
        let reasons = lock_staleness(
            &lock,
            &current(&[("greet", "s1-edited"), ("added", "s4")]),
            Some(&hex::encode([2u8; 32])),
            Some("in-edited"),
            Some("rustc 2"),
        );
        let all = reasons.join("\n");
        for expected in [
            "tile `greet`'s source changed",
            "tile `added` is new",
            "tile `gone` is in the lock but no longer in the program",
            "control flow or interface changed",
            "the guest inputs changed",
            "the lock has `rustc 1`, this machine `rustc 2`",
        ] {
            assert!(all.contains(expected), "missing `{expected}` in:\n{all}");
        }
        assert_eq!(reasons.len(), 6);
    }

    /// A format-1 lock, without `toolchain.rustc`, still parses.
    #[test]
    fn format_one_locks_still_parse() {
        let lock: RasterLock = serde_json::from_str(
            r#"{"format":1,"program_commitment":"00","tiles":{},
                "toolchain":{"risc0":"risc0-zkvm-3.0.6","build_mode":"local"}}"#,
        )
        .unwrap();
        assert_eq!(lock.toolchain.rustc, None);
        let written = serde_json::to_string(&lock_with([0u8; 32], None)).unwrap();
        assert!(!written.contains("rustc"), "an unknown compiler is omitted: {written}");
    }

    #[test]
    fn decodes_valid_image_id_hex() {
        let id = decode_image_id(&hex::encode([7u8; 32]), "greet").unwrap();
        assert_eq!(id, [7u8; 32]);
    }

    #[test]
    fn rejects_malformed_image_id_hex() {
        // Not hex at all.
        assert!(decode_image_id("nothex", "greet").is_err());
        // Valid hex, wrong length (16 bytes).
        let err = decode_image_id(&hex::encode([1u8; 16]), "greet").unwrap_err();
        assert!(err.to_string().contains("expected 32"), "got: {err}");
    }

    #[test]
    fn read_lock_reports_missing_file() {
        let err = read_lock(Path::new("/nonexistent/Raster.lock")).unwrap_err();
        assert!(
            err.to_string().contains("run `cargo raster build --backend risc0`"),
            "got: {err}"
        );
    }
}
