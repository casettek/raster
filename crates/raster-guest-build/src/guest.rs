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
    /// An existing guest package in the real directory `dir`, built as the
    /// stage workspace's only member: a protocol guest. It must not declare a
    /// `[workspace]` of its own, and every workspace between it and the stage
    /// root must exclude it. Its `[profile.*]` tables are carried to the stage
    /// root, where cargo reads profiles.
    Member { dir: PathBuf },
}

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
        GuestCrate::Member { dir } => {
            let member_manifest = dir.join("Cargo.toml");
            let member: toml::Table = fs::read_to_string(&member_manifest)?
                .parse()
                .map_err(|e| Error::Layout(format!("{}: {e}", member_manifest.display())))?;
            if member.contains_key("workspace") {
                return Err(Error::Layout(format!(
                    "{} declares its own [workspace]; a staged guest must be a member of the \
                     stage workspace (docs/proposals/path-independent-builds.md §4.4)",
                    member_manifest.display()
                )));
            }
            if let Some(profile) = member.get("profile") {
                manifest.insert("profile".into(), profile.clone());
            }
            workspace.insert(
                "members".into(),
                vec![toml::Value::String(path_string(&plan.stage_relative(dir)?)?)].into(),
            );
        }
    }
    workspace.insert("exclude".into(), entries);
    manifest.insert("workspace".into(), workspace.into());
    toml::to_string(&manifest).map_err(|e| Error::Layout(format!("stage manifest: {e}")))
}

/// Forward slashes on every platform: cargo accepts them, and the manifest,
/// which a fingerprint may hash, stays the same everywhere.
fn path_string(path: &Path) -> Result<String> {
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
    /// image id, so guests may share one.
    pub target_dir: Option<&'a Path>,
}

/// Write `krate` into `stage`: its manifest, a generated crate's `main.rs`,
/// and `lockfile` as the stage's `Cargo.lock`. [`GuestBuild::run`] does this
/// first; a caller can also do it alone to resolve the staged guest's
/// dependencies with [`crate::PathPackages::resolve_for_target`].
pub fn prepare_stage(stage: &Stage, krate: &GuestCrate, lockfile: Option<&Path>) -> Result<()> {
    let root = stage.root();
    fs::write(root.join("Cargo.toml"), stage_manifest(stage.plan(), krate)?)?;
    if let GuestCrate::Generated { main_rs, .. } = krate {
        fs::create_dir_all(root.join("src"))?;
        fs::write(root.join("src").join("main.rs"), main_rs)?;
    }
    if let Some(lockfile) = lockfile {
        fs::copy(lockfile, root.join("Cargo.lock"))?;
    }
    Ok(())
}

impl GuestBuild<'_> {
    pub fn run(&self) -> Result<BuildOutput> {
        let root = self.stage.root();
        prepare_stage(self.stage, &self.krate, self.lockfile)?;
        let lock_path = root.join("Cargo.lock");
        let target_dir = self
            .target_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.join("target"));

        let mut command = Command::new(self.toolchain.cargo());
        // Called from a build script, the outer cargo's CARGO_* variables (its
        // target dir, manifest dir, encoded rustflags...) must not leak in.
        for (key, _) in env::vars_os() {
            if key.to_string_lossy().starts_with("CARGO") {
                command.env_remove(&key);
            }
        }
        command
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
    fn a_member_guest_is_listed_and_keeps_its_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let checkout = tmp.path().join("checkout");
        let guest = checkout.join("crates/raster-prover/guests/transition");
        fs::create_dir_all(&guest).unwrap();
        fs::write(
            guest.join("Cargo.toml"),
            "[package]\nname = \"transition-guest\"\nversion = \"0.1.0\"\n\n\
             [profile.release]\nopt-level = 3\nlto = true\n",
        )
        .unwrap();
        let plan = StagePlan::new(std::slice::from_ref(&checkout), Some(&checkout)).unwrap();
        let manifest = stage_manifest(&plan, &GuestCrate::Member { dir: guest.clone() }).unwrap();
        let parsed: toml::Table = manifest.parse().unwrap();
        assert_eq!(
            parsed["workspace"]["members"].as_array().unwrap(),
            &vec![toml::Value::String(
                "raster/crates/raster-prover/guests/transition".into()
            )]
        );
        assert_eq!(parsed["profile"]["release"]["opt-level"].as_integer(), Some(3));
        assert!(!parsed.contains_key("package"));

        // A guest with its own [workspace] cannot be a member.
        fs::write(
            guest.join("Cargo.toml"),
            "[package]\nname = \"transition-guest\"\nversion = \"0.1.0\"\n[workspace]\n",
        )
        .unwrap();
        let error = stage_manifest(&plan, &GuestCrate::Member { dir: guest })
            .unwrap_err()
            .to_string();
        assert!(error.contains("own [workspace]"), "{error}");
    }
}
