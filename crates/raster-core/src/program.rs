//! Program identity: the static definition of a raster program and its
//! binding commitment.
//!
//! See `docs/proposals/program-identity.md`. A raster program's identity is a
//! commitment over three things that are the same on every run:
//!
//! - the **interface** it declares (`ProgramManifest`, authored in `Raster.toml`),
//! - its **control flow** (`ControlFlowSchema`, derived from source), and
//! - its **code** (`tiles`: the risc0 image id of every tile's replay guest).
//!
//! These are bundled into a [`ProgramDefinition`] whose canonical byte form is
//! the `program.bin` artifact. `program_commitment = sha256(domain-prefix ||
//! canonical_bytes)` — the identity preimage and the transition guest's
//! verification frame are the same bytes, so "the program a receipt is about"
//! and "the program a hash names" cannot diverge.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cfs::{ControlFlowSchema, SequenceDef, TileId};
use crate::input::ExternalEncoding;
use crate::{Error, Result};

/// Domain-separation prefix for the program-identity hash. Versioning the
/// construction here keeps the inner CFS `version` field free of that duty and
/// keeps a program commitment from ever colliding with a (bare-`sha256`)
/// manifest commitment.
pub const PROGRAM_COMMITMENT_DOMAIN: &[u8] = b"raster/program/v1";

/// A risc0 image id (32-byte digest) of a tile's replay guest.
pub type ImageId = [u8; 32];

/// One declared program input (a `main` entry argument), from `[inputs.<name>]`
/// in `Raster.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InterfaceDecl {
    /// The Rust type path of the value, e.g. `"PersonalData"` or `"u64"`.
    #[serde(rename = "type")]
    pub type_path: String,
    /// The external encoding the value is carried in.
    #[serde(default)]
    pub encoding: ExternalEncoding,
}

/// The authored program interface (from `Raster.toml`). Part of the program's
/// identity: renaming or version-bumping is an intentional identity change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProgramManifest {
    pub name: String,
    pub version: String,
    /// Declared inputs, keyed by parameter name. `BTreeMap` for a canonical
    /// (name-sorted) byte form; the true parameter order lives in the CFS's
    /// `entry_arguments`.
    #[serde(default)]
    pub inputs: BTreeMap<String, InterfaceDecl>,
    /// The declared output, present iff `main` returns a non-unit value.
    #[serde(default)]
    pub output: Option<InterfaceDecl>,
}

/// The complete static definition of a raster program. Its `canonical_bytes`
/// are the `program.bin` artifact; its `commitment` is the program identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramDefinition {
    pub manifest: ProgramManifest,
    pub cfs: ControlFlowSchema,
    /// Tile code identity: image id per tile. Key set must equal the CFS's
    /// tile ids.
    pub tiles: BTreeMap<TileId, ImageId>,
}

impl ProgramDefinition {
    /// Assemble a definition from its three parts, validating the invariants
    /// that make the resulting commitment meaningful:
    ///
    /// 1. the tile registry names exactly the CFS's tiles (no missing, no extra);
    /// 2. the CFS is canonical (tiles and sequences sorted by id, ids unique);
    /// 3. the manifest's declared inputs match `main`'s entry arguments; and
    /// 4. the manifest declares an output iff `main` produces one.
    pub fn assemble(
        manifest: ProgramManifest,
        cfs: ControlFlowSchema,
        tiles: BTreeMap<TileId, ImageId>,
    ) -> Result<Self> {
        validate_registry_matches_cfs(&cfs, &tiles)?;
        validate_cfs_canonical(&cfs)?;
        validate_manifest_matches_cfs(&manifest, &cfs)?;
        Ok(Self {
            manifest,
            cfs,
            tiles,
        })
    }

    /// The canonical byte form — the `program.bin` content and the transition
    /// guest's verification frame.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("ProgramDefinition is always serializable")
    }

    /// `program_commitment = sha256(domain-prefix || canonical_bytes)`.
    pub fn commitment(&self) -> [u8; 32] {
        commitment_of_bytes(&self.canonical_bytes())
    }

    /// Decode a definition from its canonical bytes (guest and file loaders).
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        postcard::from_bytes(bytes)
            .map_err(|e| Error::Other(format!("failed to decode ProgramDefinition: {e}")))
    }

    /// The image id the CFS declares for `tile_id`, if any.
    pub fn tile_image_id(&self, tile_id: &str) -> Option<&ImageId> {
        self.tiles.get(tile_id)
    }
}

/// Compute `program_commitment` over a raw `program.bin` frame without
/// re-serializing — what the transition guest does with the bytes it receives,
/// so its committed identity is derived from the exact bytes it verifies
/// against. Equals `ProgramDefinition::commitment` for `def.canonical_bytes()`.
pub fn commitment_of_bytes(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(PROGRAM_COMMITMENT_DOMAIN);
    hasher.update(bytes);
    hasher.finalize().into()
}

fn main_sequence<'a>(cfs: &'a ControlFlowSchema) -> Option<&'a SequenceDef> {
    cfs.sequences.iter().find(|s| s.id == "main")
}

fn validate_registry_matches_cfs(
    cfs: &ControlFlowSchema,
    tiles: &BTreeMap<TileId, ImageId>,
) -> Result<()> {
    for tile in &cfs.tiles {
        if !tiles.contains_key(&tile.id) {
            return Err(Error::Other(format!(
                "tile '{}' has no image id in the program registry",
                tile.id
            )));
        }
    }
    for id in tiles.keys() {
        if !cfs.tiles.iter().any(|t| &t.id == id) {
            return Err(Error::Other(format!(
                "program registry names tile '{id}' that the CFS does not declare"
            )));
        }
    }
    Ok(())
}

fn validate_cfs_canonical(cfs: &ControlFlowSchema) -> Result<()> {
    if !is_sorted_unique(cfs.tiles.iter().map(|t| t.id.as_str())) {
        return Err(Error::Other(
            "CFS tiles are not canonically sorted with unique ids".into(),
        ));
    }
    if !is_sorted_unique(cfs.sequences.iter().map(|s| s.id.as_str())) {
        return Err(Error::Other(
            "CFS sequences are not canonically sorted with unique ids".into(),
        ));
    }
    Ok(())
}

fn validate_manifest_matches_cfs(manifest: &ProgramManifest, cfs: &ControlFlowSchema) -> Result<()> {
    let (entry_arguments, produces_output) = match main_sequence(cfs) {
        Some(main) => (main.entry_arguments.as_slice(), main.produces_output),
        // No `main` (e.g. a library CFS): the interface contract is vacuous.
        None => (&[][..], false),
    };

    // Declared input names must match main's entry arguments as a set — the
    // canonical order lives in the CFS, and TOML tables are unordered.
    for name in entry_arguments {
        if !manifest.inputs.contains_key(name) {
            return Err(Error::Other(format!(
                "Raster.toml declares no input '{name}', but main takes it as an entry argument"
            )));
        }
    }
    for name in manifest.inputs.keys() {
        if !entry_arguments.iter().any(|a| a == name) {
            return Err(Error::Other(format!(
                "Raster.toml declares input '{name}' that main does not take"
            )));
        }
    }

    if produces_output && manifest.output.is_none() {
        return Err(Error::Other(
            "main returns a value but Raster.toml declares no [output]".into(),
        ));
    }
    if !produces_output && manifest.output.is_some() {
        return Err(Error::Other(
            "Raster.toml declares an [output] but main returns unit".into(),
        ));
    }
    Ok(())
}

fn is_sorted_unique<'a>(mut ids: impl Iterator<Item = &'a str>) -> bool {
    let mut prev: Option<&str> = None;
    for id in ids.by_ref() {
        if let Some(p) = prev {
            if p >= id {
                return false;
            }
        }
        prev = Some(id);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfs::{SequenceChildItem, SequenceDef, TileDef, TileItem};
    use alloc::string::ToString;
    use alloc::vec;

    fn image(n: u8) -> ImageId {
        [n; 32]
    }

    fn cfs_with(tiles: Vec<TileDef>, main: SequenceDef) -> ControlFlowSchema {
        ControlFlowSchema {
            version: "1.0".to_string(),
            project: "test".to_string(),
            encoding: "postcard".to_string(),
            tiles,
            sequences: vec![main],
        }
    }

    fn main_seq(entry_arguments: Vec<String>, produces_output: bool) -> SequenceDef {
        SequenceDef {
            id: "main".to_string(),
            input_sources: vec![],
            items: vec![SequenceChildItem::Tile(TileItem {
                id: "greet".to_string(),
                sources: vec![],
            })],
            entry_arguments,
            produces_output,
        }
    }

    fn manifest(inputs: Vec<(&str, &str)>, output: Option<&str>) -> ProgramManifest {
        ProgramManifest {
            name: "test".to_string(),
            version: "0.1.0".to_string(),
            inputs: inputs
                .into_iter()
                .map(|(name, ty)| {
                    (
                        name.to_string(),
                        InterfaceDecl {
                            type_path: ty.to_string(),
                            encoding: ExternalEncoding::Raster,
                        },
                    )
                })
                .collect(),
            output: output.map(|ty| InterfaceDecl {
                type_path: ty.to_string(),
                encoding: ExternalEncoding::Raster,
            }),
        }
    }

    fn registry(ids: &[&str]) -> BTreeMap<TileId, ImageId> {
        ids.iter()
            .enumerate()
            .map(|(i, id)| (id.to_string(), image(i as u8)))
            .collect()
    }

    #[test]
    fn assemble_accepts_matching_parts() {
        let cfs = cfs_with(
            vec![TileDef::iter("greet", 1, 1)],
            main_seq(vec!["personal_data".to_string()], true),
        );
        let def = ProgramDefinition::assemble(
            manifest(vec![("personal_data", "PersonalData")], Some("String")),
            cfs,
            registry(&["greet"]),
        )
        .expect("parts match");
        // Commitment is deterministic and domain-prefixed.
        assert_eq!(def.commitment(), commitment_of_bytes(&def.canonical_bytes()));
        assert_ne!(def.commitment(), [0u8; 32]);
    }

    #[test]
    fn round_trips_through_canonical_bytes() {
        let cfs = cfs_with(
            vec![TileDef::iter("greet", 1, 1)],
            main_seq(vec!["personal_data".to_string()], false),
        );
        let def = ProgramDefinition::assemble(
            manifest(vec![("personal_data", "PersonalData")], None),
            cfs,
            registry(&["greet"]),
        )
        .unwrap();
        let bytes = def.canonical_bytes();
        let decoded = ProgramDefinition::decode(&bytes).unwrap();
        assert_eq!(decoded.canonical_bytes(), bytes);
        assert_eq!(decoded.commitment(), def.commitment());
    }

    #[test]
    fn registry_must_cover_exactly_the_cfs_tiles() {
        let cfs = cfs_with(
            vec![TileDef::iter("greet", 1, 1)],
            main_seq(vec![], false),
        );
        // Missing image id.
        assert!(ProgramDefinition::assemble(manifest(vec![], None), cfs.clone(), registry(&[])).is_err());
        // Extra image id.
        assert!(
            ProgramDefinition::assemble(manifest(vec![], None), cfs, registry(&["greet", "ghost"]))
                .is_err()
        );
    }

    #[test]
    fn manifest_interface_must_match_main() {
        let cfs = cfs_with(
            vec![TileDef::iter("greet", 1, 1)],
            main_seq(vec!["seed".to_string()], false),
        );
        // Missing declared input.
        assert!(ProgramDefinition::assemble(manifest(vec![], None), cfs.clone(), registry(&["greet"])).is_err());
        // Output declared but main returns unit.
        assert!(ProgramDefinition::assemble(
            manifest(vec![("seed", "u64")], Some("String")),
            cfs,
            registry(&["greet"])
        )
        .is_err());
    }

    #[test]
    fn rejects_non_canonical_cfs() {
        // Tiles out of order.
        let cfs = cfs_with(
            vec![TileDef::iter("zeta", 1, 1), TileDef::iter("alpha", 1, 1)],
            main_seq(vec![], false),
        );
        assert!(
            ProgramDefinition::assemble(manifest(vec![], None), cfs, registry(&["zeta", "alpha"]))
                .is_err()
        );
    }
}
