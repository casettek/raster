//! Writing a stage's manifest, building the guest, packaging its binaries.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cargo_metadata::{MetadataCommand, TargetKind};

use crate::recipe::{encode_rustflags, rustflags, Toolchain, TARGET};
use crate::stage::{Stage, StagePlan};
use crate::{Error, Result};

/// A dependency of a generated guest crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    name: String,
    source: Source,
    default_features: bool,
    features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    Version(String),
    /// A real directory; written as its stage-relative path.
    Path(PathBuf),
    /// A path written exactly as given.
    Literal(String),
}

impl Dependency {
    /// A registry dependency, e.g. `("risc0-zkvm", "3.0")`.
    pub fn version(name: &str, requirement: &str) -> Self {
        Self::new(name, Source::Version(requirement.into()))
    }

    /// A path dependency on the package in the real directory `dir`.
    pub fn path(name: &str, dir: &Path) -> Self {
        Self::new(name, Source::Path(dir.to_path_buf()))
    }

    /// A path dependency written exactly as `path`: a stage-relative path, or
    /// an absolute one outside the stage. For naming a package exactly as
    /// another staged package's manifest resolves it, so cargo sees one
    /// package and not two.
    pub fn literal(name: &str, path: &str) -> Self {
        Self::new(name, Source::Literal(path.into()))
    }

    pub fn no_default_features(mut self) -> Self {
        self.default_features = false;
        self
    }

    pub fn features(mut self, features: &[&str]) -> Self {
        self.features = features.iter().map(|f| f.to_string()).collect();
        self
    }

    fn new(name: &str, source: Source) -> Self {
        Self {
            name: name.into(),
            source,
            default_features: true,
            features: Vec::new(),
        }
    }
}

/// What the stage builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestCrate {
    /// A crate generated at the stage root: a tile guest's wrapper.
    Generated {
        package: String,
        dependencies: Vec<Dependency>,
        main_rs: String,
    },
    /// An existing guest package in the real directory `dir`, rehomed at the
    /// stage root: a protocol guest. The stage root's manifest is the guest's
    /// own, with every `path` dependency rewritten to its stage path and the
    /// guest's `[workspace]` replaced by the stage's. Every other top-level
    /// entry of `dir` (`src/`, ...) is linked beside it, so the guest's sources
    /// are seen as `src/...` and its place in the checkout never reaches the
    /// image id.
    ///
    /// (Making the guest a member of the stage workspace instead would need it
    /// to drop its own `[workspace]`, and cargo then assigns it to the
    /// enclosing repository workspace: a root `exclude` is ignored for a
    /// package nested inside another member's directory, measured 2026-11-03.)
    Package { dir: PathBuf },
}

/// The guest manifest's top-level entries that are not linked into the
/// stage: the stage writes its own manifest and lockfile, and builds into its
/// own target directory.
const NOT_LINKED: [&str; 4] = ["Cargo.toml", "Cargo.lock", "target", ".git"];

/// The stage root's `Cargo.toml` for `krate`.
pub fn stage_manifest(plan: &StagePlan, krate: &GuestCrate) -> Result<String> {
    let entries: toml::Value = plan
        .entries()
        .iter()
        .map(|e| toml::Value::String(e.name.clone()))
        .collect::<Vec<_>>()
        .into();
    let mut workspace = toml::Table::new();
    workspace.insert("resolver".into(), "2".into());

    let mut manifest = toml::Table::new();
    match krate {
        GuestCrate::Generated {
            package,
            dependencies,
            ..
        } => {
            let mut table = toml::Table::new();
            table.insert("name".into(), package.clone().into());
            table.insert("version".into(), "0.1.0".into());
            table.insert("edition".into(), "2021".into());
            manifest.insert("package".into(), table.into());

            let mut deps = toml::Table::new();
            for dependency in dependencies {
                let mut dep = toml::Table::new();
                match &dependency.source {
                    Source::Version(requirement) => {
                        dep.insert("version".into(), requirement.clone().into());
                    }
                    Source::Path(dir) => {
                        dep.insert("path".into(), path_string(&plan.stage_relative(dir)?)?.into());
                    }
                    Source::Literal(path) => {
                        dep.insert("path".into(), path.clone().into());
                    }
                }
                if !dependency.default_features {
                    dep.insert("default-features".into(), false.into());
                }
                if !dependency.features.is_empty() {
                    dep.insert(
                        "features".into(),
                        dependency
                            .features
                            .iter()
                            .map(|f| toml::Value::String(f.clone()))
                            .collect::<Vec<_>>()
                            .into(),
                    );
                }
                deps.insert(dependency.name.clone(), dep.into());
            }
            manifest.insert("dependencies".into(), deps.into());

            let mut release = toml::Table::new();
            release.insert("opt-level".into(), 3.into());
            release.insert("lto".into(), true.into());
            let mut profile = toml::Table::new();
            profile.insert("release".into(), release.into());
            manifest.insert("profile".into(), profile.into());
        }
        GuestCrate::Package { dir } => {
            let dir = &absolute(dir)?;
            let guest_manifest = dir.join("Cargo.toml");
            manifest = fs::read_to_string(&guest_manifest)?
                .parse()
                .map_err(|e| Error::Layout(format!("{}: {e}", guest_manifest.display())))?;
            if !manifest.contains_key("package") {
                return Err(Error::Layout(format!(
                    "{} is not a package manifest",
                    guest_manifest.display()
                )));
            }
            // Rehomed at the stage root: its paths become stage paths.
            crate::manifest::restage_paths(&mut manifest, dir, Path::new(""), plan, true)?;
            // The guest's own `[workspace]`, if any, is replaced by the stage's.
            manifest.remove("workspace");
        }
    }
    workspace.insert("exclude".into(), entries);
    manifest.insert("workspace".into(), workspace.into());
    toml::to_string(&manifest).map_err(|e| Error::Layout(format!("stage manifest: {e}")))
}

/// `path` made absolute against the current directory, lexically: symlinks
/// are kept, as cargo keeps them.
fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(crate::stage::normalize(&std::path::absolute(path)?))
}

/// Forward slashes on every platform: cargo accepts them, and the manifest,
/// which a fingerprint may hash, stays the same everywhere.
pub fn path_string(path: &Path) -> Result<String> {
    let parts: Option<Vec<&str>> = path.components().map(|c| c.as_os_str().to_str()).collect();
    parts
        .map(|parts| parts.join("/"))
        .ok_or_else(|| Error::Layout(format!("{} is not valid UTF-8", path.display())))
}

/// One built guest binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestBinary {
    /// The cargo binary target's name.
    pub name: String,
    /// The user ELF, as cargo produced it.
    pub elf: Vec<u8>,
    /// The encoded `ProgramBinary`: the user ELF with the v1compat kernel.
    /// This is what the prover loads and what the image id hashes.
    pub program: Vec<u8>,
    pub image_id: [u8; 32],
    /// The image id as risc0's `[u32; 8]` digest words.
    pub image_id_words: [u32; 8],
}

impl GuestBinary {
    /// Package a user ELF with the v1compat kernel and compute its image id.
    pub fn package(name: &str, elf: Vec<u8>) -> Result<Self> {
        let program =
            risc0_binfmt::ProgramBinary::new(&elf, risc0_zkos_v1compat::V1COMPAT_ELF).encode();
        let digest = risc0_binfmt::compute_image_id(&program)
            .map_err(|e| Error::Binary(format!("{name}: {e}")))?;
        let image_id: [u8; 32] = digest
            .as_bytes()
            .try_into()
            .map_err(|_| Error::Binary(format!("{name}: digest is not 32 bytes")))?;
        let image_id_words: [u32; 8] = digest
            .as_words()
            .try_into()
            .map_err(|_| Error::Binary(format!("{name}: digest is not 8 words")))?;
        Ok(Self {
            name: name.into(),
            elf,
            program,
            image_id,
            image_id_words,
        })
    }

    pub fn image_id_hex(&self) -> String {
        hex::encode(self.image_id)
    }
}

/// The result of a build.
#[derive(Debug, Clone)]
pub struct BuildOutput {
    /// One per binary target of the guest package, sorted by name.
    pub binaries: Vec<GuestBinary>,
    /// The stage's `Cargo.lock` after the build.
    pub lockfile: Vec<u8>,
}

/// One guest build in a [`Stage`].
pub struct GuestBuild<'a> {
    pub stage: &'a Stage,
    pub krate: GuestCrate,
    /// Seeds the stage's `Cargo.lock`, so the guest resolves the versions this
    /// lockfile pins instead of whatever is newest when it builds.
    pub lockfile: Option<&'a Path>,
    pub toolchain: &'a Toolchain,
    /// Must be the value from [`crate::cargo_home`]; the build runs with it.
    pub cargo_home: &'a Path,
    /// Defaults to `<stage>/target`. The target directory does not reach the
    /// image id, so guests may share one. A relative path is taken against the
    /// caller's working directory (see [`Self::target_dir`]).
    pub target_dir: Option<&'a Path>,
}

/// Write `krate` into `stage`: its manifest, a generated crate's `main.rs`,
/// and `lockfile` as the stage's `Cargo.lock`. [`GuestBuild::run`] does this
/// first; a caller can also do it alone to resolve the staged guest's
/// dependencies with [`crate::PathPackages::resolve_for_target`].
pub fn prepare_stage(stage: &Stage, krate: &GuestCrate, lockfile: Option<&Path>) -> Result<()> {
    let root = stage.root();
    fs::write(root.join("Cargo.toml"), stage_manifest(stage.plan(), krate)?)?;
    match krate {
        GuestCrate::Generated { main_rs, .. } => {
            fs::create_dir_all(root.join("src"))?;
            fs::write(root.join("src").join("main.rs"), main_rs)?;
        }
        GuestCrate::Package { dir } => {
            let dir = &absolute(dir)?;
            for entry in fs::read_dir(dir)? {
                let name = entry?.file_name();
                if NOT_LINKED.iter().any(|n| name == *n) {
                    continue;
                }
                let at = root.join(&name);
                if at.exists() {
                    return Err(Error::Layout(format!(
                        "the guest's `{}` collides with a stage entry",
                        name.to_string_lossy()
                    )));
                }
                crate::stage::link(&dir.join(&name), &at)?;
            }
        }
    }
    if let Some(lockfile) = lockfile {
        fs::copy(lockfile, root.join("Cargo.lock"))?;
    }
    Ok(())
}

impl GuestBuild<'_> {
    /// The target directory, absolute. cargo runs in the stage root, while the
    /// ELF is read back from this process: a relative path would name a
    /// directory under the stage for cargo and one under the caller's working
    /// directory for the read, so the build would find no ELF, or an old one.
    fn target_dir(&self) -> Result<PathBuf> {
        match self.target_dir {
            Some(dir) => absolute(dir),
            None => Ok(self.stage.root().join("target")),
        }
    }

    pub fn run(&self) -> Result<BuildOutput> {
        let root = self.stage.root();
        prepare_stage(self.stage, &self.krate, self.lockfile)?;
        let lock_path = root.join("Cargo.lock");
        let target_dir = self.target_dir()?;

        let mut command = Command::new(self.toolchain.cargo());
        // Called from a build script, the outer cargo's CARGO_* variables (its
        // target dir, manifest dir, encoded rustflags...) must not leak in.
        for (key, _) in env::vars_os() {
            if key.to_string_lossy().starts_with("CARGO") {
                command.env_remove(&key);
            }
        }
        // A rustc wrapper from the outer build is for the host toolchain: under
        // `cargo clippy`, RUSTC_WORKSPACE_WRAPPER is clippy-driver, which then
        // compiles the guest with a sysroot that has no zkVM `std`. A caching
        // wrapper (sccache) is dropped too: guest output must come from rustc.
        command
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER")
            .env_remove("RUSTUP_TOOLCHAIN")
            .current_dir(root)
            .env("CARGO_HOME", self.cargo_home)
            .env("RUSTC", self.toolchain.rustc())
            .env(
                "CARGO_ENCODED_RUSTFLAGS",
                encode_rustflags(&rustflags(self.cargo_home)),
            )
            .env("CFLAGS_riscv32im_risc0_zkvm_elf", "-march=rv32im -nostdlib")
            // Lets crypto patches use the bigint2 accelerator.
            .env("RISC0_FEATURE_bigint2", "");
        // Target-scoped, unlike risc0-build's global CC, so host build scripts
        // keep the host compiler.
        if let Some(cc) = Toolchain::cc() {
            command.env("CC_riscv32im_risc0_zkvm_elf", cc);
        }
        command
            .args(["build", "--release", "--target", TARGET, "--target-dir"])
            .arg(&target_dir);
        let output = command.output()?;
        if !output.status.success() {
            return Err(Error::Build(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }

        let mut binaries = Vec::new();
        for name in self.binary_names()? {
            let elf = fs::read(target_dir.join(TARGET).join("release").join(&name))?;
            binaries.push(GuestBinary::package(&name, elf)?);
        }
        binaries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(BuildOutput {
            binaries,
            lockfile: fs::read(&lock_path)?,
        })
    }

    /// The binary targets of the stage workspace's members, as cargo sees them.
    fn binary_names(&self) -> Result<Vec<String>> {
        let metadata = MetadataCommand::new()
            .cargo_path(self.toolchain.cargo())
            .manifest_path(self.stage.root().join("Cargo.toml"))
            .no_deps()
            .env("CARGO_HOME", self.cargo_home)
            .exec()
            .map_err(|e| Error::Metadata(format!("stage: {e}")))?;
        let names: Vec<String> = metadata
            .workspace_packages()
            .iter()
            .flat_map(|p| p.targets.iter())
            .filter(|t| t.kind.contains(&TargetKind::Bin))
            .map(|t| t.name.clone())
            .collect();
        if names.is_empty() {
            return Err(Error::Build("the guest has no binary target".into()));
        }
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> (StagePlan, PathBuf) {
        let checkout = PathBuf::from("/w/my-raster-checkout");
        (
            StagePlan::new(std::slice::from_ref(&checkout), Some(&checkout)).unwrap(),
            checkout,
        )
    }

    #[test]
    fn a_generated_guest_names_dependencies_by_stage_path() {
        let (plan, checkout) = plan();
        let manifest = stage_manifest(
            &plan,
            &GuestCrate::Generated {
                package: "raster-guest-greet".into(),
                dependencies: vec![
                    Dependency::version("risc0-zkvm", "3.0"),
                    Dependency::path("raster", &checkout.join("crates/raster"))
                        .no_default_features(),
                    Dependency::path("hello-tiles", &checkout.join("examples/hello-tiles"))
                        .no_default_features(),
                ],
                main_rs: String::new(),
            },
        )
        .unwrap();
        let parsed: toml::Table = manifest.parse().unwrap();
        let deps = parsed["dependencies"].as_table().unwrap();
        assert_eq!(deps["raster"]["path"].as_str(), Some("raster/crates/raster"));
        assert_eq!(deps["raster"]["default-features"].as_bool(), Some(false));
        assert_eq!(
            deps["hello-tiles"]["path"].as_str(),
            Some("raster/examples/hello-tiles")
        );
        assert_eq!(deps["risc0-zkvm"]["version"].as_str(), Some("3.0"));
        assert_eq!(
            parsed["workspace"]["exclude"].as_array().unwrap(),
            &vec![toml::Value::String("raster".into())]
        );
        assert_eq!(parsed["profile"]["release"]["lto"].as_bool(), Some(true));
        assert!(
            !manifest.contains("my-raster-checkout") && !manifest.contains("/w/"),
            "no real path in the manifest:\n{manifest}"
        );
    }

    #[test]
    fn a_package_guest_is_rehomed_with_stage_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let checkout = tmp.path().join("some-checkout");
        let guest = checkout.join("crates/raster-prover/guests/transition");
        fs::create_dir_all(guest.join("src")).unwrap();
        fs::create_dir_all(checkout.join("crates/raster-core")).unwrap();
        fs::write(
            guest.join("Cargo.toml"),
            "[package]\nname = \"transition-guest\"\nversion = \"0.1.0\"\n\n[workspace]\n\n\
             [dependencies]\n\
             raster-core = { path = \"../../../raster-core\", features = [\"std\"] }\n\
             risc0-zkvm = \"3.0\"\n\n\
             [target.'cfg(target_os = \"zkvm\")'.dependencies]\n\
             raster-core-again = { package = \"raster-core\", path = \"../../../raster-core\" }\n\n\
             [profile.release]\nopt-level = 3\nlto = true\n",
        )
        .unwrap();
        let plan = StagePlan::new(
            &[checkout.clone(), guest.clone()],
            Some(&checkout),
        )
        .unwrap();
        let manifest = stage_manifest(&plan, &GuestCrate::Package { dir: guest.clone() }).unwrap();
        let parsed: toml::Table = manifest.parse().unwrap();
        assert_eq!(parsed["package"]["name"].as_str(), Some("transition-guest"));
        assert_eq!(
            parsed["dependencies"]["raster-core"]["path"].as_str(),
            Some("raster/crates/raster-core")
        );
        assert_eq!(
            parsed["target"]["cfg(target_os = \"zkvm\")"]["dependencies"]["raster-core-again"]["path"]
                .as_str(),
            Some("raster/crates/raster-core")
        );
        assert_eq!(parsed["dependencies"]["risc0-zkvm"].as_str(), Some("3.0"));
        assert_eq!(parsed["profile"]["release"]["opt-level"].as_integer(), Some(3));
        assert_eq!(
            parsed["workspace"]["exclude"].as_array().unwrap(),
            &vec![toml::Value::String("raster".into())],
            "the guest's own [workspace] is replaced by the stage's"
        );
        assert!(!manifest.contains("some-checkout"), "{manifest}");
    }

    /// A relative target directory is resolved once, against this process's
    /// working directory, so cargo (run in the stage) writes where the ELF is
    /// read back.
    #[test]
    fn a_relative_target_dir_is_made_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        let toolchain_dir = tmp.path().join("v9.9.9-rust-test");
        fs::create_dir_all(toolchain_dir.join("bin")).unwrap();
        for tool in ["cargo", "rustc"] {
            fs::write(toolchain_dir.join("bin").join(format!("{tool}{}", env::consts::EXE_SUFFIX)), "").unwrap();
        }
        let toolchain = Toolchain::from_dir(&toolchain_dir).unwrap();
        let checkout = tmp.path().join("checkout");
        fs::create_dir_all(&checkout).unwrap();
        let plan = StagePlan::new(std::slice::from_ref(&checkout), Some(&checkout)).unwrap();
        let stage = Stage::create_in(plan, Some(tmp.path())).unwrap();
        let build = |target_dir| GuestBuild {
            stage: &stage,
            krate: GuestCrate::Generated {
                package: "g".into(),
                dependencies: Vec::new(),
                main_rs: String::new(),
            },
            lockfile: None,
            toolchain: &toolchain,
            cargo_home: tmp.path(),
            target_dir,
        };

        let relative = Path::new("target/raster/guest-target");
        let resolved = build(Some(relative)).target_dir().unwrap();
        assert!(resolved.is_absolute(), "{}", resolved.display());
        assert_eq!(resolved, env::current_dir().unwrap().join(relative));
        assert_eq!(build(None).target_dir().unwrap(), stage.root().join("target"));
    }

    #[cfg(unix)]
    #[test]
    fn preparing_a_package_guest_links_its_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let checkout = tmp.path().join("checkout");
        let guest = checkout.join("guests/g");
        fs::create_dir_all(guest.join("src")).unwrap();
        fs::create_dir_all(guest.join("target")).unwrap();
        fs::write(guest.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(guest.join("Cargo.lock"), "# lock").unwrap();
        fs::write(
            guest.join("Cargo.toml"),
            "[package]\nname = \"g\"\nversion = \"0.1.0\"\n[workspace]\n",
        )
        .unwrap();
        let plan = StagePlan::new(&[checkout.clone(), guest.clone()], Some(&checkout)).unwrap();
        let stage = Stage::create_in(plan, Some(tmp.path())).unwrap();
        prepare_stage(&stage, &GuestCrate::Package { dir: guest.clone() }, Some(&guest.join("Cargo.lock")))
            .unwrap();
        assert_eq!(fs::read_link(stage.root().join("src")).unwrap(), guest.join("src"));
        assert!(!stage.root().join("target").exists(), "build output is not linked");
        assert_eq!(fs::read_to_string(stage.root().join("Cargo.lock")).unwrap(), "# lock");
        assert!(fs::read_to_string(stage.root().join("Cargo.toml"))
            .unwrap()
            .contains("name = \"g\""));
    }
}
