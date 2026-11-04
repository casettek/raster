//! Path-independent risc0 guest builds, for tile guests and protocol guests.
//!
//! A guest's image id is a hash of its loaded memory. Built the obvious way, it
//! depends on where the sources live (see
//! `docs/proposals/path-independent-builds.md`):
//!
//! - panic locations store source paths, and cargo gives rustc an **absolute**
//!   path for every crate outside the building workspace;
//! - cargo's `-C metadata` for such a path dependency hashes its absolute path,
//!   and rustc orders code by it, so even with the strings remapped a different
//!   checkout path moves the code.
//!
//! This crate builds a guest from a [`Stage`]: a temp directory that is the
//! building workspace's root and sits above every workspace the guest's path
//! packages belong to. cargo then sees each of them by a relative path, for
//! both the source paths it passes to rustc and the `-C metadata` hash, and
//! only `$CARGO_HOME` needs a `--remap-path-prefix`. Measured in the proposal
//! §3: two checkouts at different paths, with different `$CARGO_HOME`s, give
//! identical loaded segments.
//!
//! The pieces, in the order a build uses them:
//! 1. [`PathPackages::resolve`] asks cargo for the guest's local path packages.
//! 2. [`stage_plan`] decides the stage layout; [`Stage::create`] makes it.
//! 3. [`Toolchain::find`] picks the risc0 Rust toolchain.
//! 4. [`GuestBuild::run`] writes the stage manifest, builds and packages.
//!
//! [`Fingerprint`] keys a build cache on everything a guest compiles, by
//! stage-relative names, so the key is path-independent too.

mod fingerprint;
mod guest;
mod manifest;
mod packages;
mod recipe;
mod stage;

pub use fingerprint::{Fingerprint, Scope};
pub use guest::{
    path_string, prepare_stage, stage_manifest, BuildOutput, Dependency, GuestBinary, GuestBuild,
    GuestCrate,
};
pub use packages::{PathPackage, PathPackages};
pub use recipe::{
    cargo_home, encode_rustflags, rustflags, Toolchain, ToolchainInputs, CARGO_HOME_REMAP,
    RECIPE_VERSION, TARGET,
};
pub use stage::{Stage, StageEntry, StagePlan, RASTER_ENTRY};

/// Errors from resolving, staging or building a guest.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// `cargo metadata` failed, or its output was not what a guest needs.
    #[error("cargo metadata: {0}")]
    Metadata(String),

    /// The path packages cannot be arranged into a stage.
    #[error("stage layout: {0}")]
    Layout(String),

    /// No usable risc0 Rust toolchain.
    #[error("risc0 toolchain: {0}")]
    Toolchain(String),

    /// The guest's `cargo build` failed; carries its stderr.
    #[error("guest build failed: {0}")]
    Build(String),

    /// Packaging the ELF or computing its image id failed.
    #[error("guest binary: {0}")]
    Binary(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// The stage layout for a guest whose path packages are `packages`.
///
/// The entry holding the raster workspace is staged as [`RASTER_ENTRY`], so the
/// raster checkout's own directory name never reaches an image id. A tile
/// guest finds that workspace through its `raster` dependency; a protocol
/// guest, which links only `raster-core`, through that.
pub fn stage_plan(packages: &PathPackages) -> Result<StagePlan> {
    let raster_root = ["raster", "raster-core"]
        .iter()
        .find_map(|name| packages.get(name))
        .map(|package| package.workspace_root.as_path());
    // Every manifest the guest's resolve reads: the path packages' own, and
    // their workspace roots' (where `workspace = true` dependencies point).
    let manifests = packages
        .packages
        .iter()
        .flat_map(|p| [p.dir.join("Cargo.toml"), p.workspace_root.join("Cargo.toml")])
        .filter(|m| m.is_file())
        .collect();
    Ok(StagePlan::new(&packages.workspace_roots(), raster_root)?.with_manifests(manifests))
}
