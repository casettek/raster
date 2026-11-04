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
pub use stage::{Stage, StageEntry, StagePlan, PROJECT_ENTRY, RASTER_ENTRY};

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
/// The entry holding the raster workspace is staged as [`RASTER_ENTRY`] and the
/// one holding the project as [`PROJECT_ENTRY`], so no folder name reaches an
/// image id. A tile
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
    // No folder name reaches an image id: the raster checkout is `raster`,
    // the project's folder `project`, and any other folder of local crates is
    // named after the first crate it holds, by name and version. The version
    // matters: cargo allows two local crates of one name in a graph when their
    // versions differ (`foo` 1 in `foo-v1`, `foo` 2 in `foo-v2`), while a name
    // and version pair is unique among them. A version bump changes the
    // guest's ids anyway (cargo builds the version in), so this costs nothing.
    let project_dir = &packages.root.dir;
    StagePlan::new(&packages.workspace_roots(), raster_root)?
        .with_manifests(manifests)
        .with_entry_names(|target| {
            if raster_root.is_some_and(|root| root.starts_with(target)) {
                None
            } else if project_dir.starts_with(target) {
                Some(PROJECT_ENTRY.to_string())
            } else {
                packages
                    .packages
                    .iter()
                    .filter(|p| p.dir.starts_with(target))
                    .map(|p| (p.name.as_str(), p.version.as_str()))
                    .min()
                    .map(|(name, version)| format!("dep-{name}-{version}"))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn package(name: &str, version: &str, dir: &str, workspace_root: &str) -> PathPackage {
        PathPackage {
            name: name.into(),
            version: version.into(),
            dir: PathBuf::from(dir),
            workspace_root: PathBuf::from(workspace_root),
        }
    }

    /// Two versions of one local crate in sibling folders keep separate stage
    /// entries: named by crate and version, not by crate alone.
    #[test]
    fn two_versions_of_a_local_crate_get_separate_entries() {
        let app = package("app", "0.1.0", "/p/app", "/p/app");
        let packages = PathPackages {
            root: app.clone(),
            packages: vec![
                app,
                package("foo", "1.0.0", "/p/foo-v1", "/p/foo-v1"),
                package("foo", "2.0.0", "/p/foo-v2", "/p/foo-v2"),
                package("raster", "0.1.0", "/p/raster-main/crates/raster", "/p/raster-main"),
            ],
            lockfile: None,
            external: Vec::new(),
        };
        let plan = stage_plan(&packages).unwrap_or_else(|e| panic!("{e}"));
        let names: Vec<(&str, &Path)> = plan
            .entries()
            .iter()
            .map(|e| (e.name.as_str(), e.target.as_path()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("dep-foo-1.0.0", Path::new("/p/foo-v1")),
                ("dep-foo-2.0.0", Path::new("/p/foo-v2")),
                ("project", Path::new("/p/app")),
                ("raster", Path::new("/p/raster-main")),
            ]
        );
    }
}
