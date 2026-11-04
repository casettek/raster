//! Tile guests for the RISC0 zkVM.
//!
//! A tile guest is a generated crate whose `main` reads the tile's input,
//! calls the tile's replay entry from the user crate and commits the output.
//! It is built with `raster-guest-build`, from a stage where every local path
//! package is seen by a relative path, so its image id does not depend on where
//! the project, the raster checkout or `$CARGO_HOME` live
//! (docs/proposals/path-independent-builds.md).

use std::path::{Path, PathBuf};

use raster_guest_build::{
    cargo_home, path_string, prepare_stage, stage_manifest, stage_plan, Dependency, Fingerprint,
    GuestBinary, GuestBuild, GuestCrate, PathPackage, PathPackages, Scope, Stage, StagePlan,
    Toolchain, TARGET,
};

/// Everything needed to build any tile guest of one project, resolved once.
pub struct GuestBuilder {
    /// Shared by every tile guest of the project. The target directory does not
    /// reach an image id, and sharing it compiles the guest's dependencies once
    /// instead of once per tile.
    target_dir: PathBuf,
    /// The user crate: the project's package.
    user: PathPackage,
    /// The guest's `raster` dependency path, exactly as the user crate's own
    /// manifests resolve it inside the stage: a stage path, or an absolute
    /// path outside it when the project names raster absolutely. Any other
    /// path would be a second `raster` package to cargo.
    raster_dependency: String,
    plan: StagePlan,
    lockfile: Option<PathBuf>,
    /// `None` when no risc0 toolchain is installed: cached guests still load,
    /// only building fails.
    toolchain: Option<Toolchain>,
    cargo_home: PathBuf,
    fingerprint: String,
}

impl GuestBuilder {
    /// Resolve the project at `user_crate_path` with cargo: its `raster`
    /// dependency in whatever form cargo accepts (inline, table, inherited
    /// from a workspace, through a symlink), every other local path package,
    /// and the project's `Cargo.lock`.
    pub fn resolve(output_dir: &Path, user_crate_path: &Path) -> Result<Self, String> {
        let manifest = user_crate_path.join("Cargo.toml");
        let packages = PathPackages::resolve(&manifest).map_err(|e| e.to_string())?;
        // A guest needs `raster` as a local path dependency.
        packages.require("raster").map_err(|e| e.to_string())?;
        let plan = stage_plan(&packages).map_err(|e| e.to_string())?;
        if packages.lockfile.is_none() {
            eprintln!(
                "warning: `{}` has no Cargo.lock; its tile guests resolve dependency versions \
                 when they are built, so their image ids can differ between machines",
                packages.root.name
            );
        }
        let mut builder = Self {
            target_dir: output_dir.join("guest-target"),
            user: packages.root.clone(),
            raster_dependency: String::new(),
            plan,
            lockfile: packages.lockfile.clone(),
            toolchain: Toolchain::find().ok(),
            cargo_home: cargo_home().map_err(|e| e.to_string())?,
            fingerprint: String::new(),
        };
        builder.raster_dependency = builder.resolve_raster_dependency()?;
        builder.fingerprint = builder.compute_fingerprint()?;
        Ok(builder)
    }

    /// Where the user crate itself resolves `raster` inside a stage: from a
    /// probe guest that depends on the user crate alone.
    fn resolve_raster_dependency(&self) -> Result<String, String> {
        let error = |e: raster_guest_build::Error| e.to_string();
        let stage = Stage::create(self.plan.clone()).map_err(error)?;
        let probe = GuestCrate::Generated {
            package: "raster-guest-probe".into(),
            dependencies: vec![
                Dependency::version("risc0-zkvm", "3.0"),
                Dependency::path(&self.user.name, &self.user.dir).no_default_features(),
            ],
            main_rs: String::new(),
        };
        prepare_stage(&stage, &probe, self.lockfile.as_deref()).map_err(error)?;
        let staged = PathPackages::resolve_for_target(&stage.root().join("Cargo.toml"), Some(TARGET))
            .map_err(error)?;
        let raster = staged.require("raster").map_err(error)?;
        match raster.dir.strip_prefix(stage.root()) {
            Ok(relative) => path_string(relative).map_err(error),
            Err(_) => Ok(raster.dir.display().to_string()),
        }
    }

    /// A key over everything a tile guest compiles besides the tile's own
    /// source: the recipe version, the toolchain, the guest templates, the
    /// exact external crate versions, and `Cargo.toml` + library sources of
    /// every local path package the staged guest resolves. Files are named by
    /// their stage paths, so the key, like the image id, is the same at any
    /// checkout path.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Build the guest for `tile_id`.
    pub fn build_tile(&self, tile_id: &str) -> Result<GuestBinary, String> {
        let toolchain = self
            .toolchain
            .as_ref()
            .ok_or("RISC0 toolchain not found. Please install it with: rzup install rust")?;
        let stage = Stage::create(self.plan.clone()).map_err(|e| e.to_string())?;
        let output = GuestBuild {
            stage: &stage,
            krate: self.guest_crate(tile_id),
            lockfile: self.lockfile.as_deref(),
            toolchain,
            cargo_home: &self.cargo_home,
            target_dir: Some(&self.target_dir),
        }
        .run()
        .map_err(|e| e.to_string())?;
        output
            .binaries
            .into_iter()
            .next()
            .ok_or_else(|| format!("the guest for tile '{tile_id}' produced no binary"))
    }

    fn guest_crate(&self, tile_id: &str) -> GuestCrate {
        GuestCrate::Generated {
            package: format!("raster-guest-{tile_id}"),
            dependencies: vec![
                // risc0-zkvm's default features (which include `std`), matching
                // the transition guest. With `default-features = false`,
                // risc0-zkvm provides its own no_std guest `panic_impl`, which
                // collides with the `std` that transitively leaks in (e.g.
                // `digest/std` via risc0-groth16 → ark): a duplicate
                // `panic_impl` (E0152).
                Dependency::version("risc0-zkvm", "3.0"),
                Dependency::literal("raster", &self.raster_dependency).no_default_features(),
                // No default features: the user crate's `std` feature pulls in
                // the host runtime.
                Dependency::path(&self.user.name, &self.user.dir).no_default_features(),
            ],
            main_rs: self.generate_guest_main(tile_id),
        }
    }

    fn compute_fingerprint(&self) -> Result<String, String> {
        const PLACEHOLDER: &str = "__recipe__";
        let error = |e: raster_guest_build::Error| e.to_string();

        // Resolve the staged guest itself, for the guest target: exactly the
        // packages it compiles, with the features it uses.
        let stage = Stage::create(self.plan.clone()).map_err(error)?;
        let krate = self.guest_crate(PLACEHOLDER);
        prepare_stage(&stage, &krate, self.lockfile.as_deref()).map_err(error)?;
        let guest = PathPackages::resolve_for_target(&stage.root().join("Cargo.toml"), Some(TARGET))
            .map_err(error)?;

        let mut fingerprint = Fingerprint::new();
        let toolchain = self.toolchain.as_ref().map_or("none", |t| t.id.as_str());
        fingerprint.bytes("toolchain", toolchain.as_bytes());
        fingerprint.bytes(
            "manifest",
            stage_manifest(&self.plan, &krate).map_err(error)?.as_bytes(),
        );
        fingerprint.bytes("main.rs", self.generate_guest_main(PLACEHOLDER).as_bytes());
        fingerprint.bytes("external", guest.external.join("\n").as_bytes());
        let mut outside = Vec::new();
        for package in guest.packages.iter().filter(|p| p.dir != guest.root.dir) {
            // A package outside the stage is named by an absolute path in some
            // manifest; it is keyed (and compiled) by that absolute path.
            let label = match package.dir.strip_prefix(stage.root()) {
                Ok(relative) => relative.to_string_lossy().into_owned(),
                Err(_) => {
                    outside.push(package.name.as_str());
                    package.dir.to_string_lossy().into_owned()
                }
            };
            fingerprint
                .package(&label, &package.dir, Scope::Library)
                .map_err(error)?;
        }
        if !outside.is_empty() {
            eprintln!(
                "warning: `{}` reaches {} by an absolute path; its tile image ids will depend \
                 on where {}. Use a relative path for ids that do not",
                self.user.name,
                outside.join(", "),
                if outside.len() == 1 { "it lives" } else { "they live" }
            );
        }
        Ok(fingerprint.finish_hex()[..16].to_string())
    }

    /// Generate the guest crate source for a tile.
    ///
    /// The generated guest program:
    /// 1. Reads input bytes from the zkVM environment
    /// 2. Calls the tile's ABI wrapper function directly
    /// 3. Commits the serialized user output to the journal
    pub fn generate_guest_main(&self, tile_id: &str) -> String {
        // Convert tile_id to a valid Rust identifier (replace hyphens with underscores)
        let fn_name = tile_id.replace('-', "_");

        // The wrapper function name follows the pattern from the #[tile] macro
        let wrapper_name = format!("__raster_tile_replay_entry_{}", fn_name);

        // Get the crate name for the import
        let crate_name = self.user.name.replace('-', "_");

        format!(
            r##"//! Auto-generated RISC0 guest program for tile: {tile_id}
            #![no_main]
            #![no_std]

            extern crate alloc;

            use alloc::vec::Vec;

            // Import the tile's ABI wrapper from the user crate
            // The #[tile] macro generates a public wrapper function for each tile
            use {crate_name}::{wrapper_name};

            risc0_zkvm::guest::entry!(main);

            fn main() {{
                // Read input length first, then raw bytes
                let input_len: u32 = risc0_zkvm::guest::env::read();
                let mut input = alloc::vec![0u8; input_len as usize];
                risc0_zkvm::guest::env::read_slice(&mut input);

                // Call the tile's ABI wrapper (runtime failures trap here).
                let output = {wrapper_name}(&input).expect("Tile runtime failure");

                // Commit output to the journal
                risc0_zkvm::guest::env::commit_slice(&output);
            }}
            "##,
            tile_id = tile_id,
            crate_name = crate_name,
            wrapper_name = wrapper_name
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn repository() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// A copy of hello-tiles' sources in `app`, depending on `raster` as
    /// `raster_dependency` says, with no workspace inheritance of its own.
    fn app(app: &Path, raster_dependency: &str) {
        let hello = repository().join("examples/hello-tiles");
        let manifest: Vec<String> = fs::read_to_string(hello.join("Cargo.toml"))
            .unwrap()
            .lines()
            .map(|line| match line.split([' ', '.']).next().unwrap_or("") {
                "raster" => raster_dependency.to_string(),
                "serde" => r#"serde = { version = "1.0", default-features = false, features = ["derive", "alloc"] }"#.into(),
                "serde_json" => r#"serde_json = { version = "1.0", optional = true }"#.into(),
                _ => line.to_string(),
            })
            .collect();
        write(&app.join("Cargo.toml"), &manifest.join("\n"));
        for dir in ["src", "bin"] {
            for entry in fs::read_dir(hello.join(dir)).unwrap() {
                let entry = entry.unwrap();
                fs::create_dir_all(app.join(dir)).unwrap();
                fs::copy(entry.path(), app.join(dir).join(entry.file_name())).unwrap();
            }
        }
        // Same versions as this repository, without a network resolve.
        fs::copy(
            repository().join("Cargo.lock"),
            app.parent().unwrap().join("Cargo.lock"),
        )
        .unwrap();
    }

    /// build-path-dependence §1: `raster = { workspace = true }`, with the path
    /// in the workspace root's manifest, reached through a symlinked sibling
    /// checkout. The old line scan found no path and fell back to
    /// `/crates/raster`; the symlink then made cargo see two `raster` packages.
    #[cfg(unix)]
    #[test]
    fn an_inherited_raster_through_a_symlink_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(repository(), tmp.path().join("raster")).unwrap();
        let workspace = tmp.path().join("ws");
        write(
            &workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n\n\
             [workspace.dependencies]\n\
             raster = { path = \"../raster/crates/raster\", default-features = false }\n",
        );
        app(
            &workspace.join("app"),
            "raster = { workspace = true, default-features = false }",
        );

        let builder = GuestBuilder::resolve(&tmp.path().join("out"), &workspace.join("app"))
            .unwrap_or_else(|e| panic!("{e}"));
        // As the project's own manifest reaches it, through the symlinked
        // sibling: a stage path, not the canonical checkout.
        assert_eq!(builder.raster_dependency, "raster/crates/raster");
        assert_eq!(builder.user.name, "hello-tiles");
        let names: Vec<&str> = builder.plan.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["project", "raster"]);
        assert_eq!(builder.fingerprint().len(), 16);
    }

    /// A sibling project whose raster checkout is not named `raster`: the
    /// stage still names it `raster`, and keeps the project's
    /// `../raster-main/...` valid by rewriting the manifest inside the stage,
    /// so raster has one path. (Staging it as `raster` without that rewrite
    /// left the path dangling and failed the resolve.)
    #[cfg(unix)]
    #[test]
    fn a_renamed_sibling_checkout_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(repository(), tmp.path().join("raster-main")).unwrap();
        let project = tmp.path().join("app");
        app(
            &project,
            r#"raster = { path = "../raster-main/crates/raster", default-features = false }"#,
        );
        write(&project.join("Cargo.toml"), &format!(
            "{}\n[workspace]\n",
            fs::read_to_string(project.join("Cargo.toml")).unwrap()
        ));
        fs::rename(tmp.path().join("Cargo.lock"), project.join("Cargo.lock")).unwrap();

        let builder = GuestBuilder::resolve(&tmp.path().join("out"), &project)
            .unwrap_or_else(|e| panic!("{e}"));
        let names: Vec<&str> = builder.plan.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["project", "raster"], "no folder name reaches the stage");
        assert_eq!(builder.raster_dependency, "raster/crates/raster");
    }

    /// A standalone sibling project beside a `raster` checkout, in `folder`.
    #[cfg(unix)]
    fn sibling(root: &Path, folder: &str) -> PathBuf {
        std::os::unix::fs::symlink(repository(), root.join("raster")).unwrap();
        let project = root.join(folder);
        app(
            &project,
            r#"raster = { path = "../raster/crates/raster", default-features = false }"#,
        );
        write(&project.join("Cargo.toml"), &format!(
            "{}\n[workspace]\n",
            fs::read_to_string(project.join("Cargo.toml")).unwrap()
        ));
        fs::rename(root.join("Cargo.lock"), project.join("Cargo.lock")).unwrap();
        project
    }

    /// The project's folder may be called anything: it is staged as
    /// `project`, so the cache key (like the image id) does not change.
    #[cfg(unix)]
    #[test]
    fn the_project_folder_name_stays_out() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let key = |root: &Path, folder: &str| {
            let builder = GuestBuilder::resolve(&root.join("out"), &sibling(root, folder))
                .unwrap_or_else(|e| panic!("{e}"));
            let names: Vec<String> = builder.plan.entries().iter().map(|e| e.name.clone()).collect();
            assert_eq!(names, vec!["project", "raster"]);
            builder.fingerprint().to_string()
        };
        assert_eq!(key(a.path(), "iris"), key(b.path(), "iris-main"));
    }

    /// A project naming raster by an absolute path. Cargo resolves that path to
    /// the real checkout, so the guest must name the same one: a stage path
    /// alongside it would be a second `raster` package.
    #[cfg(unix)]
    #[test]
    fn an_absolute_raster_path_resolves_without_a_second_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let checkout = tmp.path().join("elsewhere/raster");
        fs::create_dir_all(checkout.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(repository(), &checkout).unwrap();
        let project = tmp.path().join("app");
        app(
            &project,
            &format!(
                r#"raster = {{ path = "{}", default-features = false }}"#,
                checkout.join("crates/raster").display()
            ),
        );
        write(&project.join("Cargo.toml"), &format!(
            "{}\n[workspace]\n",
            fs::read_to_string(project.join("Cargo.toml")).unwrap()
        ));
        fs::rename(tmp.path().join("Cargo.lock"), project.join("Cargo.lock")).unwrap();

        let builder = GuestBuilder::resolve(&tmp.path().join("out"), &project)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            builder.raster_dependency,
            checkout.join("crates/raster").display().to_string(),
            "the guest names raster exactly as the project does"
        );
    }

    #[test]
    fn a_project_without_raster_is_an_error_naming_it() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("plain");
        write(
            &project.join("Cargo.toml"),
            "[package]\nname = \"plain\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n",
        );
        write(&project.join("src/lib.rs"), "");
        let error = GuestBuilder::resolve(&tmp.path().join("out"), &project)
            .err()
            .expect("no raster dependency");
        assert!(error.contains("no `raster` path package"), "{error}");
        assert!(error.contains("`plain`"), "{error}");
    }

    /// The cache key is the same for the same sources at two checkout paths.
    #[cfg(unix)]
    #[test]
    fn the_fingerprint_does_not_depend_on_the_checkout_path() {
        let tmp = tempfile::tempdir().unwrap();
        let key = |at: &Path| {
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(repository(), at).unwrap();
            GuestBuilder::resolve(&tmp.path().join("out"), &at.join("examples/hello-tiles"))
                .unwrap_or_else(|e| panic!("{e}"))
                .fingerprint()
                .to_string()
        };
        assert_eq!(
            key(&tmp.path().join("a/raster")),
            key(&tmp.path().join("b/deeper/defender-raster"))
        );
    }
}
